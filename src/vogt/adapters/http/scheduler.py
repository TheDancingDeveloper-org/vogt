"""The in-process collector schedule (FR-L3).

`sweep` is triggerable on demand from all three transports. This is the other
half: while `serve` is running, collectors also run on their own, so an
instance nobody has poked at is not an instance with no evidence.

That is the requirement's point rather than a convenience. Stale evidence and
no evidence look identical from the outside — both render as an empty view —
and the whole observed-first model rests on being able to tell them apart.
Freshness (FR-V4) makes the difference *visible*; the schedule is what keeps
it small.

Three properties this deliberately has:

**One sweep at a time.** A sweep that overruns its interval must not have a
second one start on top of it. The loop sleeps *after* the work finishes, so
the interval is a gap between sweeps rather than a fixed drumbeat, and a slow
estate degrades to "continuously sweeping" rather than to overlapping writers.

**A failed sweep never stops the schedule.** A collector that raises is
already recorded as a failed sweep with a partial outcome, which is what
freshness reads. Letting the exception kill the loop would turn one bad
collector into permanent silence — the failure mode the schedule exists to
prevent, arriving by the back door.

**It runs as the machine, not as a person.** A scheduled sweep has no human
behind it, so `serve` hands it a service principal. A sweep writes evidence
rather than declared rows, so today this leaves no audit record to read — but
the principal is where attribution is decided, and getting it right now means
a collector that one day does write a declared row is already correct rather
than stamped with whoever started the process.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
from collections.abc import AsyncIterator, Callable

from vogt.application.context import AppContext
from vogt.application.models import SweepParams

logger = logging.getLogger("vogt.scheduler")

#: What a scheduled sweep records as its reason. Every write carries one
#: (FR-W1), and "the schedule ran" is the honest answer here — there is no
#: person to name.
SCHEDULED_REASON = "scheduled sweep (FR-L3)"


class CollectorSchedule:
    """Runs sweeps in the background for as long as the server is up."""

    def __init__(
        self,
        context_factory: Callable[[], AppContext],
        *,
        interval_seconds: int,
    ) -> None:
        self._context_factory = context_factory
        self._interval = interval_seconds
        self._task: asyncio.Task[None] | None = None
        self._stopping = asyncio.Event()
        #: Counted so tests can assert the loop ran without waiting on a wall
        #: clock, and so `/health/ready` could report it later.
        self.completed = 0
        self.failed = 0

    @property
    def enabled(self) -> bool:
        return self._interval > 0

    async def start(self) -> None:
        if not self.enabled:
            logger.info("collector schedule disabled (sweep_interval_seconds=0)")
            return
        self._stopping.clear()
        self._task = asyncio.create_task(self._loop(), name="vogt-collector-schedule")
        logger.info("collector schedule started; every %ss", self._interval)

    async def stop(self) -> None:
        """Ask the loop to finish, and wait for the sweep in flight.

        Waiting matters: a sweep is a writer, and cancelling one mid-write
        would leave a started sweep with no outcome — which reads downstream
        as a collector that hung rather than one that was shut down.
        """
        self._stopping.set()
        if self._task is None:
            return
        self._task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await self._task
        self._task = None

    async def _loop(self) -> None:
        while not self._stopping.is_set():
            await self.run_once()
            with contextlib.suppress(TimeoutError):
                # Sleeping on the stop event rather than on the clock, so
                # shutdown does not wait out a fifteen-minute interval.
                await asyncio.wait_for(self._stopping.wait(), timeout=self._interval)

    async def run_once(self) -> None:
        """One sweep, off the event loop so it cannot block the listener.

        Collectors do blocking file and network I/O and SQLite writes. Running
        them inline would stall every request for the duration of a sweep,
        which on a large estate is the difference between a service and a
        stall.
        """
        try:
            await asyncio.to_thread(self._sweep)
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self.failed += 1
            # Logged, not raised: see the module docstring. The sweep's own
            # failure is already recorded as evidence.
            logger.warning("scheduled sweep failed: %s", error)
        else:
            self.completed += 1

    def _sweep(self) -> None:
        from vogt.application.services import sweep

        ctx = self._context_factory()
        sweep(ctx, SweepParams(reason=SCHEDULED_REASON))


@contextlib.asynccontextmanager
async def scheduled(schedule: CollectorSchedule) -> AsyncIterator[CollectorSchedule]:
    """Run a schedule for the lifetime of the block."""
    await schedule.start()
    try:
        yield schedule
    finally:
        await schedule.stop()


__all__ = ["SCHEDULED_REASON", "CollectorSchedule", "scheduled"]
