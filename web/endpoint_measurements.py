"""Answers "what are this endpoint's verdicts?" from the store.

This is the second question a page for an endpoint needs, right after "what
is in it" (web/endpoint_content.py): which metrics did the most recent run
verify, decline, or find indeterminate, and how long did each take.

All the selecting is done by web/queries/endpoint_measurements.rq. This
module turns its rows into one object and does no filtering of its own: if
the query returned another endpoint's rows, or an older run's, filtering them
out here would leave the tests passing over a query that is still wrong, and
the query is what any other caller (a UI, a SPARQL console, a later HTTP
layer) would actually reuse.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from pyoxigraph import NamedNode, Store, Variable

from queries import read_query

_QUERY = read_query("endpoint_measurements")

# The variable the endpoint IRI is substituted for. See the .rq file's header
# on why substitution rather than string interpolation.
_ENDPOINT = Variable("endpoint")


@dataclass
class MetricVerdict:
    """One metric's outcome in the run this endpoint's answer came from.

    ``level`` and ``elapsed_ms`` are ``None`` when the graph does not carry
    one: today only sw:metric:service-description carries a conformance
    level, and a metric whose measurement predates sw:elapsedMs (none do
    today, but the query does not assume otherwise) would have no elapsed
    time either.
    """

    metric: str
    verdict: str
    level: int | None = None
    elapsed_ms: int | None = None


@dataclass
class DeclinedMetric:
    """A metric the run recorded as sw:NotMeasured rather than measuring.

    This is not a missing metric: the run said so rather than staying silent.
    ``reason`` is what the graph gives for the decline: "cost-ceiling" when the
    run looked at its cost budget and chose not to run the metric, or
    "prober-failed" when the prober itself never got to ask, so there was no
    observation at all rather than an inconclusive one. A declined metric never appears in
    ``EndpointMeasurements.verdicts`` too: the two lists are a partition of
    what the run recorded for this endpoint, not overlapping views of it.
    """

    metric: str
    reason: str


@dataclass
class EndpointMeasurements:
    """What the most recent run that recorded anything for ``endpoint`` saw.

    ``assessed`` is the field that keeps this honest, the same way
    ``EndpointContent.sampled`` does for content samples. An endpoint no run
    has ever mentioned and an endpoint whose metrics were all declined both
    "measured nothing" in the sense of an empty ``verdicts`` list, and a UI
    that cannot tell those apart will report the second as if the endpoint
    had never been looked at.

    ``assessed is False`` means no run in this store recorded a measurement
    or a decline for this endpoint at all: nobody has ever run the sweep
    against it, or it is not in the registry.

    ``assessed is True`` with ``verdicts == []`` and ``declined`` non-empty is
    a run that looked at this endpoint and chose to run nothing on it (every
    applicable metric declined). That is a real, reportable answer, not the
    same as ``assessed is False``.

    ``run`` and ``generated_at`` say which sweep the answer came from, the
    same way as ``EndpointContent``.
    """

    endpoint: str
    assessed: bool
    run: str | None = None
    generated_at: str | None = None
    verdicts: list[MetricVerdict] = field(default_factory=list)
    declined: list[DeclinedMetric] = field(default_factory=list)


def endpoint_measurements(store: Store, endpoint: str) -> EndpointMeasurements:
    """Return the verdicts (and declines) of the most recent run that
    recorded anything for ``endpoint``.

    Raises ValueError if two distinct runs tie for most recent on this
    endpoint, which means two run graphs carry the same
    prov:generatedAtTime. That is a corrupt store rather than a question with
    two answers, and picking one of them silently would hide it.
    """
    rows = list(
        store.query(_QUERY, substitutions={_ENDPOINT: NamedNode(endpoint)})
    )
    if not rows:
        return EndpointMeasurements(endpoint=endpoint, assessed=False)

    runs = {row["run"].value for row in rows}
    if len(runs) > 1:
        raise ValueError(
            f"{endpoint} has {len(runs)} runs tied as most recent "
            f"({sorted(runs)}); two run graphs share a prov:generatedAtTime"
        )

    first = rows[0]
    verdicts: list[MetricVerdict] = []
    declined: list[DeclinedMetric] = []
    for row in rows:
        if row["reason"] is not None:
            declined.append(
                DeclinedMetric(
                    metric=row["metric"].value,
                    reason=row["reason"].value,
                )
            )
        else:
            verdicts.append(
                MetricVerdict(
                    metric=row["metric"].value,
                    verdict=row["verdict"].value,
                    level=(
                        int(row["level"].value)
                        if row["level"] is not None
                        else None
                    ),
                    elapsed_ms=(
                        int(row["elapsedMs"].value)
                        if row["elapsedMs"] is not None
                        else None
                    ),
                )
            )

    return EndpointMeasurements(
        endpoint=endpoint,
        assessed=True,
        run=first["run"].value,
        generated_at=first["generatedAt"].value,
        # Sorted by metric id so a caller rendering the list gets a stable
        # order: SPARQL solution order is not specified, and an unstable list
        # looks like the endpoint's verdicts changed between two identical
        # questions.
        verdicts=sorted(verdicts, key=lambda v: v.metric),
        declined=sorted(declined, key=lambda d: d.metric),
    )
