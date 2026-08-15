"""Where this instance is, as a client sees it (FR-A8, FR-A9, MERGE §5.3).

Vogt answers two questions about its own address — `/connection-info` and
`connect` — and both were right for exactly as long as Vogt was the only
process serving them. r9's merge put a Rust front door in front of the core,
and the core kept answering with its own address and its own mount points:
`http://vogt-dev.tailc7d3c.ts.net:8910` and `/api`, while clients arrive at
`https://vogt-dev.sprooty.com` and `/api/vogt`. The `connect` operation, whose
entire reason for existing is that a client should not have to work out how to
reach the product, rendered a pasteable MCP configuration pointing at an
address nothing can reach (#26).

r10's rule is that a client-facing fact about an address belongs to the
process that publishes the address. The core does not publish it and cannot
learn it — the same argument r7 made for the core against its own container
port, one hop further out. So the door supplies it and the core renders with
it, which keeps the *rendering* in one place: `connect` is a hundred lines of
prose and JSON shapes that are Vogt's content, not addressing, and mirroring
them into the front door would be a second copy to drift.

**Why this is not the rule FR-S2 forbids.** `build_context` deliberately has
nowhere to inject a principal, because *who you are* is never read from
request data. This is not that. An address is not a claim about the caller and
grants nothing; it is the deployment describing its own shape. Two things keep
the distinction honest:

- `config.fronted` — a deployment states that it *is* fronted. An instance
  that has not said so ignores these headers entirely, so the core-only shape
  cannot be told it lives somewhere else by anyone who can reach it.
- Nothing here is authorisation. The identity chooses what a response *says*,
  never what a request is *allowed to do*.

Without both, an unfronted instance reachable by a client would render
`connect` — a document whose whole purpose is to be pasted into a client
config, with a token beside it — against an attacker's URL.
"""

from __future__ import annotations

from dataclasses import dataclass

from vogt.config import VogtConfig

#: The headers a front door uses to say what it publishes. Named rather than
#: derived from `X-Forwarded-*`: those describe one hop of a proxy chain and
#: are set by anything in the path, while these are a deliberate statement by
#: the process that owns the mount points.
HEADER_URL = "x-vogt-public-url"
HEADER_API_PATH = "x-vogt-api-path"
HEADER_MCP_PATH = "x-vogt-mcp-path"

#: What the core serves at, and therefore what it reports when it is the door.
DEFAULT_API_PATH = "/api"
DEFAULT_MCP_PATH = "/mcp"


@dataclass(frozen=True)
class PublicIdentity:
    """The address and mount points a client should use.

    `url` is `None` when nobody has said — which is an answer, and a different
    one from a URL that does not work.
    """

    url: str | None = None
    api_path: str = DEFAULT_API_PATH
    mcp_path: str = DEFAULT_MCP_PATH

    @property
    def mcp_url(self) -> str | None:
        return None if self.url is None else f"{self.url}{self.mcp_path}"


def identity_from_config(config: VogtConfig) -> PublicIdentity:
    """This instance describing itself: the core-only shape, unchanged."""
    return PublicIdentity(url=_clean_url(config.public_url))


def identity_from_headers(
    config: VogtConfig, headers: object, base: PublicIdentity | None = None
) -> PublicIdentity:
    """Resolve the identity for one request, honouring a door that said so.

    `headers` is anything with a case-insensitive `get`, which is every HTTP
    framework's header mapping; the adapter passes its own rather than this
    module learning about a web framework.

    `base` is what this process would answer unfronted. It is a parameter
    because the caller may know better than the defaults here: the HTTP app
    can be mounted somewhere other than `/api`, and an unfronted instance must
    keep reporting where it actually serves.

    Falls back field by field rather than all-or-nothing: a door that
    publishes the core's paths unchanged sends only the URL, and should not
    have to restate two constants to be believed.
    """
    base = base if base is not None else identity_from_config(config)
    if not config.fronted:
        return base

    get = getattr(headers, "get", None)
    if not callable(get):  # pragma: no cover - defensive
        return base

    return PublicIdentity(
        url=_clean_url(get(HEADER_URL)) or base.url,
        api_path=_clean_path(get(HEADER_API_PATH)) or base.api_path,
        mcp_path=_clean_path(get(HEADER_MCP_PATH)) or base.mcp_path,
    )


def _clean_url(value: object) -> str | None:
    """A URL with no trailing slash, or nothing at all."""
    if not isinstance(value, str):
        return None
    return value.strip().rstrip("/") or None


def _clean_path(value: object) -> str | None:
    """A mount point, normalised to a leading slash and no trailing one.

    A path is joined to a URL by concatenation, so `api` and `/api/` produce
    two different wrong answers and neither fails loudly.
    """
    if not isinstance(value, str):
        return None
    candidate = value.strip().rstrip("/")
    if not candidate:
        return None
    return candidate if candidate.startswith("/") else f"/{candidate}"


__all__ = [
    "DEFAULT_API_PATH",
    "DEFAULT_MCP_PATH",
    "HEADER_API_PATH",
    "HEADER_MCP_PATH",
    "HEADER_URL",
    "PublicIdentity",
    "identity_from_config",
    "identity_from_headers",
]
