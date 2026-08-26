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

    The seven fields after them say whether that sweep, and the newest sweep
    in the store, actually finished. They are the raw facts stage 1c-b4's prober
    writes, not a conclusion: ``run_did_not_finish`` and
    ``newer_run_did_not_reach_this_endpoint`` below are the two conclusions,
    and they are kept separate from the facts so that the RDF representation
    can serve the same facts and reach the same conclusions without this
    module's help. See web/queries/endpoint_measurements.rq's header.
    """

    endpoint: str
    assessed: bool
    run: str | None = None
    generated_at: str | None = None
    verdicts: list[MetricVerdict] = field(default_factory=list)
    declined: list[DeclinedMetric] = field(default_factory=list)

    # This run's own two run-level facts. ``emission`` is None for a run from
    # before stage 1c-b4, which promised nothing about finishing; ``finalised``
    # is False both for a run that did not finish and for a run that never
    # promised to say, which is why neither can be read on its own.
    emission: str | None = None
    finalised: bool = False

    # The newest run in the store, which is routinely not ``run``: see the
    # query header for why this one selection is not per-endpoint.
    # ``newest_completed_this_endpoint`` is True when that run recorded a
    # sw:completedEndpoint naming this endpoint, which is a published fact
    # rather than an inference from the presence of its measurements.
    newest_run: str | None = None
    newest_generated_at: str | None = None
    newest_emission: str | None = None
    newest_finalised: bool = False
    newest_completed_this_endpoint: bool = False

    @property
    def run_did_not_finish(self) -> bool:
        """The run whose facts these are stopped before writing its footer.

        Both halves are required. ``emission`` present is the run saying it is
        written as a header, then chunks, then a footer, so its missing
        ``finalised`` is a fact about that run rather than a gap in the
        vocabulary. Reading the missing ``finalised`` alone would mark every
        run captured before stage 1c-b4 as crashed, which is every historical
        run a production store keeps.
        """
        return self.emission is not None and not self.finalised

    @property
    def newer_run_did_not_reach_this_endpoint(self) -> bool:
        """A run newer than ``run`` stopped before it got to this endpoint.

        This is the case a per-endpoint question cannot see, and the one a
        crash produces for every endpoint after the one it died on: that run
        recorded nothing here, so ``verdicts`` and ``declined`` are an older
        run's and they are complete and current as far as this endpoint's own
        facts go.

        All five conjuncts below are required, and they are not
        interchangeable. ``newest_run is not None`` is first because
        ``None != self.run`` is True, so an absent newest run would otherwise
        read as "a run other than this one".
        ``newest_run != run`` is what stops this firing on a finished run's own
        page, where the newest run in the store IS the run being shown.
        ``newest_emission is not None`` is the same requirement as above, for
        the same reason, and it is the one a store of two finished historical
        runs turns on: see test_a_newer_historical_run_is_not_a_crash.
        ``newest_completed_this_endpoint`` is what makes the
        claim a fact: an endpoint the run did finish is one whose facts are
        simply older than the crash, and saying the run never reached it would
        be false.

        Three of the five cannot be reached one at a time through a real run
        file, because the query binds ?newestRun and ?newestEmission in one
        OPTIONAL and a chunk's sw:completedEndpoint is in the same chunk as
        its measurements. They are pinned on this dataclass instead, one test
        per conjunct, in test_endpoint_measurements.py.
        """
        return (
            self.newest_run is not None
            and self.newest_run != self.run
            and self.newest_emission is not None
            and not self.newest_finalised
            and not self.newest_completed_this_endpoint
        )


def _value(row, name: str) -> str | None:
    """One binding's lexical value, or None where the row does not bind it.

    Every run-level fact this query returns is optional in some real store, so
    each one needs the same two-line guard; writing it once keeps the
    constructor call below readable as a list of facts.
    """
    term = row[name]
    return None if term is None else term.value


def endpoint_measurements(store: Store, endpoint: str) -> EndpointMeasurements:
    """Return the verdicts (and declines) of the most recent run that
    recorded anything for ``endpoint``.

    Raises ValueError if two distinct runs tie for most recent on this
    endpoint, which means two run graphs carry the same
    prov:generatedAtTime. That is a corrupt store rather than a question with
    two answers, and picking one of them silently would hide it. The same
    check is made on the newest run in the store, which the query selects the
    same way and which a tie would also make ambiguous; the two ties are
    different stores, because the second can be produced by two runs neither
    of which touched this endpoint.
    """
    rows = list(
        store.query(_QUERY, substitutions={_ENDPOINT: NamedNode(endpoint)})
    )
    if not rows:
        return EndpointMeasurements(endpoint=endpoint, assessed=False)
    return measurements_from_rows(endpoint, rows)


def measurements_from_rows(endpoint: str, rows: list) -> EndpointMeasurements:
    """Turn one endpoint's solutions into one ``EndpointMeasurements``.

    Split out of the function above so that web/endpoint_index.py can build the
    same object from web/queries/index.rq's rows, which are the same columns
    asked of every endpoint at once. The index and the endpoint page are two
    renderings of one question, and this is the one place that decides what a
    row means, so they cannot come to disagree about a verdict, about which run
    a fact came from, or about whether that run finished.

    ``rows`` must be non-empty and must all be about ``endpoint``: an empty list
    is "not assessed", which is a different answer, and the caller is the one
    that knows whether nothing came back or nothing was asked.
    """
    runs = {row["run"].value for row in rows}
    if len(runs) > 1:
        raise ValueError(
            f"{endpoint} has {len(runs)} runs tied as most recent "
            f"({sorted(runs)}); two run graphs share a prov:generatedAtTime"
        )

    # The newest-run branch is an OPTIONAL, so it is either unbound on every
    # row or bound to the same run on every row, unless two run graphs tie for
    # newest in the whole store. That tie multiplies the rows above, so it is
    # checked rather than resolved: it is the same corrupt store as the tie
    # checked above, reached from a different direction.
    newest_runs = {
        row["newestRun"].value
        for row in rows
        if row["newestRun"] is not None
    }
    if len(newest_runs) > 1:
        raise ValueError(
            f"{len(newest_runs)} runs tie as the newest in this store "
            f"({sorted(newest_runs)}); two run graphs share a "
            f"prov:generatedAtTime"
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
        emission=_value(first, "emission"),
        # Read as "the store holds this quad", not as "the quad's literal is
        # true". sw:finalised is written once, as the last line of a finished
        # run, and only ever as true, so its presence is the fact; a
        # hypothetical sw:finalised false would mean the same thing as its
        # absence and must not read as "finished".
        finalised=first["finalised"] is not None,
        newest_run=_value(first, "newestRun"),
        newest_generated_at=_value(first, "newestGeneratedAt"),
        newest_emission=_value(first, "newestEmission"),
        newest_finalised=first["newestFinalised"] is not None,
        newest_completed_this_endpoint=first["newestCompleted"] is not None,
        # Sorted by metric id so a caller rendering the list gets a stable
        # order: SPARQL solution order is not specified, and an unstable list
        # looks like the endpoint's verdicts changed between two identical
        # questions.
        verdicts=sorted(verdicts, key=lambda v: v.metric),
        declined=sorted(declined, key=lambda d: d.metric),
    )
