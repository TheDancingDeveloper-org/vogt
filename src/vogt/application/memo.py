"""Share an expensive read between callers while the stores have not moved.

Some answers are pure functions of the two stores' contents: the shell's
badge counts (`place.metrics`) and the stored half of the Inbox are the ones
that matter (WI-1224). Every open tab and device asks for them on the same
event, and each one recomputed the full backlog and Inbox projection — about a
second of single-threaded Python apiece, so a burst of them queued behind each
other for ten to twenty seconds.

`StoreMemo` keeps one answer per pair of stores, keyed on a `StoreState` that
moves whenever either store's content could have: the declared revision (every
audited write), the newest event (an observed-side publish), and the observed
projection generation (every `rebuild_latest`). It is process-wide rather than
per context, because a context is built per request.

Concurrent callers share one computation. A caller that finds a computation
already running waits for it instead of starting its own, and takes its answer
if that computation read the stores *after the caller arrived* — which is all
an uncached read promises too, so it is no less fresh than computing again.
Without that rule a store that moves faster than the answer takes to compute
(agents edit work items all day) would make every waiter recompute in turn,
and a burst of eight badge reads would cost eight computations, serially. The
computation runs at most one at a time per pair of stores, which under the GIL
costs nothing: two parallel copies of a CPU-bound read take as long as two
serial ones.

An answer is computed over the stores as they are *after* the key was read, so
it is never older than its key. Another process writing the same data
directory (the CLI, a stdio MCP server) moves the same revision and generation,
so its writes are seen too.
"""

from __future__ import annotations

import threading
import time
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass
from typing import Generic, TypeVar

from vogt.application.context import AppContext

T = TypeVar("T")

#: Pairs of stores remembered per memo. A server has one; a test suite
#: builds thousands of throwaway instances, and they must not accumulate.
MEMO_STORES_MAX = 4


@dataclass(frozen=True)
class StoreState:
    """Everything a memoised answer's validity depends on."""

    instance_id: str
    revision: int
    latest_event_seq: int
    latest_generation: int
    #: The configuration the answer was computed under. Constant for a
    #: process; part of the key so two configurations over one data
    #: directory (tests do this) never share an answer.
    config: str


def store_state(ctx: AppContext) -> StoreState:
    """Read the key: four cheap point reads, no scans."""
    with ctx.declared.read() as view:
        instance_id = view.instance_id()
        revision = view.current_revision()
        latest_event_seq = view.latest_event_seq()
    return StoreState(
        instance_id=instance_id,
        revision=revision,
        latest_event_seq=latest_event_seq,
        latest_generation=ctx.observed.latest_generation(),
        config=ctx.config.model_dump_json(),
    )


def _stores_of(ctx: AppContext) -> tuple[str, str]:
    return (str(ctx.declared.path), str(ctx.observed.path))


@dataclass(frozen=True)
class _Answer(Generic[T]):
    state: StoreState
    #: `time.monotonic()` just before `state` was read.
    read_at: float
    value: T


class StoreMemo(Generic[T]):
    """One remembered answer per pair of stores, valid for one `StoreState`."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._flights: dict[tuple[str, str], threading.Lock] = {}
        self._answers: OrderedDict[tuple[str, str], _Answer[T]] = OrderedDict()

    def get(
        self,
        ctx: AppContext,
        compute: Callable[[], T],
        *,
        keep: Callable[[T], bool] = lambda _: True,
    ) -> T:
        """The remembered answer if the stores have not moved, else a new one.

        `keep` decides whether a fresh answer may be remembered; an answer
        built while part of it was unavailable should be asked for again
        rather than served until the next write.
        """
        arrived = time.monotonic()
        stores = _stores_of(ctx)
        with self._lock:
            flight = self._flights.setdefault(stores, threading.Lock())
        with flight:
            with self._lock:
                remembered = self._answers.get(stores)
            if (
                remembered is not None
                and remembered.read_at >= arrived
                and remembered.state.config == ctx.config.model_dump_json()
            ):
                # Computed by the caller this one waited behind, from stores
                # read after this one arrived.
                return remembered.value
            read_at = time.monotonic()
            state = store_state(ctx)
            if remembered is not None and remembered.state == state:
                return remembered.value
            answer = compute()
            if keep(answer):
                with self._lock:
                    self._answers[stores] = _Answer(state, read_at, answer)
                    self._answers.move_to_end(stores)
                    while len(self._answers) > MEMO_STORES_MAX:
                        dropped, _ = self._answers.popitem(last=False)
                        self._flights.pop(dropped, None)
            return answer

    def clear(self) -> None:
        """Forget every answer (tests, and a restore or clone)."""
        with self._lock:
            self._answers.clear()


__all__ = ["MEMO_STORES_MAX", "StoreMemo", "StoreState", "store_state"]
