"""Turning what callers type into what the store holds.

Callers name things the way humans and agents do — `WI-7`, a project slug, an
actor's `identity_ref`. Everything resolves here so that a mistyped reference
produces "no work item WI-70" rather than a foreign-key error surfacing from
three layers down.
"""

from __future__ import annotations

from vogt.core.entities import Actor, Initiative, Project, WorkItem
from vogt.errors import NotFound
from vogt.storage.interface import ReadView


def project(view: ReadView, slug: str) -> Project:
    found = view.project_by_slug(slug)
    if found is None:
        msg = f"no project with slug {slug!r}"
        raise NotFound(msg)
    return found


def work_item(view: ReadView, ref: str) -> WorkItem:
    found = view.work_item_by_ref(ref)
    if found is None:
        msg = f"no work item {ref!r}"
        raise NotFound(msg)
    return found


def actor(view: ReadView, identity_ref: str) -> Actor:
    found = view.actor_by_identity(identity_ref)
    if found is None:
        msg = (
            f"no actor with identity {identity_ref!r} — "
            "create it with `actor create` first"
        )
        raise NotFound(msg)
    return found


def initiative(view: ReadView, slug: str) -> Initiative:
    found = view.initiative_by_slug(slug)
    if found is None:
        msg = f"no initiative with slug {slug!r}"
        raise NotFound(msg)
    return found


def label_exists(view: ReadView, name: str) -> None:
    if view.label_by_name(name) is None:
        msg = f"no label named {name!r} — create it with `label create` first"
        raise NotFound(msg)
