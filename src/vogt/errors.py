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
