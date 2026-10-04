"""Workflow guardrails the runner-policy gate enforces beyond `runs-on`.

Two rules, both about the self-hosted pool staying usable:

1. **Every job carries `timeout-minutes`.** GitHub's default is 360 minutes,
   which is how a Playwright job whose Vite server never exited held a runner
   for 90+ minutes after its tests had passed, and how a wedged `cargo test`
   sat for 36. A job that calls a reusable workflow (`uses:` at job level) is
   exempt: GitHub rejects `timeout-minutes` there, and the called workflow's
   own jobs are checked where they are defined.
2. **Every required status check has a producer.** A context required on
   `main` that no workflow job produces on `pull_request` (or `merge_group`)
   is a check that can never report, so every pull request blocks on it —
   which is what the `dependency review` context did on 2026-09-10. The
   required list is read from `SECURITY.md`, the place it is documented for
   the operator who configures the ruleset.

Standard library only, so the gate needs no environment beyond `python3`:
the workflows are parsed line by line, which is enough for the keys this reads
(job ids, `name`, `timeout-minutes`, `uses`, a static `strategy.matrix`, and
the top-level `on`) and keeps PyYAML out of a job that runs on every PR.
"""

from __future__ import annotations

import argparse
import itertools
import re
import sys
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from pathlib import Path

#: Events on which a producing job satisfies a required check for a PR.
PR_EVENTS = frozenset({"pull_request", "merge_group"})

#: A literal timeout above GitHub's own ceiling is no guardrail at all.
MAX_TIMEOUT_MINUTES = 360

_KEY = re.compile(
    r"^(?P<indent> *)(?P<key>[\"']?[A-Za-z0-9_.-]+[\"']?):(?:\s+(?P<value>.*))?$"
)
_LIST_ITEM = re.compile(r"^(?P<indent> *)-\s+(?P<value>.*)$")
_MATRIX_REF = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")
_REQUIRED_MARKER = "required checks:"
_BULLET = re.compile(r"^-\s+`(?P<name>[^`]+)`\s*$")


@dataclass
class Job:
    """What the gate needs to know about one job."""

    job_id: str
    line: int
    name: str | None = None
    timeout: str | None = None
    uses: str | None = None
    #: Static matrix axes; `None` for an axis whose values are an expression.
    matrix: dict[str, list[str] | None] = field(default_factory=dict)


@dataclass
class Workflow:
    path: str
    events: set[str]
    jobs: list[Job]


@dataclass(frozen=True)
class Problem:
    path: str
    line: int
    message: str

    def annotation(self) -> str:
        return f"::error file={self.path},line={self.line}::{self.message}"


def _scalar(value: str | None) -> str:
    text = (value or "").strip()
    # A trailing comment is only a comment after whitespace (`a#b` is a value).
    text = re.sub(r"\s+#.*$", "", text)
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'":
        text = text[1:-1]
    return text


def _inline_list(value: str) -> list[str] | None:
    text = _scalar(value)
    if not (text.startswith("[") and text.endswith("]")):
        return None
    inner = text[1:-1].strip()
    return [_scalar(part) for part in inner.split(",")] if inner else []


def _meaningful(lines: Sequence[str]) -> list[tuple[int, int, str]]:
    """(line number, indent, text) for every non-blank, non-comment line."""
    out = []
    for number, raw in enumerate(lines, start=1):
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        out.append((number, len(raw) - len(raw.lstrip(" ")), raw.rstrip()))
    return out


def _block(
    rows: list[tuple[int, int, str]], start: int, parent_indent: int
) -> list[tuple[int, int, str]]:
    """The rows nested under the key at `rows[start]`."""
    block = []
    for row in rows[start + 1 :]:
        if row[1] <= parent_indent:
            break
        block.append(row)
    return block


def _children(
    block: list[tuple[int, int, str]],
) -> list[tuple[int, str, str, list[tuple[int, int, str]]]]:
    """Direct `key: value` children of a block: (line, key, value, sub-block)."""
    if not block:
        return []
    indent = block[0][1]
    out = []
    for index, (number, row_indent, text) in enumerate(block):
        if row_indent != indent:
            continue
        match = _KEY.match(text)
        if match:
            out.append(
                (
                    number,
                    _scalar(match["key"]),
                    match["value"] or "",
                    _block(block, index, indent),
                )
            )
    return out


def _list_values(value: str, block: list[tuple[int, int, str]]) -> list[str] | None:
    """A sequence written inline (`[a, b]`) or as a block (`- a`)."""
    inline = _inline_list(value)
    if inline is not None:
        return inline
    if _scalar(value):
        return None  # an expression or a scalar, not a static list
    items = []
    for _number, _indent, text in block:
        match = _LIST_ITEM.match(text)
        if match and _indent == block[0][1]:
            items.append(_scalar(match["value"]))
    return items


def _events(value: str, block: list[tuple[int, int, str]]) -> set[str]:
    text = _scalar(value)
    if text:
        inline = _inline_list(text)
        return set(inline) if inline is not None else {text}
    items = _list_values("", block)
    keys = {key for _n, key, _v, _b in _children(block)}
    return keys | set(items or [])


def parse_workflow(path: str, text: str) -> Workflow:
    rows = _meaningful(text.splitlines())
    top = [(i, row) for i, row in enumerate(rows) if row[1] == 0]
    events: set[str] = set()
    jobs: list[Job] = []
    for index, (_number, _indent, line) in top:
        match = _KEY.match(line)
        if not match:
            continue
        key = _scalar(match["key"])
        block = _block(rows, index, 0)
        # YAML 1.1 reads a bare `on` as boolean true; GitHub does not, but
        # some writers quote it — accept all three spellings.
        if key in {"on", "true"}:
            events = _events(match["value"] or "", block)
        elif key == "jobs":
            for number, job_id, _value, job_block in _children(block):
                job = Job(job_id=job_id, line=number)
                for _n, prop, prop_value, prop_block in _children(job_block):
                    if prop == "name":
                        job.name = _scalar(prop_value)
                    elif prop == "timeout-minutes":
                        job.timeout = _scalar(prop_value)
                    elif prop == "uses":
                        job.uses = _scalar(prop_value)
                    elif prop == "strategy":
                        job.matrix = _matrix(prop_block)
                jobs.append(job)
    return Workflow(path=path, events=events, jobs=jobs)


def _matrix(strategy: list[tuple[int, int, str]]) -> dict[str, list[str] | None]:
    for _n, key, value, block in _children(strategy):
        if key != "matrix":
            continue
        if _scalar(value):
            return {}  # a whole-matrix expression: nothing static to expand
        return {
            axis: _list_values(axis_value, axis_block)
            for _n2, axis, axis_value, axis_block in _children(block)
            if axis not in {"include", "exclude"}
        }
    return {}


def check_contexts(job: Job) -> set[str]:
    """The status-check names a job reports, as far as is statically known.

    An unnamed matrix job reports `id (v1, v2)`; a named one renders its
    `${{ matrix.* }}` references per combination. A name that still carries
    an expression after that cannot be known here and is returned as written,
    so it simply never matches a required context.
    """
    axes = {axis: values for axis, values in job.matrix.items() if values is not None}
    template = job.name
    if not axes:
        return {template or job.job_id}
    names = list(axes)
    out = set()
    for combo in itertools.product(*(axes[n] for n in names)):
        values = dict(zip(names, combo, strict=True))
        if template is None:
            out.add(f"{job.job_id} ({', '.join(combo)})")
        else:
            out.add(_render(template, values))
    return out


def _render(template: str, values: dict[str, str]) -> str:
    return _MATRIX_REF.sub(lambda m: values.get(m[1], m[0]), template)


def timeout_problems(workflow: Workflow) -> list[Problem]:
    problems = []
    for job in workflow.jobs:
        if job.uses is not None:
            continue
        if job.timeout is None:
            problems.append(
                Problem(
                    workflow.path,
                    job.line,
                    f"job '{job.job_id}' has no timeout-minutes; the 360-minute "
                    "default lets a wedged job hold a self-hosted runner for hours",
                )
            )
            continue
        if job.timeout.isdigit():
            minutes = int(job.timeout)
            if not 0 < minutes <= MAX_TIMEOUT_MINUTES:
                problems.append(
                    Problem(
                        workflow.path,
                        job.line,
                        f"job '{job.job_id}' timeout-minutes {minutes} is outside "
                        f"1..{MAX_TIMEOUT_MINUTES}",
                    )
                )
    return problems


def producer_problems(
    workflows: Iterable[Workflow], required: Iterable[str], source: str
) -> list[Problem]:
    produced: dict[str, list[str]] = {}
    for workflow in workflows:
        for job in workflow.jobs:
            if job.uses is not None:
                continue  # reports as `caller / callee`, never a bare context
            for context in check_contexts(job):
                produced.setdefault(context, []).append(
                    workflow.path if workflow.events & PR_EVENTS else ""
                )
    problems = []
    for context in required:
        producers = produced.get(context, [])
        if not producers:
            message = f"required check '{context}' is produced by no workflow job"
        elif not any(producers):
            message = (
                f"required check '{context}' is produced only by workflows that "
                "do not run on pull_request or merge_group"
            )
        else:
            continue
        problems.append(
            Problem(source, 1, message + "; every pull request would block on it")
        )
    return problems


def required_checks(security_md: str) -> list[str]:
    """The bullet list that follows the 'required checks:' sentence."""
    lines = security_md.splitlines()
    for index, line in enumerate(lines):
        if _REQUIRED_MARKER in line:
            out: list[str] = []
            for following in lines[index + 1 :]:
                stripped = following.strip()
                if not stripped and not out:
                    continue
                match = _BULLET.match(stripped)
                if not match:
                    break
                out.append(match["name"])
            return out
    return []


def run(root: Path) -> list[Problem]:
    workflow_dir = root / ".github" / "workflows"
    paths = sorted([*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")])
    workflows = [
        parse_workflow(str(p.relative_to(root)), p.read_text(encoding="utf-8"))
        for p in paths
    ]
    problems = [p for w in workflows for p in timeout_problems(w)]
    security = root / "SECURITY.md"
    required = required_checks(security.read_text(encoding="utf-8"))
    if not required:
        problems.append(
            Problem(
                "SECURITY.md",
                1,
                "no 'required checks:' bullet list found; the producer lint has "
                "nothing to check against",
            )
        )
    problems += producer_problems(workflows, required, "SECURITY.md")
    return problems


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    problems = run(args.root)
    for problem in problems:
        print(problem.annotation())
    if not problems:
        print(
            "workflow policy: every job times out; every required check has a producer"
        )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
