"""`contract-checker` — evaluated when asked, and only when asked.

FR-O1 lists this among the core collectors, with one difference from the
others: **it runs on demand only.** Nothing re-checks compliance on a timer
(r3, FR-G5 deferred). The contract is evaluated when somebody asks, and the
answer is reported with its age like every other value in the system — a
three-week-old `compliant` is honest in a way a silently refreshed one is
not (DESIGN §5).

Being a collector at all is what makes the result evidence: it lands in the
observed store with a subject key and a timestamp, and the project's
compliance status is a projection of it.
"""

from __future__ import annotations

from collections.abc import Iterable
from pathlib import Path

from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.contract import configured_contract, evaluate
from vogt.core.entities import Project

KIND_CONTRACT = "contract.check"


class ContractCheckerCollector:
    """Evaluates the project contract against a registered project."""

    #: Read by the sweeper's default selection: a collector that opts out of
    #: scheduled sweeps runs only when named. Without this, "nothing
    #: re-checks on a timer" would depend on nobody adding it to a schedule.
    on_demand_only = True

    @property
    def name(self) -> str:
        return "contract-checker"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        result = evaluate(Path(project.root_path), configured_contract(ctx.config))
        yield finding(
            kind=KIND_CONTRACT,
            subject_key=f"contract:{project.slug}",
            project=project,
            payload={
                "contract_version": result.contract_version,
                "status": result.status,
                "failing": [
                    {"rule": c.rule, "target": c.target, "detail": c.detail}
                    for c in result.failing
                ],
                # FR-G3: the rules *evaluated*, not only the ones that failed.
                # "AGENTS.md is missing" is only actionable if you can also
                # see that AGENTS.md was one of the things being looked for.
                "evaluated": [
                    {"rule": c.rule, "target": c.target, "satisfied": c.satisfied}
                    for c in result.criteria
                ],
            },
        )
