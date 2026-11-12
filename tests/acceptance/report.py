"""Markdown and JSON reports, including the requirement-coverage table."""

from __future__ import annotations

import json
import tomllib
from dataclasses import asdict
from pathlib import Path
from typing import Any

from .harness import CaseResult, KnownIssue, RunSummary, redact

HERE = Path(__file__).resolve().parent


def load_matrix() -> list[dict[str, Any]]:
    document = tomllib.loads((HERE / "matrix.toml").read_text("utf-8"))
    return [row for row in document.get("requirement", []) if isinstance(row, dict)]


def load_known_issues() -> list[KnownIssue]:
    document = tomllib.loads((HERE / "known_issues.toml").read_text("utf-8"))
    issues: list[KnownIssue] = []
    for row in document.get("known_issue", []):
        if not isinstance(row, dict):
            continue
        issues.append(
            KnownIssue(
                test=str(row.get("test", "")),
                ref=str(row.get("ref", "")),
                reason=str(row.get("reason", "")),
                match=row.get("match"),
            )
        )
    return issues


def _verdict_for(rows: list[CaseResult]) -> str:
    if not rows:
        return "untested"
    verdicts = {row.verdict for row in rows}
    if "FAIL" in verdicts:
        return "failing"
    if "PASS" in verdicts:
        return "partial" if verdicts - {"PASS", "FAIL-known"} else "covered"
    return "not testable"


def coverage(summary: RunSummary, matrix: list[dict[str, Any]]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for requirement in matrix:
        rid = str(requirement.get("id"))
        cases = [result for result in summary.results if result.case.requirement == rid]
        rows.append(
            {
                "id": rid,
                "title": str(requirement.get("title", "")),
                "matrix_status": str(requirement.get("status", "")),
                "items": str(requirement.get("items", "")),
                "cases": [result.case.id for result in cases],
                "verdict": _verdict_for(cases),
                "results": [
                    {
                        "id": result.case.id,
                        "verdict": result.verdict,
                        "evidence": result.check.evidence,
                    }
                    for result in cases
                ],
            }
        )
    attributed = {str(row["id"]) for row in rows}
    for result in summary.results:
        if result.case.requirement not in attributed:
            rows.append(
                {
                    "id": result.case.requirement,
                    "title": "(suite-local requirement, not a matrix row)",
                    "matrix_status": "",
                    "items": "",
                    "cases": [result.case.id],
                    "verdict": _verdict_for([result]),
                    "results": [
                        {
                            "id": result.case.id,
                            "verdict": result.verdict,
                            "evidence": result.check.evidence,
                        }
                    ],
                }
            )
            attributed.add(result.case.requirement)
    return rows


def _clean(text: object, secrets: tuple[str, ...]) -> str:
    return redact(str(text), secrets=secrets)


def _counts(summary: RunSummary) -> dict[str, int]:
    return {
        "PASS": summary.count("PASS"),
        "FAIL": summary.count("FAIL"),
        "FAIL-known": summary.count("FAIL-known"),
        "SKIP": summary.count("SKIP"),
    }


def markdown(
    summary: RunSummary, matrix: list[dict[str, Any]], secrets: tuple[str, ...] = ()
) -> str:
    counts = _counts(summary)
    duration = (summary.finished_at - summary.started_at).total_seconds()
    lines: list[str] = [
        "# Vogt acceptance report",
        "",
        f"- Target: `{_clean(summary.target, secrets)}`",
        f"- Mode: `{summary.mode}`"
        + (" (in-process fake — not live evidence)" if summary.fake else ""),
        f"- Run tag: `{summary.tag}`",
        f"- Started: {summary.started_at.isoformat()}  "
        f"Finished: {summary.finished_at.isoformat()} ({duration:.0f}s)",
        "- Token: read from the token file only; never printed, logged or "
        "stored in this report",
        "",
        "## Summary",
        "",
        "| PASS | FAIL | FAIL-known | SKIP |",
        "|---|---|---|---|",
        f"| {counts['PASS']} | {counts['FAIL']} | "
        f"{counts['FAIL-known']} | {counts['SKIP']} |",
        "",
    ]
    if summary.fake:
        lines.extend(
            [
                "> **Fake-server run.** Every result below was produced against the "
                "in-process fake. "
                "It proves the suite's plumbing (MCP framing, cleanup, redaction, "
                "reporting); it is "
                "not evidence about vogt-dev.",
                "",
            ]
        )
    lines.extend(["## Results", ""])
    current_group = ""
    for result in summary.results:
        if result.case.group != current_group:
            current_group = result.case.group
            lines.extend(
                [
                    f"### {current_group}",
                    "",
                    "| Test | Requirement | Severity | Verdict | Evidence |",
                    "|---|---|---|---|---|",
                ]
            )
        evidence = (
            _clean(result.check.evidence, secrets)
            .replace("|", "\\|")
            .replace("\n", " ")
        )
        lines.append(
            f"| `{result.case.id}` | {result.case.requirement} | "
            f"{result.case.severity} | "
            f"{result.verdict} | {evidence} |"
        )
    lines.append("")
    hits = [result for result in summary.results if result.known is not None]
    lines.extend(["## Known issues", ""])
    if hits:
        lines.extend(["| Test | Item | Why |", "|---|---|---|"])
        for result in hits:
            issue = result.known
            assert issue is not None
            lines.append(
                f"| `{result.case.id}` | {issue.ref} | "
                f"{_clean(issue.reason, secrets)} |"
            )
    else:
        lines.append("No known-issue exemption applied on this run.")
    lines.append("")
    lines.extend(
        [
            "## Requirement coverage",
            "",
            "| Requirement | Matrix | Verdict | Tests | Title |",
            "|---|---|---|---|---|",
        ]
    )
    for row in coverage(summary, matrix):
        cases = ", ".join(f"`{case}`" for case in row["cases"]) or "—"
        lines.append(
            f"| {row['id']} | {row['matrix_status']} | {row['verdict']} | {cases} | "
            f"{_clean(row['title'], secrets)} |"
        )
    lines.append("")
    untested = [
        row for row in coverage(summary, matrix) if row["verdict"] == "untested"
    ]
    not_testable = [
        row for row in coverage(summary, matrix) if row["verdict"] == "not testable"
    ]
    lines.extend(["## Not covered or not testable", ""])
    if untested:
        lines.append(
            "**No test yet:** " + ", ".join(f"`{row['id']}`" for row in untested)
        )
    if not_testable:
        lines.append(
            "**Every test skipped:** "
            + ", ".join(f"`{row['id']}`" for row in not_testable)
        )
    if not untested and not not_testable:
        lines.append("Every matrix row has at least one executed test.")
    lines.append("")
    return "\n".join(lines)


def as_json(
    summary: RunSummary, matrix: list[dict[str, Any]], secrets: tuple[str, ...] = ()
) -> dict[str, Any]:
    return {
        "suite": "vogt-acceptance",
        "target": _clean(summary.target, secrets),
        "mode": summary.mode,
        "fake": summary.fake,
        "run_tag": summary.tag,
        "started_at": summary.started_at.isoformat(),
        "finished_at": summary.finished_at.isoformat(),
        "counts": _counts(summary),
        "results": [
            {
                "id": result.case.id,
                "group": result.case.group,
                "requirement": result.case.requirement,
                "severity": result.case.severity,
                "title": result.case.title,
                "verdict": result.verdict,
                "evidence": _clean(result.check.evidence, secrets),
                "duration_ms": result.duration_ms,
                "known_issue": asdict(result.known)
                if result.known is not None
                else None,
            }
            for result in summary.results
        ],
        "coverage": [
            {
                **row,
                "title": _clean(row["title"], secrets),
                "results": [
                    {**entry, "evidence": _clean(entry["evidence"], secrets)}
                    for entry in row["results"]
                ],
            }
            for row in coverage(summary, matrix)
        ],
    }


def write_reports(
    summary: RunSummary,
    *,
    markdown_path: Path | None,
    json_path: Path | None,
    secrets: tuple[str, ...] = (),
) -> None:
    matrix = load_matrix()
    text = markdown(summary, matrix, secrets)
    if markdown_path is not None:
        markdown_path.parent.mkdir(parents=True, exist_ok=True)
        markdown_path.write_text(text + "\n", "utf-8")
    if json_path is not None:
        json_path.parent.mkdir(parents=True, exist_ok=True)
        json_path.write_text(
            json.dumps(as_json(summary, matrix, secrets), indent=2, sort_keys=False)
            + "\n",
            "utf-8",
        )
