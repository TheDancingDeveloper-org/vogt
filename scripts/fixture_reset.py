#!/usr/bin/env python3
"""Rebuild the forge test fixture repository to the state the tests expect.

`TheDancingDeveloper-org/vogt-fixture` is a small, real GitHub repository that
the `live_forge` suite (`tests/test_forge_live.py`) and the live stack read
against, so that something exercises real pagination, PAT scopes, write-back
and check runs — none of which the fake-transport suite
(`tests/test_forge_provider.py`) can reach.

This script drives that repository to the known state declared in
``tests/fixtures/forge_fixture_manifest.json`` **through the API**. It is:

* **Idempotent.** Every step finds the object by a stable key (a label name, an
  issue title, a pull-request head branch) and creates it only when absent;
  a second run relabels and reopens/closes to match, and changes nothing else.
* **Additive with respect to history.** It closes, reopens, relabels and
  comments. It never issues a DELETE, never force-pushes, and never rewrites
  a commit — the same rule the product's own write-back holds (FR-B4).
* **Not wired to run now.** The repository must be created once, by a human
  (`gh repo create TheDancingDeveloper-org/vogt-fixture --private`), and a
  token supplied explicitly. `--help` and a `--dry-run` plan touch no network.

Usage::

    # A human, once, having created the empty repository and a PAT:
    VOGT_FIXTURE_TOKEN=ghp_xxx python scripts/fixture_reset.py
    python scripts/fixture_reset.py --repo owner/name --token-file ./pat
    python scripts/fixture_reset.py --dry-run      # prints the plan, no network

The token is read from ``--token-file``, then ``--token``, then
``$VOGT_FIXTURE_TOKEN``, then ``$GH_TOKEN``. It needs ``repo`` scope, and
``workflow`` scope for the failing-checks pull request's CI file.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

API_ROOT = "https://api.github.com"
USER_AGENT = "vogt-fixture-reset"
TIMEOUT_SECONDS = 30
DEFAULT_MANIFEST = (
    Path(__file__).resolve().parents[1]
    / "tests"
    / "fixtures"
    / "forge_fixture_manifest.json"
)


class FixtureError(RuntimeError):
    """The remote said no, or the manifest asks for the impossible."""


class Api:
    """The narrow slice of the GitHub REST API this rebuild needs.

    GET/POST/PATCH/PUT only — there is deliberately no ``delete`` method on
    this class, so the "never destroys history" rule is a property of the code
    and not merely of the caller's restraint. In ``dry_run`` every mutating
    call is logged and skipped, so a plan can be reviewed without a network.
    """

    def __init__(self, *, repo: str, token: str, dry_run: bool) -> None:
        self.repo = repo
        self._token = token
        self.dry_run = dry_run

    # -- transport ---------------------------------------------------------

    def _request(
        self, method: str, path: str, payload: dict[str, Any] | None = None
    ) -> tuple[int, Any]:
        url = path if path.startswith("http") else f"{API_ROOT}{path}"
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(url, data=data, method=method)
        request.add_header("Accept", "application/vnd.github+json")
        request.add_header("X-GitHub-Api-Version", "2022-11-28")
        request.add_header("User-Agent", USER_AGENT)
        request.add_header("Authorization", f"Bearer {self._token}")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                body = response.read()
                return int(response.status), _parse(body)
        except urllib.error.HTTPError as exc:
            return int(exc.code), _parse(exc.read())
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            msg = f"{method} {path} could not reach GitHub: {exc}"
            raise FixtureError(msg) from exc

    def get(self, path: str, **params: str | int) -> Any:
        query = urllib.parse.urlencode({k: str(v) for k, v in params.items()})
        full = path + (f"?{query}" if query else "")
        status, body = self._request("GET", full)
        if status == 404:
            return None
        if status >= 400:
            raise FixtureError(f"GET {full} -> {status}: {body}")
        return body

    def get_all(self, path: str, **params: str | int) -> list[dict[str, Any]]:
        """Walk every page of a list endpoint (the real pagination path)."""
        collected: list[dict[str, Any]] = []
        page = 1
        while True:
            batch = self.get(path, per_page=100, page=page, **params)
            if not isinstance(batch, list) or not batch:
                break
            collected.extend(item for item in batch if isinstance(item, dict))
            if len(batch) < 100:
                break
            page += 1
        return collected

    def write(self, method: str, path: str, payload: dict[str, Any]) -> Any:
        if self.dry_run:
            print(f"  would {method} {path} {json.dumps(payload)[:120]}")
            return {}
        status, body = self._request(method, path, payload)
        if status >= 400:
            raise FixtureError(f"{method} {path} -> {status}: {body}")
        return body

    # -- convenience -------------------------------------------------------

    def repo_path(self, suffix: str) -> str:
        return f"/repos/{self.repo}{suffix}"


def _parse(body: bytes) -> Any:
    text = body.decode("utf-8").strip()
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


# -- rebuild steps ---------------------------------------------------------


def ensure_labels(api: Api, manifest: dict[str, Any]) -> None:
    existing = {
        str(item["name"]): item
        for item in api.get_all(api.repo_path("/labels"))
        if item.get("name")
    }
    for label in manifest["labels"]:
        name = str(label["name"])
        body = {
            "name": name,
            "color": label["color"],
            "description": label.get("description", ""),
        }
        if name in existing:
            api.write("PATCH", api.repo_path(f"/labels/{_q(name)}"), body)
        else:
            api.write("POST", api.repo_path("/labels"), body)
    print(f"labels: {len(manifest['labels'])} ensured")


def ensure_milestones(api: Api, manifest: dict[str, Any]) -> dict[str, int]:
    existing = {
        str(item["title"]): int(item["number"])
        for item in api.get_all(api.repo_path("/milestones"), state="all")
        if item.get("title")
    }
    numbers: dict[str, int] = {}
    for milestone in manifest["milestones"]:
        title = str(milestone["title"])
        body = {
            "title": title,
            "state": milestone.get("state", "open"),
            "description": milestone.get("description", ""),
        }
        if title in existing:
            number = existing[title]
            api.write("PATCH", api.repo_path(f"/milestones/{number}"), body)
        else:
            created = api.write("POST", api.repo_path("/milestones"), body)
            number = int(created.get("number", 0)) if isinstance(created, dict) else 0
        numbers[title] = number
    print(f"milestones: {len(manifest['milestones'])} ensured")
    return numbers


def ensure_issues(
    api: Api, manifest: dict[str, Any], milestones: dict[str, int]
) -> None:
    by_title = {
        str(item["title"]): item
        for item in api.get_all(api.repo_path("/issues"), state="all")
        if item.get("title") and "pull_request" not in item
    }
    for issue in manifest["issues"]:
        title = str(issue["title"])
        milestone_title = issue.get("milestone")
        milestone_number = milestones.get(milestone_title) if milestone_title else None
        current = by_title.get(title)
        if current is None:
            body: dict[str, Any] = {
                "title": title,
                "body": issue.get("body", ""),
                "labels": issue.get("labels", []),
            }
            if milestone_number:
                body["milestone"] = milestone_number
            current = api.write("POST", api.repo_path("/issues"), body)
        number = _number_of(current)
        if number is None:
            continue
        # Relabel additively and set the milestone, idempotently.
        if issue.get("labels"):
            api.write(
                "POST",
                api.repo_path(f"/issues/{number}/labels"),
                {"labels": issue["labels"]},
            )
        patch: dict[str, Any] = {"state": issue.get("state", "open")}
        if milestone_number:
            patch["milestone"] = milestone_number
        api.write("PATCH", api.repo_path(f"/issues/{number}"), patch)
    print(f"issues: {len(manifest['issues'])} ensured")


def default_branch_sha(api: Api, manifest: dict[str, Any]) -> str:
    branch = manifest["default_branch"]
    ref = api.get(api.repo_path(f"/git/ref/heads/{branch}"))
    if not isinstance(ref, dict):
        raise FixtureError(
            f"default branch {branch!r} has no ref — create the repository and "
            "push an initial commit before running this script"
        )
    return str(ref["object"]["sha"])


def ensure_file(
    api: Api, path: str, contents: str, *, branch: str, message: str
) -> None:
    """Create or update one file on a branch (a commit), idempotently."""
    existing = api.get(api.repo_path(f"/contents/{path}"), ref=branch)
    body: dict[str, Any] = {
        "message": message,
        "content": base64.b64encode(contents.encode("utf-8")).decode("ascii"),
        "branch": branch,
    }
    if isinstance(existing, dict) and existing.get("sha"):
        if existing.get("content"):
            current = base64.b64decode(str(existing["content"])).decode("utf-8")
            if current == contents:
                return
        body["sha"] = str(existing["sha"])
    api.write("PUT", api.repo_path(f"/contents/{path}"), body)


def ensure_branch(api: Api, name: str, base_sha: str) -> None:
    existing = api.get(api.repo_path(f"/git/ref/heads/{_q(name)}"))
    if isinstance(existing, dict) and existing.get("object"):
        return
    api.write(
        "POST",
        api.repo_path("/git/refs"),
        {"ref": f"refs/heads/{name}", "sha": base_sha},
    )


def ensure_content_files(api: Api, manifest: dict[str, Any]) -> None:
    branch = manifest["default_branch"]
    for spec in manifest["markers"]["files"]:
        ensure_file(
            api,
            spec["path"],
            spec["contents"],
            branch=branch,
            message="fixture: TODO/FIXME markers",
        )
    dep = manifest["dependency_manifest"]
    ensure_file(
        api, dep["path"], dep["contents"], branch=branch, message="fixture: deps"
    )
    posture = manifest.get("posture")
    if posture:
        ensure_file(
            api,
            posture["path"],
            posture["contents"],
            branch=branch,
            message="fixture: posture",
        )
    print("content files: markers, dependency manifest, posture ensured")


def ensure_branches(api: Api, manifest: dict[str, Any], base_sha: str) -> None:
    for spec in manifest["branches"]:
        ensure_branch(api, str(spec["name"]), base_sha)
    print(f"branches: {len(manifest['branches'])} ensured (#283 pattern)")


def _open_pull_for(api: Api, head: str) -> dict[str, Any] | None:
    owner = api.repo.split("/", 1)[0]
    for item in api.get_all(
        api.repo_path("/pulls"), state="all", head=f"{owner}:{head}"
    ):
        return item
    return None


def ensure_pulls(api: Api, manifest: dict[str, Any]) -> None:
    base = manifest["default_branch"]
    for pull in manifest["pulls"]:
        head = str(pull["branch"])
        # A pull request needs a diff; give each head branch a distinguishing
        # file so the branch is genuinely ahead of base.
        ensure_file(
            api,
            f"fixtures/{pull['key']}.txt",
            f"Fixture pull request: {pull['title']}\n",
            branch=head,
            message=f"fixture: {pull['key']} change",
        )
        if pull.get("checks") == "failure":
            ensure_file(
                api,
                ".github/workflows/fixture-fail.yml",
                _FAILING_WORKFLOW,
                branch=head,
                message="fixture: a workflow that fails",
            )
        existing = _open_pull_for(api, head)
        if existing is None:
            existing = api.write(
                "POST",
                api.repo_path("/pulls"),
                {
                    "title": pull["title"],
                    "head": head,
                    "base": pull.get("base", base),
                    "body": pull.get("body", ""),
                    "draft": bool(pull.get("draft", False)),
                },
            )
        number = _number_of(existing)
        if number is None:
            continue
        if pull.get("merged") and not _is_merged(api, number):
            api.write(
                "PUT",
                api.repo_path(f"/pulls/{number}/merge"),
                {"merge_method": "squash"},
            )
    print(f"pulls: {len(manifest['pulls'])} ensured")


def ensure_writeback_probe(api: Api, manifest: dict[str, Any]) -> None:
    """Leave the write-back marker comment the live test looks for, once."""
    target = manifest.get("writeback_target")
    if not target:
        return
    number = int(target["issue_number"])
    marker = str(target["comment_marker"])
    for comment in api.get_all(api.repo_path(f"/issues/{number}/comments")):
        if marker in str(comment.get("body", "")):
            print("writeback probe: already present")
            return
    api.write(
        "POST",
        api.repo_path(f"/issues/{number}/comments"),
        {"body": f"{marker}\n\nSeed comment so the live write-back test has a target."},
    )
    print("writeback probe: seeded")


def _is_merged(api: Api, number: int) -> bool:
    status, _ = api._request("GET", api.repo_path(f"/pulls/{number}/merge"))
    return status == 204


_FAILING_WORKFLOW = """\
name: fixture-fail
on: [push, pull_request]
jobs:
  fail:
    runs-on: ubuntu-latest
    steps:
      - run: exit 1
"""


def _number_of(payload: Any) -> int | None:
    if isinstance(payload, dict) and isinstance(payload.get("number"), int):
        return int(payload["number"])
    return None


def _q(value: str) -> str:
    return urllib.parse.quote(value, safe="")


# -- entry point -----------------------------------------------------------


def resolve_token(args: argparse.Namespace) -> str:
    if args.token_file:
        token = Path(args.token_file).expanduser().read_text(encoding="utf-8").strip()
        if token:
            return token
    if args.token:
        return str(args.token)
    for env in ("VOGT_FIXTURE_TOKEN", "GH_TOKEN"):
        value = os.environ.get(env, "").strip()
        if value:
            return value
    raise FixtureError(
        "no token: pass --token-file/--token or set VOGT_FIXTURE_TOKEN / GH_TOKEN "
        "(needs 'repo', plus 'workflow' for the failing-checks pull request)"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="fixture_reset.py",
        description=(
            "Rebuild the vogt-fixture forge repository to the manifest's known "
            "state via the API. Idempotent; never deletes history. The "
            "repository must already exist (a human creates it once)."
        ),
    )
    parser.add_argument(
        "--repo",
        default=os.environ.get("VOGT_FIXTURE_REPO", ""),
        help="owner/name of the fixture repo (default: manifest's `repo`, or "
        "$VOGT_FIXTURE_REPO).",
    )
    parser.add_argument(
        "--manifest",
        default=str(DEFAULT_MANIFEST),
        help="path to the fixture manifest JSON.",
    )
    parser.add_argument("--token", default="", help="a PAT (prefer --token-file).")
    parser.add_argument(
        "--token-file", default="", help="path to a file holding the PAT."
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the plan and touch no network beyond nothing at all.",
    )
    return parser


def load_manifest(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise FixtureError(f"{path} is not a JSON object")
    return data


def print_plan(manifest: dict[str, Any]) -> None:
    exp = manifest.get("expected", {})
    print("plan (no network):")
    print(f"  labels:     {len(manifest['labels'])}")
    print(f"  milestones: {len(manifest['milestones'])}")
    print(
        f"  issues:     {len(manifest['issues'])} "
        f"({exp.get('issues_open')} open, {exp.get('issues_closed')} closed)"
    )
    draft = exp.get("pulls_draft")
    closes = exp.get("pulls_with_closes")
    print(
        f"  pulls:      {len(manifest['pulls'])} "
        f"({draft} draft, {closes} closes-linked)"
    )
    print(f"  branches:   {len(manifest['branches'])} (#283 pattern)")
    print("  content:    markers, dependency manifest, posture")


def run(args: argparse.Namespace) -> int:
    manifest = load_manifest(Path(args.manifest))
    repo = args.repo or manifest.get("repo")
    if not repo:
        raise FixtureError("no repo: pass --repo or set `repo` in the manifest")
    token = "dry-run" if args.dry_run else resolve_token(args)
    api = Api(repo=str(repo), token=token, dry_run=bool(args.dry_run))
    print(f"rebuilding {repo} ({'dry run' if args.dry_run else 'live'})")

    if args.dry_run:
        # A plan reads nothing and writes nothing; it summarises the manifest's
        # intent straight from the file, so `--dry-run` touches no network.
        print_plan(manifest)
        return 0

    ensure_labels(api, manifest)
    milestones = ensure_milestones(api, manifest)
    ensure_content_files(api, manifest)
    base_sha = default_branch_sha(api, manifest)
    ensure_branches(api, manifest, base_sha)
    ensure_pulls(api, manifest)
    ensure_issues(api, manifest, milestones)
    ensure_writeback_probe(api, manifest)
    print("done")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except FixtureError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
