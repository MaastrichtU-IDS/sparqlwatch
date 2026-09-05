"""What each metric has read for one endpoint, across every run in the store.

The reader behind the only view in this service that crosses runs. Everything
else answers "what is true now", from `urn:sparqlwatch:current`; this answers
"what has changed", from the run graphs themselves, which are the record and are
never rewritten.

WHAT A GAP MEANS HERE, because it is the thing most easily got wrong. A run that
said NOTHING about a metric is not a run that declined it:

  * a decline is a fact. The sweep ran, the metric was in its definition set,
    and it chose not to spend the request. That is `not-measured`, and this
    reader returns it as a cell with a reason.
  * a metric absent from a run's definition set produces no fact at all. The
    question was not being asked yet. Six of this store's metrics did not exist
    in its oldest run, so this is the common case rather than a corner.

Drawing those alike would tell a reader that a sweep declined a metric nobody
had written yet. Both are `None` cells here and the page draws them as gaps,
which is the reading the index already gives an empty cell: this run recorded
nothing, which is not a verdict about the endpoint.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from pyoxigraph import NamedNode, Store, Variable

_QUERY = (Path(__file__).resolve().parent / "queries" / "endpoint_history.rq").read_text()
_ENDPOINT = Variable("endpoint")


@dataclass(frozen=True)
class Reading:
    """What one run said about one metric.

    Exactly one of `verdict` and `reason` is set. A reading with neither is not
    constructed: the absence of a reading IS the gap, and a Reading object
    holding nothing would be an assertion that a run said something empty.
    """

    verdict: str | None = None
    reason: str | None = None


@dataclass
class MetricHistory:
    """One metric's readings, aligned to the run list, oldest first.

    `readings` is the same length as the history's `runs`, with `None` where
    that run said nothing. Aligned rather than sparse so a caller renders a row
    by zipping, and cannot accidentally draw five readings under six runs.
    """

    metric: str
    readings: list[Reading | None] = field(default_factory=list)

    @property
    def changed(self) -> bool:
        """Whether this metric ever read differently from one run to the next.

        Gaps are SKIPPED rather than counted as a change: a metric that did not
        exist in the oldest run and has read `verified` ever since has not
        changed, and saying it did would put every new metric on a list of
        things that moved.
        """
        seen = [r for r in self.readings if r is not None]
        return any(a != b for a, b in zip(seen, seen[1:]))


@dataclass
class EndpointHistory:
    """Every run that said anything about this endpoint, oldest first."""

    endpoint: str
    runs: list[str] = field(default_factory=list)
    metrics: list[MetricHistory] = field(default_factory=list)

    @property
    def has_history(self) -> bool:
        """Whether there is more than one run to compare.

        One run is not a history, and a page that drew a single-cell timeline
        would imply a trend from one observation.
        """
        return len(self.runs) > 1


def endpoint_history(store: Store, endpoint: str, limit: int = 30) -> EndpointHistory:
    """Read `endpoint`'s history, keeping the newest `limit` runs.

    Newest kept rather than oldest, because a reader looking at a timeline is
    asking what has happened lately. The list is returned oldest-first all the
    same, since that is the direction time runs and the direction a row reads.
    """
    rows = list(store.query(_QUERY, substitutions={_ENDPOINT: NamedNode(endpoint)}))

    runs: list[str] = []
    for row in rows:
        at = row["generatedAt"].value
        if at not in runs:
            runs.append(at)
    runs.sort()
    runs = runs[-limit:]
    index = {at: i for i, at in enumerate(runs)}

    by_metric: dict[str, list[Reading | None]] = {}
    for row in rows:
        slot = index.get(row["generatedAt"].value)
        if slot is None:
            continue
        metric = row["metric"].value
        readings = by_metric.setdefault(metric, [None] * len(runs))
        readings[slot] = Reading(
            verdict=row["verdict"].value if row["verdict"] is not None else None,
            reason=row["reason"].value if row["reason"] is not None else None,
        )

    return EndpointHistory(
        endpoint=endpoint,
        runs=runs,
        # Sorted by metric id, so two identical requests draw the same rows in
        # the same order: SPARQL solution order is not specified.
        metrics=[
            MetricHistory(metric=m, readings=by_metric[m]) for m in sorted(by_metric)
        ],
    )
