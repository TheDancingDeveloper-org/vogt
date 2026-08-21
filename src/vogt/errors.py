"""The error taxonomy every adapter maps onto its own transport.

Adapters translate these into exit codes (CLI), status codes (HTTP), and
error payloads (MCP). Keeping the taxonomy here — rather than raising
transport-shaped errors from the application layer — is what lets the
parity tests assert that all three surfaces fail the same way.
"""

from __future__ import annotations


class VogtError(Exception):
    """Base class for every error Vogt raises deliberately."""

    #: Stable machine-readable code, surfaced on every transport.
    code = "error"
    #: HTTP status the REST adapter uses for this class of error.
    http_status = 500


class NotInitialized(VogtError):
    """The data directory has no initialised instance in it."""

    code = "not_initialized"
    http_status = 409


class AlreadyInitialized(VogtError):
    """`init` was asked to create an instance that already exists."""

    code = "already_initialized"
    http_status = 409


class NotFound(VogtError):
    """A referenced entity does not exist."""

    code = "not_found"
    http_status = 404


class Conflict(VogtError):
    """The write conflicts with existing state (e.g. a duplicate slug)."""

    code = "conflict"
    http_status = 409


class InvalidRequest(VogtError):
    """The request is structurally valid but semantically wrong."""

    code = "invalid_request"
    http_status = 400


class InvalidCursor(InvalidRequest):
    """A paging cursor does not belong to the requested Inbox query."""

    code = "invalid_cursor"


class ForgeAccountsNotConfigured(InvalidRequest):
    """Per-actor forge account linking needs a key that this instance lacks.

    Linking stores a recoverable PAT encrypted at rest, so it depends on a
    Fernet key file (`forge_account_key_file`). With no key there is no way
    to store a token safely, so the feature is *off* rather than insecure:
    an honest "not configured" is the only correct answer (issue #179).
    """

    code = "forge_accounts_not_configured"


class ImportParityRefused(Conflict):
    """Importing onto a pre-existing checkout is refused: it is not at clean
    parity with origin, and Vogt performs no merge on the user's behalf (#180).

    Import over an existing folder never fetches-and-merges, rebases or stashes.
    When the local default branch has diverged from origin, or the working tree
    is dirty, the honest answer is to hand the reconciliation back to the person
    — push or pull yourself, then retry — rather than guess at a merge. The two
    subclasses name which of the two conditions failed, so the receipt is
    actionable rather than a bare "refused".
    """

    code = "import_parity_refused"


class ImportWorkingTreeDirty(ImportParityRefused):
    """The destination's working tree has uncommitted changes (#180).

    Vogt will not write into a dirty tree to satisfy an import, because the one
    thing worse than refusing is silently reconciling somebody's uncommitted
    work. Commit, stash or discard it yourself, then retry."""

    code = "import_working_tree_dirty"


class ImportBranchDiverged(ImportParityRefused):
    """The local default branch has diverged from origin (#180).

    Local HEAD and origin HEAD on the default branch name two different commits.
    Vogt performs no merge, rebase or stash, so it refuses rather than pick a
    reconciliation the person did not ask for. Push or pull the branch yourself,
    then retry."""

    code = "import_branch_diverged"


class PublishRefused(Conflict):
    """`forge.publish` found a precondition missing, and names it (#182).

    Publishing is the first verb that creates upstream state and pushes
    commits, so every precondition is checked before anything leaves the
    machine: the project must not already be linked or carry a `repo_url`
    (a repository that already exists upstream is `forge.link`'s job, never
    publish's to clobber), and the local checkout must be clean and explicit.
    The subclasses name which condition failed, so the receipt is actionable
    rather than a bare "refused".
    """

    code = "forge_publish_refused"


class PublishSourceInvalid(PublishRefused):
    """The project root is not a publishable git checkout (#182).

    No git repository at the root, no commit to push, or a detached HEAD
    with no branch to name upstream. Publish creates the remote from what is
    on disk, so a source that cannot answer "which branch, which commit" is
    refused before any remote state exists.
    """

    code = "publish_source_invalid"


class PublishWorkingTreeDirty(PublishRefused):
    """The project's working tree has uncommitted changes (#182).

    The same rule as the import parity gate (#180), mirrored at the other
    boundary: Vogt does not commit, stash or discard on the user's behalf,
    and publishing a tree whose state the person has not settled would push
    a history they did not choose. Commit or stash, then retry.
    """

    code = "publish_working_tree_dirty"


class PublishNonFastForward(PublishRefused):
    """The push was rejected as non-fast-forward, and was not forced (#182).

    FR-B4's "no force, ever" applied to the one place a force flag would be
    trivially reachable: `forge.publish` runs a plain `git push`, and a
    remote that is ahead — which for a just-created repository means someone
    else got there in between — is a refusal handed back to the person, not
    a history to overwrite.
    """

    code = "publish_non_fast_forward"


class RemoteRepoExists(PublishRefused):
    """The forge already has a repository by that name (#182).

    Mapped from the forge's own name-conflict answer (GitHub's 422).
    Publish never adopts, reuses or overwrites an existing remote — that
    repository is somebody's state, and attaching to it is `forge.link`'s
    explicit act. Pick another name, or link instead.
    """

    code = "remote_repo_exists"


class NotLinked(Conflict):
    """A write verb needs a forge-linked project, and this one is not (#181).

    Decision 10 of the #178 pivot: on an unlinked project the work write
    verbs — create, comment, label and state changes — do not fall back to a
    local-only work model that would silently diverge from the one the linked
    projects use. The honest answer names the two ways forward: link the
    project (`forge.link`, or re-import it through `project.import`), or
    publish it (`forge.publish`, #182). Items with no project at all are
    untouched by this — there is no project to be linked.
    """

    code = "project_not_linked"


class LinkRefused(InvalidRequest):
    """`forge.link` found a precondition missing, and names it (#181).

    Linking is an explicit act, so it validates up front what write-through
    will need: a `repo_url` some registered provider matches, and a usable
    credential (the acting actor's PAT from #179, or the FR-S7 file token).
    Refusing here, with the missing piece named, beats a project that links
    and then fails its first `work.create`.
    """

    code = "forge_link_refused"


class UpstreamWriteRefused(Conflict):
    """Write-through was refused by policy before anything was sent (#181).

    On a linked project a work write *is* an upstream write (decision 9), so
    a write-back policy that does not permit the action refuses the whole
    operation — loudly, naming the policy — rather than committing a local
    half that the forge never heard about.
    """

    code = "upstream_write_refused"


class UpstreamWriteFailed(VogtError):
    """The forge rejected or failed a write-through, so nothing changed (#181).

    Decision 9's fail-loud rule: no silent queueing, no eventually-consistent
    success. The provider call runs inside the declared transaction and this
    error aborts it, so for `work.create` specifically the caller learns the
    issue was *not* created and no local row claims otherwise.
    """

    code = "upstream_write_failed"
    http_status = 502


class InboxEntryNotFound(NotFound):
    """The requested Inbox occurrence is not in the current projection."""

    code = "inbox_entry_not_found"


class InvalidTriageState(Conflict):
    """The requested triage action does not apply to the entry's state."""

    code = "invalid_triage_state"


class InvalidSnooze(InvalidRequest):
    """A snooze deadline is not a future instant."""

    code = "invalid_snooze"


class MissingReason(InvalidRequest):
    """A write arrived without the reason every audited write requires.

    `reason` is the only caller-supplied audit field (DESIGN §4.1); a write
    without one cannot be explained after the fact, so it is refused rather
    than recorded as an empty string.
    """

    code = "missing_reason"
    http_status = 400


class MigrationError(VogtError):
    """The schema could not be brought forward to the expected version."""

    code = "migration_error"
    http_status = 500


class MigrationLocked(MigrationError):
    """Another process holds the migration lock."""

    code = "migration_locked"
    http_status = 503
