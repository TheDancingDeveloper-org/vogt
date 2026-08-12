"""Content digests.

Used for `audit.payload_digest` at M0 and for observation dedup from M2
(FR-O7), which is why the canonical form is fixed here once: sorted keys,
no insignificant whitespace, UTF-8.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any


def canonical_json(payload: Any) -> str:
    """Render a payload in the one form digests are computed over."""
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), default=str)


def digest_of(payload: Any) -> str:
    """Return the ``sha256:`` digest of a payload's canonical JSON form."""
    encoded = canonical_json(payload).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"
