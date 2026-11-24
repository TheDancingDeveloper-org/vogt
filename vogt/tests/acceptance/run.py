#!/usr/bin/env python3
"""Vogt live-instance acceptance suite.

    python3 tests/acceptance/run.py \
        --base-url https://vogt-dev.sprooty.com \
        --token-file ~/.config/vogt-a5/token \
        --report acceptance.md --json acceptance.json

Read-only by default. ``--allow-writes`` adds the throwaway-project work
checks; ``--with-sessions`` adds the pod-session checks. ``--fake`` runs the
whole suite against a local in-process fake, with no network and no credential,
so the suite's own plumbing can be verified anywhere.

The token is read from ``--token-file`` and nowhere else: not argv, not the
environment. It is never printed, and every report line is redacted before it
is written.
"""

from __future__ import annotations

import argparse
import sys
from contextlib import AbstractContextManager
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from tests.acceptance import harness, policy, report
from tests.acceptance.checks import all_cases
from tests.acceptance.client import HttpClient, McpClient
from tests.acceptance.fake_server import FakeServer


class _FakeRun(AbstractContextManager[FakeServer]):
    def __enter__(self) -> FakeServer:
        self._server = FakeServer()
        self._server.__enter__()
        return self._server

    def __exit__(self, *exc: object) -> None:
        self._server.__exit__(*exc)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="tests/acceptance/run.py",
        description="Read-only-by-default acceptance checks for a Vogt dev instance.",
    )
    parser.add_argument(
        "--base-url", help="target instance, e.g. https://vogt-dev.sprooty.com"
    )
    parser.add_argument(
        "--token-file",
        type=Path,
        help="file holding the bearer (0600). The only source of the token.",
    )
    parser.add_argument(
        "--read-token-file",
        type=Path,
        help="optional read-only token, to prove that a read credential cannot write",
    )
    parser.add_argument(
        "--with-sessions",
        action="store_true",
        help="start throwaway pod sessions (always stopped; see the docs)",
    )
    parser.add_argument(
        "--allow-writes",
        action="store_true",
        help="run the throwaway-project work-tracking checks",
    )
    parser.add_argument(
        "--allow-restart",
        action="store_true",
        help="allow the restart-durability check (it still needs an operator restart)",
    )
    parser.add_argument(
        "--expect-digest",
        help="expected engine image digest, e.g. sha256:... — enables H5",
    )
    parser.add_argument(
        "--kube-context",
        default="estate-ro",
        help="read-only kube context (default: estate-ro)",
    )
    parser.add_argument(
        "--timeout", type=float, default=30.0, help="per-request timeout in seconds"
    )
    parser.add_argument(
        "--only", default="", help="comma-separated groups or case-id prefixes"
    )
    parser.add_argument(
        "--report",
        type=Path,
        help="write a Markdown report (or JSON if the name ends .json)",
    )
    parser.add_argument(
        "--json",
        type=Path,
        dest="json_report",
        help="write the machine-readable report",
    )
    parser.add_argument(
        "--prod-instance-id", help="the production instance id to refuse (optional)"
    )
    parser.add_argument(
        "--fake",
        action="store_true",
        help="run against the in-process fake; no network or token",
    )
    parser.add_argument("--list", action="store_true", help="list the cases and exit")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate invocation and list cases; make no requests",
    )
    return parser


def _options(args: argparse.Namespace) -> harness.Options:
    only = tuple(part for part in str(args.only or "").split(",") if part.strip())
    return harness.Options(
        base_url=args.base_url,
        token_file=args.token_file,
        read_token_file=args.read_token_file,
        with_sessions=bool(args.with_sessions),
        allow_writes=bool(args.allow_writes),
        allow_restart=bool(args.allow_restart),
        expect_digest=args.expect_digest,
        kube_context=args.kube_context,
        timeout=args.timeout,
        fake=bool(args.fake),
        report=args.report,
        json_report=args.json_report,
        only=only,
        prod_instance_id=args.prod_instance_id,
        dry_run=bool(args.dry_run),
    )


def _preflight(token: harness.Token, options: harness.Options) -> str | None:
    """Check the credential and learn the instance id. Returns prod id if found."""
    http = HttpClient(options.base_url or "", token=token, timeout=options.timeout)
    try:
        response = http.request("GET", "/api/vogt/instance/diagnostics")
    except harness.TransportError as exc:
        raise policy.PolicyRefusal(f"cannot reach the target: {exc}") from None
    if response.status in (401, 403):
        raise policy.PolicyRefusal("the token file's credential was refused (401/403)")
    if response.status != 200:
        raise policy.PolicyRefusal(f"instance diagnostics answered {response.status}")
    payload = response.json()
    if not isinstance(payload, dict):
        raise policy.PolicyRefusal("instance diagnostics did not return an object")
    instance_id = payload.get("instance_id")
    prod_id = options.prod_instance_id
    if not prod_id:
        try:
            peer = http.request("GET", "/api/vogt/instance/diagnostics?peer=true")
            peer_body = peer.json() if peer.status == 200 else {}
            peer_diag = (
                peer_body.get("peer", {}).get("diagnostics")
                if isinstance(peer_body, dict)
                else None
            )
            if isinstance(peer_diag, dict):
                prod_id = peer_diag.get("instance_id") or prod_id
        except harness.TransportError:
            pass
    policy.validate_instance(
        instance_id=str(instance_id) if instance_id else None,
        prod_instance_id=prod_id,
    )
    return str(prod_id) if prod_id else None


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    options = _options(args)
    cases = harness.select(all_cases(), options.only)
    if args.list or args.dry_run:
        for case in cases:
            print(
                f"{case.id:<10} {case.group:<12} {case.requirement:<10} "
                f"{case.severity:<9} {case.title}"
            )
        if args.dry_run:
            try:
                if not options.fake:
                    _validated_target(options)
            except policy.PolicyRefusal as exc:
                print(f"refused: {exc}", file=sys.stderr)
                return 2
        return 0
    if not cases:
        print(f"no cases match --only {options.only!r}", file=sys.stderr)
        return 2

    if options.fake:
        with _FakeRun() as fake:
            options.base_url = fake.base_url
            token = fake.token
            if options.read_token_file is None:
                options.read_token = fake.read_token
            else:
                options.read_token = harness.Token(
                    policy.read_token_file(options.read_token_file)
                )
            return _run(options, token, fake=True)
    try:
        target = _validated_target(options)
        assert options.token_file is not None
        token = harness.Token(policy.read_token_file(options.token_file))
        if options.read_token_file is not None:
            options.read_token = harness.Token(
                policy.read_token_file(options.read_token_file)
            )
        _preflight(token, options)
    except policy.PolicyRefusal as exc:
        print(f"refused: {exc}", file=sys.stderr)
        return 2
    except harness.TransportError as exc:
        print(f"refused: target unreachable: {exc}", file=sys.stderr)
        return 2
    print(
        f"target {target.host} ({target.base_url}); mode "
        f"{'write' if options.allow_writes else 'read-only'}"
    )
    return _run(options, token, fake=False)


def _validated_target(options: harness.Options) -> policy.Target:
    if not options.base_url:
        raise policy.PolicyRefusal("--base-url is required (or use --fake)")
    return policy.validate_base_url(options.base_url)


def _run(options: harness.Options, token: harness.Token, *, fake: bool) -> int:
    assert options.base_url is not None
    http = HttpClient(options.base_url, token=token, timeout=options.timeout)
    client = McpClient(http)
    tag = harness.run_tag()
    harness.install_atexit_sweep(client, tag)
    summary = harness.run_cases(
        harness.select(all_cases(), options.only),
        client=client,
        options=options,
        tag=tag,
        known_issues=report.load_known_issues(),
    )
    markdown_path = options.report
    json_path = options.json_report
    if (
        markdown_path is not None
        and markdown_path.suffix == ".json"
        and json_path is None
    ):
        json_path, markdown_path = markdown_path, None
    report.write_reports(
        summary,
        markdown_path=markdown_path,
        json_path=json_path,
        secrets=client.secrets,
    )
    counts = summary.results
    failed_count = sum(1 for row in counts if row.counts_as_failure)
    print()
    print(
        f"{len(counts)} checks: "
        f"{sum(1 for row in counts if row.verdict == 'PASS')} PASS, "
        f"{failed_count} FAIL, "
        f"{sum(1 for row in counts if row.verdict == 'FAIL-known')} FAIL-known, "
        f"{sum(1 for row in counts if row.verdict == 'SKIP')} SKIP"
    )
    if markdown_path is not None:
        print(f"report: {markdown_path}")
    if json_path is not None:
        print(f"json:   {json_path}")
    if not fake and options.with_sessions:
        print(f"run tag: {tag} (any session whose reason carries it is a leak)")
    return 1 if failed_count else 0


if __name__ == "__main__":
    raise SystemExit(main())
