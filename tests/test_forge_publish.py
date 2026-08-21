"""`forge.publish` — create the remote, push, link (#182, FR-B8).

The acceptance box, pinned:

- publish creates the named repository under the acting credential and
  pushes the local default branch (a stand-in transport mints the repo; the
  push is exercised for real against a local bare remote);
- an existing-repo conflict is a typed refusal, never a clobber;
- **no force-push on any path** — asserted against the recorded git argv;
- a published project is thereafter linked / upstream-truth.
"""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest

import vogt.adapters.git.clone as clone_mod
from vogt.adapters.git import (
    Pusher,
    PushRequest,
    inspect_publish_source,
    push_branch,
)
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    CreateWorkParams,
    ForgeLinkParams,
    ForgePublishParams,
    GetWorkParams,
    InitParams,
    OnboardParams,
    RegisterProjectParams,
    SetWriteBackParams,
)
from vogt.application.services import (
    create_work,
    get_work,
    init_instance,
    link_project,
    onboard,
    publish_project,
    register_project,
    set_write_back,
)
from vogt.errors import (
    PublishNonFastForward,
    PublishRefused,
    PublishSourceInvalid,
    PublishWorkingTreeDirty,
    RemoteRepoExists,
)

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock
from tests.test_upstream_truth import RecordingForge

WHY = "publish test"


class PublishForge(RecordingForge):
    """The #181 stand-in forge, taught the one new endpoint (#182).

    `POST /user/repos` mints a repository under the fixed account `acme`;
    a name in `existing` answers GitHub's 422, which is how the conflict
    mapping is exercised without a network.
    """

    def __init__(
        self, *, fail_mutations: bool = False, existing: tuple[str, ...] = ()
    ) -> None:
        super().__init__(fail_mutations=fail_mutations)
        self.existing = set(existing)
        self.created: list[dict[str, Any]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if method == "POST" and url.rstrip("/").endswith("/user/repos"):
            self.requests.append((method, url))
            payload = json.loads(body.decode()) if body else {}
            self.bodies.append(payload)
            name = str(payload.get("name", ""))
            if self.fail_mutations:
                return 500, b"upstream said no"
            if name in self.existing:
                return 422, json.dumps(
                    {"message": "name already exists on this account"}
                ).encode()
            self.created.append(payload)
            return 201, json.dumps(
                {
                    "name": name,
                    "owner": {"login": "acme"},
                    "private": payload.get("private", True),
                    "default_branch": None,
                    "html_url": f"https://github.com/acme/{name}",
                }
            ).encode()
        return super().__call__(url, headers, body, method)


def _git(args: list[str], cwd: Path) -> str:
    completed = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
    )
    return completed.stdout.strip()


def _seed_repo(root: Path) -> str:
    """A publishable checkout: one branch `main`, one commit, clean tree."""
    root.mkdir(parents=True, exist_ok=True)
    _git(["init", "-q", "-b", "main"], root)
    (root / "README.md").write_text("publish me\n", encoding="utf-8")
    _git(["add", "."], root)
    _git(
        [
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=Publish Test",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
        root,
    )
    return _git(["rev-parse", "HEAD"], root)


class RecordingPusher:
    """Captures the one PushRequest the operation makes."""

    def __init__(self) -> None:
        self.requests: list[PushRequest] = []

    def __call__(self, request: PushRequest) -> clone_mod.PushOutcome:
        self.requests.append(request)
        return clone_mod.PushOutcome(
            remote=request.remote, branch=request.branch, revision="0" * 40
        )


def _instance(tmp_path: Path, forge: PublishForge, pusher: Pusher) -> AppContext:
    from vogt.config import VogtConfig

    token_file = tmp_path / "github_token"
    token_file.write_text("ghp_file_token", encoding="utf-8")
    ctx = build_context(
        config=VogtConfig(
            data_dir=tmp_path / "instance",
            import_root=tmp_path / "imported",
            sqlite_synchronous="off",
            github_token_file=token_file,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        forge_transport=forge,
        pusher=pusher,
    )
    init_instance(ctx, InitParams())
    return ctx


@pytest.fixture
def forge() -> PublishForge:
    return PublishForge()


@pytest.fixture
def pusher() -> RecordingPusher:
    return RecordingPusher()


@pytest.fixture
def publishable(
    tmp_path: Path, forge: PublishForge, pusher: RecordingPusher
) -> AppContext:
    """A registered local-only project whose root is a clean git checkout."""
    ctx = _instance(tmp_path, forge, pusher)
    _seed_repo(tmp_path / "demo")
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path=str(tmp_path / "demo"), reason=WHY
        ),
    )
    return ctx


# -- acceptance: create + push + linked ------------------------------------


def test_publish_creates_the_repo_pushes_and_links(
    publishable: AppContext, forge: PublishForge, pusher: RecordingPusher
) -> None:
    result = publish_project(
        publishable, ForgePublishParams(project="demo", reason=WHY)
    )

    assert forge.created and forge.created[0]["name"] == "demo", (
        "the repository name defaults to the project slug"
    )
    assert forge.created[0]["private"] is True, "private by default"
    assert forge.created[0]["auto_init"] is False, (
        "no generated first commit — the local history is the history"
    )

    assert len(pusher.requests) == 1
    push = pusher.requests[0]
    assert push.remote == "https://github.com/acme/demo.git", (
        "the push remote is the plain clone URL"
    )
    assert push.branch == "main"
    assert push.token == "ghp_file_token", (
        "authenticated with the resolved credential, per invocation"
    )
    assert push.token not in push.remote, (
        "the token is never embedded in the remote URL (FR-S8)"
    )

    assert result.repo == "https://github.com/acme/demo"
    assert result.branch == "main"
    assert result.project.link_state == "linked", "published means linked (#181)"
    assert result.project.repo_url == "https://github.com/acme/demo"
    assert result.migrated == []

    with publishable.declared.read() as view:
        operations = [r.operation for r in view.list_audit(limit=50)]
    assert "forge.publish" in operations


def test_a_published_project_is_upstream_truth_thereafter(
    publishable: AppContext, forge: PublishForge, pusher: RecordingPusher
) -> None:
    set_write_back(
        publishable, SetWriteBackParams(project="demo", policy="full", reason=WHY)
    )
    publish_project(publishable, ForgePublishParams(project="demo", reason=WHY))
    created = create_work(
        publishable,
        CreateWorkParams(kind="bug", title="First", project="demo", reason=WHY),
    )
    assert created.item.ref == "gh:acme/demo#1", (
        "work.create on the published project writes through (#181)"
    )
    onboard(publishable, OnboardParams(project="demo", reason=WHY))
    fetched = get_work(publishable, GetWorkParams(ref="gh:acme/demo#1"))
    assert fetched.item.title == "First"


def test_the_name_and_visibility_are_the_callers(
    publishable: AppContext, forge: PublishForge, pusher: RecordingPusher
) -> None:
    result = publish_project(
        publishable,
        ForgePublishParams(
            project="demo",
            name="renamed",
            private=False,
            description="a demo",
            reason=WHY,
        ),
    )
    assert forge.created[0]["name"] == "renamed"
    assert forge.created[0]["private"] is False
    assert forge.created[0]["description"] == "a demo"
    assert result.project.repo_url == "https://github.com/acme/renamed"


# -- acceptance: typed refusals, nothing clobbered --------------------------


def test_an_existing_remote_is_a_typed_refusal_not_a_clobber(
    tmp_path: Path, pusher: RecordingPusher
) -> None:
    forge = PublishForge(existing=("demo",))
    ctx = _instance(tmp_path, forge, pusher)
    _seed_repo(tmp_path / "demo")
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path=str(tmp_path / "demo"), reason=WHY
        ),
    )
    with pytest.raises(RemoteRepoExists, match="never adopts"):
        publish_project(ctx, ForgePublishParams(project="demo", reason=WHY))
    assert pusher.requests == [], "nothing was pushed at an existing remote"
    with ctx.declared.read() as view:
        project = view.project_by_slug("demo")
    assert project is not None
    assert project.link_state == "unlinked" and project.repo_url is None, (
        "the refusal changed nothing locally"
    )


def test_an_already_linked_project_is_refused(
    publishable: AppContext, forge: PublishForge, pusher: RecordingPusher
) -> None:
    publish_project(publishable, ForgePublishParams(project="demo", reason=WHY))
    with pytest.raises(PublishRefused, match="already linked"):
        publish_project(publishable, ForgePublishParams(project="demo", reason=WHY))


def test_a_project_with_a_repo_url_is_pointed_at_link_instead(
    tmp_path: Path, forge: PublishForge, pusher: RecordingPusher
) -> None:
    ctx = _instance(tmp_path, forge, pusher)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo",
            root_path=str(tmp_path / "demo"),
            repo_url="https://github.com/acme/existing",
            reason=WHY,
        ),
    )
    with pytest.raises(PublishRefused, match="forge link"):
        publish_project(ctx, ForgePublishParams(project="demo", reason=WHY))


def test_a_dirty_tree_is_refused_by_name(
    publishable: AppContext, forge: PublishForge, pusher: RecordingPusher
) -> None:
    with publishable.declared.read() as view:
        project = view.project_by_slug("demo")
    assert project is not None
    (Path(project.root_path) / "unsaved.txt").write_text("wip\n", encoding="utf-8")
    with pytest.raises(PublishWorkingTreeDirty, match=r"unsaved\.txt"):
        publish_project(publishable, ForgePublishParams(project="demo", reason=WHY))
    assert forge.created == [] and pusher.requests == []


def test_a_root_without_a_repository_is_refused_by_name(
    tmp_path: Path, forge: PublishForge, pusher: RecordingPusher
) -> None:
    ctx = _instance(tmp_path, forge, pusher)
    plain = tmp_path / "plain"
    plain.mkdir()
    register_project(
        ctx,
        RegisterProjectParams(name="Plain", root_path=str(plain), reason=WHY),
    )
    with pytest.raises(PublishSourceInvalid, match="not a git repository"):
        publish_project(ctx, ForgePublishParams(project="plain", reason=WHY))


# -- the gate, unit-level ---------------------------------------------------


def test_the_gate_refuses_an_empty_repository(tmp_path: Path) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    _git(["init", "-q", "-b", "main"], empty)
    with pytest.raises(PublishSourceInvalid, match="no commits"):
        inspect_publish_source(empty)


def test_the_gate_refuses_a_detached_head(tmp_path: Path) -> None:
    root = tmp_path / "detached"
    _seed_repo(root)
    _git(["checkout", "-q", "--detach", "HEAD"], root)
    with pytest.raises(PublishSourceInvalid, match="detached"):
        inspect_publish_source(root)


def test_the_gate_reads_the_branch_and_revision(tmp_path: Path) -> None:
    root = tmp_path / "clean"
    revision = _seed_repo(root)
    source = inspect_publish_source(root)
    assert source.branch == "main"
    assert source.revision == revision


# -- the push, for real, against a bare remote ------------------------------


@pytest.fixture
def spied_git(monkeypatch: pytest.MonkeyPatch) -> list[list[str]]:
    """Every git argv the adapter runs, for the no-force assertion."""
    recorded: list[list[str]] = []
    original = clone_mod._run_git

    def spy(args: list[str], **kwargs: Any) -> str:
        recorded.append(list(args))
        return original(args, **kwargs)

    monkeypatch.setattr(clone_mod, "_run_git", spy)
    return recorded


def _assert_no_force(recorded: list[list[str]]) -> None:
    for argv in recorded:
        assert "--force" not in argv and "-f" not in argv, argv
        assert "--force-with-lease" not in argv, argv
        assert not any(arg.startswith("+") for arg in argv), (
            f"a +refspec is a force by another spelling: {argv}"
        )


def test_push_branch_pushes_plainly_to_a_bare_remote(
    tmp_path: Path, spied_git: list[list[str]]
) -> None:
    bare = tmp_path / "remote.git"
    bare.mkdir()
    _git(["init", "-q", "--bare", str(bare)], tmp_path)
    root = tmp_path / "local"
    revision = _seed_repo(root)

    outcome = push_branch(
        PushRequest(root=root, remote=str(bare), branch="main", token=None)
    )
    assert outcome.branch == "main"
    assert outcome.revision == revision
    assert _git(["rev-parse", "refs/heads/main"], bare) == revision, (
        "the bare remote holds exactly the pushed commit"
    )
    _assert_no_force(spied_git)


def test_a_non_fast_forward_is_refused_never_forced(
    tmp_path: Path, spied_git: list[list[str]]
) -> None:
    bare = tmp_path / "remote.git"
    bare.mkdir()
    _git(["init", "-q", "--bare", str(bare)], tmp_path)
    # Somebody else got there first: a diverged main already on the remote.
    other = tmp_path / "other"
    _seed_repo(other)
    (other / "theirs.txt").write_text("theirs\n", encoding="utf-8")
    _git(["add", "."], other)
    _git(
        [
            "-c",
            "user.email=other@example.invalid",
            "-c",
            "user.name=Other",
            "commit",
            "-q",
            "-m",
            "theirs",
        ],
        other,
    )
    push_branch(PushRequest(root=other, remote=str(bare), branch="main"))

    ours = tmp_path / "ours"
    _seed_repo(ours)
    before = _git(["rev-parse", "refs/heads/main"], bare)
    with pytest.raises(PublishNonFastForward, match="never forces"):
        push_branch(PushRequest(root=ours, remote=str(bare), branch="main"))
    assert _git(["rev-parse", "refs/heads/main"], bare) == before, (
        "the remote's history stands"
    )
    _assert_no_force(spied_git)


def test_publish_end_to_end_with_the_real_pusher(
    tmp_path: Path, forge: PublishForge, spied_git: list[list[str]]
) -> None:
    """The operation drives the real `git push` against a local bare remote.

    The pusher seam redirects only the *remote address* — everything else,
    the gate included, is the production path — which is what makes "no
    force on any path" an assertion about the operation rather than about
    one helper.
    """
    bare = tmp_path / "remote.git"
    bare.mkdir()
    _git(["init", "-q", "--bare", str(bare)], tmp_path)

    def to_bare(request: PushRequest) -> clone_mod.PushOutcome:
        assert request.remote == "https://github.com/acme/demo.git"
        return push_branch(replace(request, remote=str(bare), token=None))

    ctx = _instance(tmp_path, forge, to_bare)
    revision = _seed_repo(tmp_path / "demo")
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path=str(tmp_path / "demo"), reason=WHY
        ),
    )
    result = publish_project(ctx, ForgePublishParams(project="demo", reason=WHY))
    assert result.revision == revision
    assert _git(["rev-parse", "refs/heads/main"], bare) == revision
    assert result.project.link_state == "linked"
    _assert_no_force(spied_git)


# -- publish shares link's credential rule ---------------------------------


def test_publish_without_any_credential_is_refused(tmp_path: Path) -> None:
    from vogt.config import VogtConfig

    ctx = build_context(
        config=VogtConfig(data_dir=tmp_path / "instance", sqlite_synchronous="off"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )
    init_instance(ctx, InitParams())
    _seed_repo(tmp_path / "demo")
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path=str(tmp_path / "demo"), reason=WHY
        ),
    )
    with pytest.raises(PublishRefused, match="credential"):
        publish_project(ctx, ForgePublishParams(project="demo", reason=WHY))


def test_link_still_works_beside_publish(
    tmp_path: Path, forge: PublishForge, pusher: RecordingPusher
) -> None:
    """The two explicit acts stay distinct: link attaches, publish creates."""
    ctx = _instance(tmp_path, forge, pusher)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo",
            root_path=str(tmp_path / "demo"),
            repo_url="https://github.com/acme/demo",
            reason=WHY,
        ),
    )
    linked = link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    assert linked.project.link_state == "linked"
    assert linked.migrated == []
