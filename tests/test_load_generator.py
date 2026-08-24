"""Tests for the registry-driven load generator, `scripts/load.py` (#297).

The generator's *timings* are not asserted — a wall-clock number on a shared
runner is not a fact a test can pin. What is asserted is its deterministic
logic: the percentile computation, the dataset-plan and report-shape builders,
and the self-free/duplicate-free relation generator. A tiny end-to-end run at
the smallest plan then proves the whole thing drives real operations through
the registry against a temp database and emits the documented JSON shape.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import load
from load import (
    DatasetPlan,
    LatencyRecorder,
    OperationStats,
    Regression,
    SoakOperationStats,
    SoakPlan,
    SoakRecorder,
    build_parser,
    build_report,
    build_soak_report,
    compare_to_baseline,
    current_rss_mb,
    percentile,
    plan_from_args,
    plan_from_scale,
    relation_triples,
    run_load,
    run_soak,
    soak_plan_from_scale,
)
from vogt.core.entities import DECLARABLE_RELATION_KINDS

# -- percentile ------------------------------------------------------------


def test_percentile_is_nearest_rank_on_a_sorted_copy() -> None:
    values = [50.0, 10.0, 30.0, 20.0, 40.0]  # unsorted on purpose
    assert percentile(values, 0.50) == 30.0
    assert percentile(values, 0.95) == 50.0
    assert percentile(values, 0.0) == 10.0
    assert percentile(values, 1.0) == 50.0


def test_percentile_of_a_single_sample_is_that_sample() -> None:
    assert percentile([7.5], 0.50) == 7.5
    assert percentile([7.5], 0.95) == 7.5


def test_percentile_rejects_an_empty_sample() -> None:
    with pytest.raises(ValueError, match="empty sample"):
        percentile([], 0.5)


def test_percentile_rejects_a_fraction_out_of_range() -> None:
    with pytest.raises(ValueError, match=r"\[0, 1\]"):
        percentile([1.0], 1.5)


# -- OperationStats --------------------------------------------------------


def test_operation_stats_rounds_for_the_report() -> None:
    stats = OperationStats(count=3, p50_ms=1.23456, p95_ms=9.87654)
    assert stats.to_dict() == {"count": 3, "p50_ms": 1.235, "p95_ms": 9.877}


# -- DatasetPlan / plan_from_scale -----------------------------------------


def test_total_work_items_is_projects_times_items() -> None:
    plan = DatasetPlan(
        projects=4, work_items_per_project=25, relations=3, ranking_iterations=2
    )
    assert plan.total_work_items == 100
    assert plan.to_dict()["total_work_items"] == 100


@pytest.mark.parametrize("bad", ["projects", "work_items_per_project"])
def test_dataset_plan_rejects_non_positive_counts(bad: str) -> None:
    kwargs = {
        "projects": 1,
        "work_items_per_project": 1,
        "relations": 0,
        "ranking_iterations": 1,
    }
    kwargs[bad] = 0
    with pytest.raises(ValueError, match=bad):
        DatasetPlan(**kwargs)


def test_dataset_plan_rejects_negative_relations() -> None:
    with pytest.raises(ValueError, match="relations"):
        DatasetPlan(
            projects=1, work_items_per_project=1, relations=-1, ranking_iterations=1
        )


def test_plan_from_scale_grows_with_the_knob() -> None:
    small = plan_from_scale(1)
    big = plan_from_scale(10)
    assert small.projects == 5
    assert small.total_work_items == 100
    assert big.projects == 50
    assert big.total_work_items == 1000


def test_plan_from_scale_rejects_scale_below_one() -> None:
    with pytest.raises(ValueError, match="scale"):
        plan_from_scale(0)


# -- relation_triples ------------------------------------------------------


def test_relation_triples_are_self_free_and_duplicate_free() -> None:
    refs = [f"WI-{i}" for i in range(10)]
    triples = relation_triples(refs, 25, seed=1)
    assert len(triples) == 25
    assert len(set(triples)) == 25, "no duplicate (source, target, kind)"
    for source, target, kind in triples:
        assert source != target
        assert kind in DECLARABLE_RELATION_KINDS


def test_relation_triples_are_deterministic_for_a_seed() -> None:
    refs = [f"WI-{i}" for i in range(8)]
    assert relation_triples(refs, 15, seed=42) == relation_triples(refs, 15, seed=42)


def test_relation_triples_zero_count_is_empty() -> None:
    assert relation_triples(["WI-1", "WI-2"], 0, seed=0) == []


def test_relation_triples_needs_two_items() -> None:
    with pytest.raises(ValueError, match="two work items"):
        relation_triples(["WI-1"], 1, seed=0)


# -- LatencyRecorder -------------------------------------------------------


def test_recorder_collects_samples_and_computes_stats() -> None:
    recorder = LatencyRecorder()
    for sample in (10.0, 20.0, 30.0):
        recorder.record("op", sample)
    stats = recorder.stats()["op"]
    assert stats.count == 3
    assert stats.p50_ms == 20.0
    assert stats.p95_ms == 30.0


def test_recorder_time_records_and_returns_the_result() -> None:
    recorder = LatencyRecorder()
    result = recorder.time("op", lambda: 99)
    assert result == 99
    assert recorder.stats()["op"].count == 1


# -- build_report ----------------------------------------------------------


def test_build_report_has_the_documented_shape() -> None:
    from datetime import UTC, datetime

    plan = DatasetPlan(
        projects=2, work_items_per_project=3, relations=1, ranking_iterations=1
    )
    stats = {"backlog": OperationStats(count=2, p50_ms=1.0, p95_ms=2.0)}
    report = build_report(
        plan=plan,
        stats=stats,
        rss_mb=42.5,
        seed=7,
        duration_s=1.234,
        generated_at=datetime(2026, 8, 24, tzinfo=UTC),
    )
    assert report["schema_version"] == load.REPORT_SCHEMA_VERSION
    assert report["seed"] == 7
    assert report["rss_mb"] == 42.5
    assert report["plan"]["total_work_items"] == 6
    assert report["operations"]["backlog"] == {
        "count": 2,
        "p50_ms": 1.0,
        "p95_ms": 2.0,
    }
    # Must be JSON-serialisable as-is.
    json.dumps(report)


# -- plan_from_args --------------------------------------------------------


def test_plan_from_args_uses_scale_when_no_overrides() -> None:
    args = build_parser().parse_args(["--scale", "2"])
    plan = plan_from_args(args)
    assert plan == plan_from_scale(2)


def test_plan_from_args_lets_explicit_flags_override_scale() -> None:
    args = build_parser().parse_args(
        ["--scale", "10", "--projects", "3", "--relations", "1"]
    )
    plan = plan_from_args(args)
    assert plan.projects == 3
    assert plan.relations == 1
    # Untouched fields still come from the scale.
    assert plan.work_items_per_project == plan_from_scale(10).work_items_per_project


# -- end to end ------------------------------------------------------------


def test_run_load_drives_real_operations_and_emits_the_shape(tmp_path: Path) -> None:
    plan = DatasetPlan(
        projects=1, work_items_per_project=2, relations=1, ranking_iterations=1
    )
    report = run_load(plan, data_dir=tmp_path / "instance", seed=0)

    assert report["plan"]["total_work_items"] == 2
    assert report["rss_mb"] > 0
    ops = report["operations"]
    for name in ("project.register", "work.create", "work.relate", "backlog", "sweep"):
        assert name in ops, f"{name} was not exercised"
        assert ops[name]["count"] >= 1
        assert ops[name]["p50_ms"] >= 0
    assert ops["work.create"]["count"] == 2


# -- soak: SoakPlan --------------------------------------------------------


def test_soak_plan_counts_its_sweeps() -> None:
    base = DatasetPlan(
        projects=1, work_items_per_project=1, relations=0, ranking_iterations=1
    )
    plan = SoakPlan(base=base, iterations=100, sweep_every=25)
    assert plan.sweeps == 4
    assert plan.to_dict()["sweeps"] == 4
    assert plan.to_dict()["base"]["total_work_items"] == 1


@pytest.mark.parametrize(("iterations", "sweep_every"), [(0, 1), (1, 0)])
def test_soak_plan_rejects_non_positive_knobs(
    iterations: int, sweep_every: int
) -> None:
    base = DatasetPlan(
        projects=1, work_items_per_project=1, relations=0, ranking_iterations=1
    )
    with pytest.raises(ValueError, match="must be >= 1"):
        SoakPlan(base=base, iterations=iterations, sweep_every=sweep_every)


def test_soak_plan_from_scale_trims_seed_ranking() -> None:
    plan = soak_plan_from_scale(2, iterations=50, sweep_every=10)
    assert plan.iterations == 50
    assert plan.sweep_every == 10
    # The base reuses plan_from_scale's sizing but not its 20 warm-up rankings.
    assert plan.base.projects == plan_from_scale(2).projects
    assert plan.base.ranking_iterations == 1


# -- soak: SoakOperationStats / SoakRecorder -------------------------------


def test_soak_operation_stats_rounds_and_carries_errors() -> None:
    stats = SoakOperationStats(
        count=3, errors=2, p50_ms=1.2345, p95_ms=9.8765, p99_ms=12.3456
    )
    assert stats.to_dict() == {
        "count": 3,
        "errors": 2,
        "p50_ms": 1.234,
        "p95_ms": 9.877,
        "p99_ms": 12.346,
    }


def test_soak_recorder_guard_times_success_and_counts_failure() -> None:
    recorder = SoakRecorder()
    recorder.guard("op", lambda: None)

    def _boom() -> None:
        raise RuntimeError("under load")

    recorder.guard("op", _boom)

    assert recorder.successful_calls == 1
    assert recorder.total_errors == 1
    stats = recorder.stats()["op"]
    assert stats.count == 1
    assert stats.errors == 1


def test_soak_recorder_computes_p99() -> None:
    recorder = SoakRecorder()
    for sample in range(1, 101):  # 1..100 ms
        recorder.record("op", float(sample))
    stats = recorder.stats()["op"]
    assert stats.p50_ms == 50.0
    assert stats.p95_ms == 95.0
    assert stats.p99_ms == 99.0


def test_current_rss_is_positive() -> None:
    assert current_rss_mb() > 0


# -- soak: build_soak_report -----------------------------------------------


def test_build_soak_report_computes_throughput_and_error_rate() -> None:
    from datetime import UTC, datetime

    base = DatasetPlan(
        projects=1, work_items_per_project=2, relations=0, ranking_iterations=1
    )
    plan = SoakPlan(base=base, iterations=10, sweep_every=5)
    stats = {"backlog": SoakOperationStats(2, 0, 1.0, 2.0, 3.0)}
    report = build_soak_report(
        plan=plan,
        stats=stats,
        successful_calls=90,
        total_errors=10,
        rss_start_mb=50.0,
        rss_end_mb=55.0,
        rss_peak_mb=52.0,
        seed=1,
        duration_s=2.0,
        produced_by="in-process",
        generated_at=datetime(2026, 8, 24, tzinfo=UTC),
    )
    assert report["kind"] == "soak"
    assert report["schema_version"] == load.SOAK_REPORT_SCHEMA_VERSION
    assert report["produced_by"] == "in-process"
    assert report["throughput_ops_per_s"] == 45.0  # 90 / 2s
    assert report["error_rate"] == 0.1  # 10 / (90 + 10)
    assert report["total_calls"] == 100
    assert report["rss_mb"]["growth"] == 5.0
    # Peak is clamped to at least the observed live samples.
    assert report["rss_mb"]["peak"] == 55.0
    json.dumps(report)


# -- soak: compare_to_baseline ---------------------------------------------


def _soak_report(*, p95: float, throughput: float) -> dict[str, object]:
    return {
        "throughput_ops_per_s": throughput,
        "operations": {"backlog": {"p95_ms": p95}},
    }


def test_compare_flags_a_latency_regression_past_the_threshold() -> None:
    baseline = _soak_report(p95=10.0, throughput=100.0)
    current = _soak_report(p95=25.0, throughput=100.0)  # 2.5x, past 2x
    regressions = compare_to_baseline(baseline, current)
    assert [r.metric for r in regressions] == ["backlog.p95_ms"]
    assert regressions[0].ratio == 2.5


def test_compare_flags_a_throughput_drop_past_the_threshold() -> None:
    baseline = _soak_report(p95=10.0, throughput=100.0)
    current = _soak_report(p95=10.0, throughput=40.0)  # fell to 0.4x, past 1/2x
    regressions = compare_to_baseline(baseline, current)
    assert [r.metric for r in regressions] == ["throughput_ops_per_s"]


def test_compare_is_quiet_within_the_threshold() -> None:
    baseline = _soak_report(p95=10.0, throughput=100.0)
    current = _soak_report(p95=18.0, throughput=60.0)  # 1.8x / 1.67x, both under 2x
    assert compare_to_baseline(baseline, current) == []


def test_compare_skips_operations_absent_from_either_report() -> None:
    baseline = {"operations": {"backlog": {"p95_ms": 10.0}}}
    current = {"operations": {"bugs": {"p95_ms": 999.0}}}  # no overlap
    assert compare_to_baseline(baseline, current) == []


def test_compare_rejects_a_non_positive_threshold() -> None:
    with pytest.raises(ValueError, match="threshold"):
        compare_to_baseline({}, {}, threshold=0)


def test_regression_describes_itself() -> None:
    text = Regression("backlog.p95_ms", 10.0, 25.0, 2.5).describe()
    assert "backlog.p95_ms" in text
    assert "2.5" in text


# -- soak: end to end ------------------------------------------------------


def test_run_soak_drives_the_steady_mix_and_records_numbers(tmp_path: Path) -> None:
    base = DatasetPlan(
        projects=1, work_items_per_project=2, relations=1, ranking_iterations=1
    )
    plan = SoakPlan(base=base, iterations=6, sweep_every=3)
    report = run_soak(plan, data_dir=tmp_path / "instance", seed=0)

    assert report["kind"] == "soak"
    assert report["error_rate"] == 0.0
    assert report["throughput_ops_per_s"] > 0
    ops = report["operations"]
    for name in ("work.create", "work.update", "work.get", "backlog", "bugs", "sweep"):
        assert name in ops, f"{name} was not exercised"
    # 6 iterations, one create each; sweep every 3 -> 2 sweeps.
    assert ops["work.create"]["count"] == 6
    assert ops["sweep"]["count"] == 2
    assert report["rss_mb"]["end"] > 0


def test_main_soak_mode_writes_and_checks_a_baseline(tmp_path: Path) -> None:
    baseline_path = tmp_path / "baseline.json"
    args = [
        "--mode",
        "soak",
        "--projects",
        "1",
        "--work-items-per-project",
        "2",
        "--relations",
        "0",
        "--iterations",
        "6",
        "--sweep-every",
        "3",
        "--data-dir",
        str(tmp_path / "data"),
        "--out",
        str(baseline_path),
    ]
    assert load.main(args) == 0
    written = json.loads(baseline_path.read_text(encoding="utf-8"))
    assert written["kind"] == "soak"

    # Checking a run against itself as the baseline is drift-free -> exit 0.
    check = [
        *args,
        "--data-dir",
        str(tmp_path / "data2"),
        "--check-baseline",
        str(baseline_path),
    ]
    assert load.main(check) == 0


def test_main_check_baseline_fails_on_drift(tmp_path: Path) -> None:
    # A baseline with an absurdly fast p95 forces a regression on any real run.
    baseline_path = tmp_path / "tiny.json"
    baseline_path.write_text(
        json.dumps(
            {
                "throughput_ops_per_s": 1_000_000.0,
                "operations": {"backlog": {"p95_ms": 0.0001}},
            }
        ),
        encoding="utf-8",
    )
    args = [
        "--mode",
        "soak",
        "--projects",
        "1",
        "--work-items-per-project",
        "2",
        "--relations",
        "0",
        "--iterations",
        "4",
        "--sweep-every",
        "2",
        "--data-dir",
        str(tmp_path / "data"),
        "--out",
        str(tmp_path / "out.json"),
        "--check-baseline",
        str(baseline_path),
    ]
    assert load.main(args) == 1


def test_main_writes_a_report_file(tmp_path: Path) -> None:
    out = tmp_path / "report.json"
    code = load.main(
        [
            "--projects",
            "1",
            "--work-items-per-project",
            "2",
            "--relations",
            "0",
            "--ranking-iterations",
            "1",
            "--out",
            str(out),
            "--data-dir",
            str(tmp_path / "data"),
        ]
    )
    assert code == 0
    written = json.loads(out.read_text(encoding="utf-8"))
    assert written["operations"]["work.create"]["count"] == 2
