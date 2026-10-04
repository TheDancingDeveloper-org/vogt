"""Per-actor preferences: small, versioned settings that follow a login.

The store is generic — a JSON object per `(actor, key)` — so any surface can
keep a person's settings server-side instead of in one browser's
localStorage. The Inbox's saved filter (`inbox.filter`) is the first key;
Board and Backlog saved filters can move onto it without another table.

Everything here acts on the *calling* principal's own rows. There is no
parameter naming another actor, so neither operation can read or write
anybody else's settings — which is why `preference.set` needs only the
`read` scope, as `auth.logout` does.

A key with a known shape (`KNOWN_SHAPES`) is validated on write, so a
malformed value is refused with a useful error rather than stored and
silently ignored by its reader. Unknown keys are accepted as opaque objects,
bounded in size.
"""

from __future__ import annotations

import json

from pydantic import BaseModel, ValidationError

from vogt.application.context import AppContext
from vogt.application.models import (
    InboxSavedFilter,
    PreferenceGetParams,
    PreferenceGetResult,
    PreferenceSetParams,
    PreferenceSetResult,
    PreferenceView,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, ActorPreference
from vogt.errors import InvalidPreference, PreferenceVersionConflict
from vogt.storage.interface import ReadView, WriteTxn

PREFERENCE_SET_EVENT = "preference.set"
INBOX_FILTER_KEY = "inbox.filter"
#: A preference is a setting, not a document store.
MAX_VALUE_BYTES = 16 * 1024

#: Keys whose value has a defined shape. `{}` (cleared) is always accepted.
KNOWN_SHAPES: dict[str, type[BaseModel]] = {INBOX_FILTER_KEY: InboxSavedFilter}


def get_preferences(
    ctx: AppContext, params: PreferenceGetParams
) -> PreferenceGetResult:
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        if actor is None:
            # A principal that has never written has no actor row and so no
            # settings; a read does not auto-register.
            return PreferenceGetResult(preferences=[])
        if params.key is not None:
            found = view.actor_preference(actor_id=actor.id, key=params.key)
            rows = [] if found is None else [found]
        else:
            rows = view.actor_preferences(actor.id)
    return PreferenceGetResult(preferences=[_view(row) for row in rows])


def set_preference(ctx: AppContext, params: PreferenceSetParams) -> PreferenceSetResult:
    value = _validated(params.key, params.value)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[PreferenceSetResult]:
        existing = txn.actor_preference(actor_id=actor.id, key=params.key)
        current = 0 if existing is None else existing.version
        if params.expected_version is not None and params.expected_version != current:
            msg = (
                f"preference {params.key!r} is at version {current}, not "
                f"{params.expected_version}; re-read it and apply the change "
                "on top of the current value"
            )
            raise PreferenceVersionConflict(msg)
        preference = ActorPreference(
            actor_id=actor.id,
            key=params.key,
            value=value,
            version=current + 1,
            updated_at=ctx.clock(),
        )
        txn.upsert_actor_preference(preference)
        return WriteOutcome(
            result=PreferenceSetResult(preference=_view(preference)),
            entity_kind="preference",
            entity_id=f"{actor.id}:{params.key}",
            payload=preference.model_dump(mode="json"),
            event_kind=PREFERENCE_SET_EVENT,
            summary={
                "key": params.key,
                "version": preference.version,
                "cleared": not value,
            },
        )

    return audited_write(
        ctx, operation="preference.set", reason=params.reason, body=body
    )


def saved_inbox_filter(ctx: AppContext, view: ReadView) -> InboxSavedFilter | None:
    """The caller's saved Inbox filter, or None when they have none.

    Lenient on read: a stored value that no longer parses (an older shape) is
    treated as no filter rather than failing the badge."""
    actor = view.actor_by_identity(ctx.principal.identity_ref)
    if actor is None:
        return None
    stored = view.actor_preference(actor_id=actor.id, key=INBOX_FILTER_KEY)
    if stored is None or not stored.value:
        return None
    try:
        parsed = InboxSavedFilter.model_validate(stored.value)
    except ValidationError:
        return None
    return None if parsed.is_default() else parsed


def _validated(key: str, value: dict[str, object]) -> dict[str, object]:
    try:
        encoded = json.dumps(value, sort_keys=True)
    except (TypeError, ValueError) as error:
        raise InvalidPreference(f"value is not JSON-serialisable: {error}") from None
    if len(encoded.encode("utf-8")) > MAX_VALUE_BYTES:
        msg = f"preference values are limited to {MAX_VALUE_BYTES} bytes of JSON"
        raise InvalidPreference(msg)
    shape = KNOWN_SHAPES.get(key)
    if shape is None or not value:
        return value
    try:
        return shape.model_validate(value).model_dump(mode="json")
    except ValidationError as error:
        problems = "; ".join(
            f"{'.'.join(str(part) for part in item['loc']) or 'value'}: {item['msg']}"
            for item in error.errors()
        )
        raise InvalidPreference(f"invalid {key} value — {problems}") from None


def _view(row: ActorPreference) -> PreferenceView:
    return PreferenceView(
        key=row.key, value=row.value, version=row.version, updated_at=row.updated_at
    )


__all__ = [
    "INBOX_FILTER_KEY",
    "get_preferences",
    "saved_inbox_filter",
    "set_preference",
]
