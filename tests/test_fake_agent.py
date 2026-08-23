"""Contract tests for `scripts/fake-agent`, the synthetic agent CLI (#296).

The engine drives a real agent CLI in a PTY; the Rust integration suite proves
the edit+commit, findings and outcome scenarios end to end through that path.
These tests pin the script's *documented contract* directly — including the
idle/steer and stall scenarios that need PTY input the HTTP suite cannot
easily supply — so #289+ (gates/steer) and #283+ (git story) have a stable
stand-in to build against.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
FAKE_AGENT = REPO_ROOT / "scripts" / "fake-agent"


def run_fake_agent(
    scenario: str | None = None,
    *,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
    stdin: str | None = None,
    timeout: float = 15.0,
) -> subprocess.CompletedProcess[str]:
    """Invoke the fake-agent the way the engine would: argv[0] is the program,
    the prompt is delivered via the environment, scenario via env or argv."""
    argv = [str(FAKE_AGENT)]
    if scenario is not None:
        argv.append(scenario)
    full_env = os.environ.copy()
    if env:
        full_env.update(env)
    return subprocess.run(
        argv,
        env=full_env,
        cwd=str(cwd) if cwd else None,
        input=stdin,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def git(args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=str(cwd), capture_output=True, text=True, check=False
    )


def init_repo(path: Path) -> None:
    git(["init", "-q"], path)
    git(["config", "user.email", "seed@vogt.invalid"], path)
    git(["config", "user.name", "seed"], path)


def test_entry_script_exists_and_is_executable() -> None:
    assert FAKE_AGENT.exists()
    assert os.access(FAKE_AGENT, os.X_OK)


def test_print_contract_is_valid_json_naming_every_step() -> None:
    result = run_fake_agent("--print-contract")
    assert result.returncode == 0, result.stderr
    contract = json.loads(result.stdout)
    assert contract["scenario_default"] == "outcome"
    assert set(contract["steps"]) == {
        "edit",
        "commit",
        "findings",
        "idle",
        "outcome",
        "stall",
        "skip",
        "cost",
        "schema",
    }
    assert contract["notify_phrase_default"] == "VOGT_NOTIFY:"


def test_edit_commit_writes_a_trailered_checkpoint(tmp_path: Path) -> None:
    init_repo(tmp_path)
    result = run_fake_agent(
        "edit+commit",
        cwd=tmp_path,
        env={
            "MYDEVENV2_AGENT_TASK_ID": "task-42",
            "MYDEVENV2_AGENT_TASK_RUN_ID": "run-99",
        },
    )
    assert result.returncode == 0, result.stderr

    # The working tree got an edit.
    edited = tmp_path / "fake-agent-edit.txt"
    assert edited.exists()
    assert "run-99" in edited.read_text(encoding="utf-8")

    # The commit carries the checkpoint trailers keyed to task and run.
    body = git(["log", "-1", "--pretty=%B"], tmp_path).stdout
    assert "Vogt-Task: task-42" in body
    assert "Vogt-Run: run-99" in body
    run_trailer = git(
        ["log", "-1", "--pretty=%(trailers:key=Vogt-Run,valueonly)"], tmp_path
    ).stdout.strip()
    assert run_trailer == "run-99"


def test_findings_prints_notify_line_and_dumps_json(tmp_path: Path) -> None:
    dump = tmp_path / "findings.json"
    result = run_fake_agent(
        "findings",
        env={
            "FAKE_AGENT_NOTIFY_DELAY": "0",
            "FAKE_AGENT_NOTIFY_TEXT": "the sky is falling",
            "FAKE_AGENT_FINDINGS_FILE": str(dump),
            "MYDEVENV2_AGENT_TASK_ID": "t",
            "MYDEVENV2_AGENT_TASK_RUN_ID": "r",
        },
    )
    assert result.returncode == 0, result.stderr
    # The load-bearing half: the line the engine's phrase watcher parses.
    assert "VOGT_NOTIFY: the sky is falling" in result.stdout

    payload = json.loads(dump.read_text(encoding="utf-8"))
    assert payload["findings"][0]["text"] == "the sky is falling"
    assert payload["findings"][0]["source"] == "fake-agent"


def test_findings_honours_a_bespoke_notify_phrase() -> None:
    result = run_fake_agent(
        "findings",
        env={
            "FAKE_AGENT_NOTIFY_DELAY": "0",
            "FAKE_AGENT_NOTIFY_PHRASE": "ALERT:",
            "FAKE_AGENT_NOTIFY_TEXT": "custom",
        },
    )
    assert result.returncode == 0, result.stderr
    assert "ALERT: custom" in result.stdout


@pytest.mark.parametrize("code", [0, 3, 7])
def test_outcome_exits_with_the_chosen_code(code: int) -> None:
    result = run_fake_agent("outcome", env={"FAKE_AGENT_EXIT_CODE": str(code)})
    assert result.returncode == code


def test_idle_waits_for_a_steer_line_on_stdin() -> None:
    result = run_fake_agent("idle", stdin="continue please\n")
    assert result.returncode == 0, result.stderr
    assert "fake-agent idle>" in result.stdout
    assert "steered with 'continue please'" in result.stdout


def test_idle_continues_on_eof() -> None:
    result = run_fake_agent("idle", stdin="")
    assert result.returncode == 0
    assert "idle continued (eof)" in result.stdout


def test_stall_sleeps_then_returns() -> None:
    # A short, bounded stall stands in for the timeout path without a real hang.
    result = run_fake_agent("stall", env={"FAKE_AGENT_STALL_SECONDS": "0.2"})
    assert result.returncode == 0
    assert "stalling for 0.2s" in result.stdout


def test_stall_can_actually_hang_past_a_deadline() -> None:
    # The stall scenario really does block; a tight timeout must expire.
    with pytest.raises(subprocess.TimeoutExpired):
        run_fake_agent("stall", env={"FAKE_AGENT_STALL_SECONDS": "30"}, timeout=0.5)


def test_scenario_from_prompt_marker(tmp_path: Path) -> None:
    prompt = tmp_path / "prompt.md"
    prompt.write_text("# Task\n\nFAKE_AGENT_SCENARIO: outcome\n", encoding="utf-8")
    result = run_fake_agent(
        None,
        env={
            "MYDEVENV2_AGENT_TASK_PROMPT_FILE": str(prompt),
            "FAKE_AGENT_EXIT_CODE": "5",
        },
    )
    # The marker selected `outcome`; the exit-code knob then took effect.
    assert result.returncode == 5
    assert "scenario outcome" in result.stdout


def test_skip_prints_the_skip_sentinel_and_exits_clean() -> None:
    result = run_fake_agent("skip", env={"FAKE_AGENT_SKIP_REASON": "already current"})
    assert result.returncode == 0, result.stderr
    assert "VOGT_SKIP: already current" in result.stdout


def test_cost_prints_a_parseable_cost_line() -> None:
    result = run_fake_agent("cost", env={"FAKE_AGENT_COST": "$0.40"})
    assert result.returncode == 0, result.stderr
    assert "VOGT_COST: $0.40" in result.stdout


def test_schema_passes_first_try_emits_a_good_block_and_exits() -> None:
    result = run_fake_agent(
        "schema",
        env={"FAKE_AGENT_SCHEMA_DELAY": "0", "FAKE_AGENT_SCHEMA_PASS_ON": "1"},
    )
    assert result.returncode == 0, result.stderr
    assert "```json" in result.stdout
    assert '"risk": "low"' in result.stdout


def test_schema_without_reprompt_gives_up_on_eof() -> None:
    # PASS_ON high enough that the first block is wrong; with no re-prompt on
    # stdin the step stops cleanly on eof rather than spinning.
    result = run_fake_agent(
        "schema",
        env={"FAKE_AGENT_SCHEMA_DELAY": "0", "FAKE_AGENT_SCHEMA_PASS_ON": "9"},
        stdin="",
    )
    assert result.returncode == 0, result.stderr
    # The first (wrong) block omits the required `risk` field.
    assert '"summary": "did the thing"' in result.stdout


def test_unknown_step_is_refused() -> None:
    result = run_fake_agent("edit+teleport")
    assert result.returncode != 0
    assert "unknown scenario step" in result.stderr


def test_default_scenario_is_a_clean_exit() -> None:
    result = run_fake_agent(None)
    assert result.returncode == 0
    assert "scenario outcome" in result.stdout


if __name__ == "__main__":  # pragma: no cover - manual runs
    sys.exit(pytest.main([__file__, "-v"]))
