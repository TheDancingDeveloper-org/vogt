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
