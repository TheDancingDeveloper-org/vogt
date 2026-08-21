"""Trust from last-confirmed, not last-changed (#174).

A forge subject re-read every sweep is still there, even if its payload has
not moved since it was filed. Before Phase 3, trust read the immutable
first-seen `observed_at`, so a stable open issue aged to `stale` at the verify
horizon for no reason but its own steadiness. Now `subject_seen.last_confirmed_at`
takes precedence.
"""

from __future__ import annotations

from datetime import timedelta

from vogt.application.context import AppContext
from vogt.application.services.views import trust_for


def test_a_reconfirmed_subject_stays_verified(instance: AppContext) -> None:
    now = instance.clock()
    horizon = timedelta(hours=instance.config.verify_horizon_hours)
    stale_observed = now - horizon - timedelta(hours=1)
    fresh_confirmed = now - timedelta(minutes=5)

    # First-seen long ago and nothing changed since: without confirmation this
    # is `stale`.
    assert trust_for(instance, observed_at=stale_observed) == "stale"
    # But it was confirmed to still exist five minutes ago.
    assert (
        trust_for(instance, observed_at=stale_observed, confirmed_at=fresh_confirmed)
        == "verified"
    )


def test_confirmation_does_not_rescue_a_genuinely_stale_subject(
    instance: AppContext,
) -> None:
    now = instance.clock()
    horizon = timedelta(hours=instance.config.verify_horizon_hours)
    old = now - horizon - timedelta(hours=2)
    # Confirmed, but the confirmation itself aged out — a sweep stopped
    # reaching it. That is genuinely `stale`, and must still read as such.
    assert trust_for(instance, observed_at=old, confirmed_at=old) == "stale"


def test_no_evidence_at_all_is_unverified(instance: AppContext) -> None:
    assert trust_for(instance, observed_at=None) == "unverified"
    assert trust_for(instance, observed_at=None, confirmed_at=None) == "unverified"
