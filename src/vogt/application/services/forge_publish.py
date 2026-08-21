"""`forge.publish` — create the remote, push, link, migrate (#182, #183).

The first verb in the product that *creates upstream state and pushes
commits* — everything before it was the deliberately non-destructive FR-B4
set. The bounded shape (FR-B8):

- **Refuses an existing remote.** The repository is created fresh under the
  acting actor's credential; a name that already exists upstream is a typed
  refusal from the provider (`RemoteRepoExists`), never a clobber, and a
  project that already carries a `repo_url` or is already linked is refused
  here before anything leaves the machine — attaching to an existing
  repository is `forge.link`'s explicit act.
- **Never force-pushes.** The push is `push_branch`'s plain `git push`; a
  non-fast-forward rejection is a typed refusal handed back to the person.
- **Requires clean, explicit local state.** `inspect_publish_source` is the
  read-only gate (#180's posture at the other boundary): a git repository at
  the project root, a clean working tree, a named branch on a commit.
- **The token is never written down.** The push authenticates through the
  same `GIT_ASKPASS` road the clone uses (FR-S8): the remote URL is the
  plain clone URL, built per invocation, and the credential reaches git only
  through its environment — never argv, never `.git/config`.

Ordering is the fail-loud rule (decision 9) applied to a multi-step act:
gate, create remote, push, and only then the declared write that records
`repo_url` and sets the project linked — so a failure at any network step
leaves the project exactly as it was. The one crash window that ordering
leaves (remote created, local record lost) is visible and recoverable: the
repository exists under the actor's account and a retry is refused with its
name, never silently re-pointed. After the link, #183's migration publishes
any open native items upstream, exactly as `forge.link`'s does.
"""

from __future__ import annotations

from pathlib import Path

from vogt.adapters.forge import GitHubProvider
from vogt.adapters.git import PushRequest, inspect_publish_source
from vogt.application.context import AppContext
from vogt.application.models import ForgePublishParams, ForgePublishResult
from vogt.application.services import _resolve, native_migration
from vogt.application.services.writeback import _writer_provider
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Project
from vogt.errors import PublishRefused
from vogt.storage.interface import ProjectUpdate, WriteTxn

FORGE_PUBLISH = "forge.publish"
FORGE_PUBLISHED_EVENT = "forge.published"

#: Publish resolves its provider before a repository exists to name, so the
#: host is probed with a placeholder URL. GitHub-only is the same v1 ceiling
#: the whole write path holds (`_writer_provider` says so too); a second
#: forge makes the target host a parameter, not a constant.
_HOST_PROBE_URL = "https://github.com/-/-"


def publish_project(ctx: AppContext, params: ForgePublishParams) -> ForgePublishResult:
    """Create a remote repository from a local project and hand it to #181."""
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        pending = native_migration.open_native_items(view, project)

    if project.link_state == "linked":
        raise PublishRefused(
            f"{project.slug!r} is already linked to {project.repo_url}; "
            "publish creates a repository for a project that has none"
        )
    if project.repo_url:
        raise PublishRefused(
            f"{project.slug!r} already names a repository "
            f"({project.repo_url}); publish never overwrites that "
            "association — attach to it with `forge link`, or clear the "
            "repo_url first if it is wrong"
        )

    # The read-only gate before anything exists upstream: a git repository at
    # the root, a clean tree, a named branch on a commit — typed refusals.
    source = inspect_publish_source(Path(project.root_path))

    provider, identity = _writer_provider(ctx, actor, _HOST_PROBE_URL)
    if provider is None:
        raise PublishRefused(
            f"cannot publish {project.slug!r}: no usable forge credential — "
            "link your own forge account (`forge account link`, #179) or "
            "configure the instance token file (FR-S7), then retry"
        )
    # Same rule as `forge.link` (#183): if open native items must migrate,
    # a policy that would refuse their creates refuses the whole publish.
    native_migration.require_migratable(project, pending)

    name = params.name or project.slug
    # An existing name is the provider's typed refusal (RemoteRepoExists) —
    # nothing local has changed at that point, by the ordering above.
    repo = provider.create_repo(
        name, private=params.private, description=params.description
    )
    assert isinstance(provider, GitHubProvider)  # the v1 ceiling, as probed
    ref = provider.parse(repo.web_url)
    if ref is None:
        raise PublishRefused(
            f"the forge created {repo.web_url!r} but the provider cannot "
            "parse its own repository address; nothing local was changed"
        )
    # Plain push, authenticated per-invocation: the remote is the bare clone
    # URL and the token rides GIT_ASKPASS — it is never embedded in the URL,
    # argv, or the checkout's configuration (FR-S8). Never --force: a
    # non-fast-forward is `push_branch`'s typed refusal.
    outcome = ctx.pusher(
        PushRequest(
            root=source.root,
            remote=provider.clone_url(ref),
            branch=source.branch,
            token=provider.clone_token(),
            # Same executability constraint as the import's clone: the
            # hardened /tmp is noexec, so the helper lives on the data volume.
            helper_dir=ctx.config.data_dir / "tmp",
        )
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[Project]:
        del actor
        txn.update_project(
            project.id,
            ProjectUpdate(repo_url=repo.web_url, link_state="linked"),
            at=ctx.clock(),
        )
        updated = txn.project_by_id(project.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=updated,
            entity_kind="project",
            entity_id=project.id,
            payload={
                "repo_url": repo.web_url,
                "link_state": "linked",
                "branch": source.branch,
                "credential": identity,
            },
            event_kind=FORGE_PUBLISHED_EVENT,
            summary={
                "slug": project.slug,
                "repo": repo.web_url,
                "branch": source.branch,
                "credential": identity,
            },
        )

    published = audited_write(
        ctx, operation=FORGE_PUBLISH, reason=params.reason, body=body
    )

    migrated = []
    if pending:
        migrated = native_migration.migrate_open_native_items(
            ctx,
            project=published,
            items=pending,
            provider=provider,
            identity=identity,
            repo=ref,
            reason=params.reason,
            operation=FORGE_PUBLISH,
        )
    return ForgePublishResult(
        project=published,
        repo=repo.web_url,
        branch=outcome.branch,
        revision=outcome.revision,
        migrated=migrated,
    )


__all__ = ["FORGE_PUBLISH", "FORGE_PUBLISHED_EVENT", "publish_project"]
