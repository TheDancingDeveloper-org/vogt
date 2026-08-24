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
    build_parser,
    build_report,
    percentile,
    plan_from_args,
    plan_from_scale,
    relation_triples,
    run_load,
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
