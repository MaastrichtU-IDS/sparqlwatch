"""The fleet over time, and the few numbers worth stating about it.

Two readers behind the index's overview. Both cross run graphs, like
`endpoint_history`, and for the same reason: `urn:sparqlwatch:current` names one
run per endpoint, so anything read through it is a snapshot.

WHY THE GRID IS ENDPOINTS BY RUNS and not a stacked bar of verdict counts per
run. This store holds two registries swept on overlapping dates: ten daily
sweeps of eight endpoints, and five sweeps of three others all on one day. A bar
per run would alternate between heights of eight and three and read as a fleet
that collapsed and recovered five times in an afternoon. A grid has somewhere
honest to put "this sweep did not cover this endpoint", which is a gap, and 70
of this store's 165 cells are exactly that.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from pyoxigraph import Store

_HISTORY = (Path(__file__).resolve().parent / "queries" / "fleet_history.rq").read_text()


@dataclass
class FleetRow:
    """One endpoint's availability across every run, aligned to the run list."""

    endpoint: str
    cells: list[str | None] = field(default_factory=list)

    @property
    def changed(self) -> bool:
        """Whether this endpoint ever read differently from one sweep to the next.

        Gaps are skipped, not counted as change: an endpoint first swept
        halfway through the record has not changed, and saying it did would put
        every newly registered endpoint on a list of things that moved.
        """
        seen = [c for c in self.cells if c is not None]
        return any(a != b for a, b in zip(seen, seen[1:]))


@dataclass
class FleetHistory:
    runs: list[str] = field(default_factory=list)
    rows: list[FleetRow] = field(default_factory=list)

    @property
    def has_history(self) -> bool:
        """One sweep is not a history, and a single-column grid implies a trend
        from one observation."""
        return len(self.runs) > 1

    @property
    def changed(self) -> list[FleetRow]:
        return [r for r in self.rows if r.changed]

    @property
    def steady(self) -> list[FleetRow]:
        """Every endpoint that read the same way in every sweep that saw it.

        Counted rather than drawn. A grid of 543 endpoints is 543 rows of which
        the great majority are one repeated state, and a reader scanning for
        what moved has to find it among them. The count is the fact worth
        stating; the rows are noise, and each one is still a row in the listing
        below with its own page.
        """
        return [r for r in self.rows if not r.changed]


@dataclass
class FleetStats:
    """The few numbers a watcher wants before reading anything else."""

    endpoints: int = 0
    sweeps: int = 0
    first_sweep: str | None = None
    last_sweep: str | None = None
    changed: int = 0
    # Summed across the endpoints something actually counted, WITH that count
    # beside it. A total presented without its denominator reads as the fleet's
    # size when it is the size of the part of the fleet anybody measured, and at
    # the default cheap ceiling that part is none of it.
    triples: int | None = None
    triples_from: int = 0


def fleet_history(store: Store, limit: int = 40) -> FleetHistory:
    """Every endpoint's availability, oldest sweep first.

    `limit` keeps the newest sweeps: a reader is asking what has happened
    lately, and dropping the recent end to keep ancient history backwards.
    """
    rows = list(store.query(_HISTORY))

    runs = sorted({r["generatedAt"].value for r in rows})[-limit:]
    slot = {at: i for i, at in enumerate(runs)}

    by_endpoint: dict[str, list[str | None]] = {}
    for row in rows:
        i = slot.get(row["generatedAt"].value)
        if i is None:
            continue
        cells = by_endpoint.setdefault(row["endpoint"].value, [None] * len(runs))
        cells[i] = row["verdict"].value

    return FleetHistory(
        runs=runs,
        # By endpoint url, which is what the listing below sorts by and what a
        # reader scanning for one is scanning for.
        rows=[FleetRow(endpoint=e, cells=by_endpoint[e]) for e in sorted(by_endpoint)],
    )


def fleet_stats(store: Store, history: FleetHistory, entries) -> FleetStats:
    """The overview numbers, from facts already read rather than re-queried.

    Takes the history and the index's own entries instead of asking the store
    again: three readers answering the same question three ways is three chances
    to disagree on one page.
    """
    counted = [
        v.observed_count
        for entry in entries
        for v in entry.verdicts
        if v.metric.endswith(":triple-count") and v.observed_count is not None
    ]
    return FleetStats(
        endpoints=len(entries),
        sweeps=len(history.runs),
        first_sweep=history.runs[0] if history.runs else None,
        last_sweep=history.runs[-1] if history.runs else None,
        changed=len(history.changed),
        triples=sum(counted) if counted else None,
        triples_from=len(counted),
    )
