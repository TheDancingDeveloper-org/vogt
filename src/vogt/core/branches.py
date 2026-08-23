"""Branch ↔ work-item binding — the naming convention, stated once (#283).

A branch belongs to a work item when its name carries that item's reference.
Three shapes are recognised by default and the set is configuration
(`branch_binding_patterns`), because which prefixes an estate uses for its
branches is the estate's business, not Vogt's:

- ``wi-7/…`` / ``feature/WI-7-…`` — the vogt work-item number, matched
  case-insensitively with an optional dash after ``wi``.
- ``gh-264-…`` — the forge issue number, for a linked project whose items are
  its upstream issues (#181). The number alone is recorded here; resolving it
  to a work item is a cross-store question the application layer answers, the
  same division `git_local` keeps for every other match.

Nothing in this module reads git or the store. It turns a branch name into a
match, and a work-item ref into the branch a Vogt-started session will use —
two pure functions the collector, the session write, and the tests all share,
so the convention has exactly one implementation to disagree with itself.

The binding is **observed-first and additive** (#287): matching a branch never
drives git, and a declared default is recorded next to — never on top of — what
a sweep observes.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: The shipped default match patterns. Each is a regular expression carrying at
#: most one of two named groups: ``n`` (a vogt work-item number → ``WI-<n>``)
#: or ``forge`` (a forge issue number, recorded as-is). A word boundary rather
#: than the ``(?:^|…)`` alternation anchors them, deliberately: the ``|`` would
#: split the generated CONFIG.md table into extra columns, and ``\b`` says the
#: same thing — ``kiwi-7`` is not ``wi-7`` — without one.
DEFAULT_BRANCH_PATTERNS: tuple[str, ...] = (
    r"(?i)\bwi-?(?P<n>\d+)\b",
    r"(?i)\bgh-(?P<forge>\d+)\b",
)

#: The shipped default for the branch a Vogt-started session declares it will
#: use. Formatted with a single ``{number}`` field; ``WI-7`` → ``wi-7``.
DEFAULT_BRANCH_TEMPLATE = "wi-{number}"

_WI_REF = re.compile(r"(?i)^wi-(\d+)$")
_FORGE_SUBJECT = re.compile(r"#(\d+)$")


@dataclass(frozen=True)
class BranchMatch:
    """What a branch name says about which work item it belongs to.

    Exactly one of the two is set. ``work_ref`` is a resolved vogt ref
    (``WI-7``); ``forge_number`` is a bare issue number the caller resolves
    against a linked project, because a branch name cannot know which
    repository it belongs to.
    """

    work_ref: str | None = None
    forge_number: int | None = None


def match_branch(name: str, patterns: tuple[str, ...]) -> BranchMatch | None:
    """The work item a branch name binds to, or ``None`` for an unrelated name.

    First match wins, in the order the patterns are configured. A pattern that
    does not compile is skipped rather than raised: a bad estate-supplied
    pattern must not cost a sweep every other branch it could have matched.
    """
    for raw in patterns:
        try:
            pattern = re.compile(raw)
        except re.error:
            continue
        match = pattern.search(name)
        if match is None:
            continue
        groups = match.groupdict()
        number = groups.get("n")
        if number:
            return BranchMatch(work_ref=f"WI-{int(number)}")
        forge = groups.get("forge")
        if forge:
            return BranchMatch(forge_number=int(forge))
    return None


def default_branch_name(work_ref: str, *, template: str) -> str:
    """The branch a Vogt-started session for ``work_ref`` will use.

    A native ``WI-7`` renders through ``template`` (default ``wi-7``); an
    upstream subject key (``gh:owner/repo#264``) renders to the forge form
    ``gh-264`` regardless of the template, because that is the shape its own
    default project recognises. Anything else falls back to a slug of the ref,
    so the function is total and a session always has a name to declare.
    """
    wi = _WI_REF.match(work_ref)
    if wi:
        return template.format(number=wi.group(1))
    forge = _FORGE_SUBJECT.search(work_ref)
    if forge:
        return f"gh-{forge.group(1)}"
    return re.sub(r"[^a-z0-9]+", "-", work_ref.lower()).strip("-") or "work"


__all__ = [
    "DEFAULT_BRANCH_PATTERNS",
    "DEFAULT_BRANCH_TEMPLATE",
    "BranchMatch",
    "default_branch_name",
    "match_branch",
]
