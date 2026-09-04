"""Answers "what is in this endpoint?" from the store.

This is the first question the UI asks, because it is the first thing anyone
wanting to write a query against an unfamiliar endpoint needs: which classes
are in there, how many were found, and whether the list is complete.

All the selecting is done by web/queries/endpoint_content.rq. This module
turns its rows into one object and does no filtering of its own: if the query
returned another endpoint's values, or an older run's, filtering them out here
would leave the tests passing over a query that is still wrong, and the query
is what any other caller (a UI, a SPARQL console, a later HTTP layer) would
actually reuse.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from pyoxigraph import NamedNode, Store, Variable

from queries import read_query

_QUERY = read_query("endpoint_content")

# The variable the endpoint IRI is substituted for. See the .rq file's header
# on why substitution rather than string interpolation.
_ENDPOINT = Variable("endpoint")


CLASSES_METRIC = "urn:sparqlwatch:metric:classes"
CLASS_PROFILES_METRIC = "urn:sparqlwatch:metric:class-profiles"

# The metric ids whose sample is a list of CLASSES, current scheme first.
#
# It is a list and not a single id because retiring a metric does not retire
# the samples it already took. A store holding history has `classes` samples in
# its older run graphs and `class-profiles` samples in its newer ones, so a
# default pinned to either id answers wrongly for half the store.
#
# The ORDER decides ties only. Which sample is "the" sample is decided on the
# sample's own prov:generatedAtTime, because a store can hold a new-scheme run
# older than an old-scheme one and preferring the new id unconditionally would
# publish a stale list as the current one. The order matters because one
# transitional run publishes BOTH samples, giving them identical timestamps by
# construction; there the current scheme wins.
#
# A metric that samples something OTHER than classes must never be added here.
# web/tests/fixtures/run-properties-sample.nq is an endpoint whose only sample
# is of properties, and reporting those as its classes is the specific wrong
# answer test_another_metrics_sample_is_not_reported_as_classes pins.
CLASS_SAMPLING_METRICS = (CLASS_PROFILES_METRIC, CLASSES_METRIC)

_METRIC = Variable("metric")


@dataclass
class EndpointContent:
    """What the most recent run that sampled ``endpoint`` saw in it.

    ``sampled`` is the field that keeps this honest. An endpoint no run has
    sampled and an endpoint sampled to find nothing both have an empty
    ``classes``, and a UI that cannot tell them apart will state the second
    while meaning the first.

    ``sampled is False`` means exactly one thing: no run in this store
    published a class sample for this endpoint. It does NOT mean nobody
    looked. At least three situations reach it: a sample never attempted, a
    metric declined by the run's cost ceiling (which the prober records as a
    sw:NotMeasured fact), and a probe that ran and could not finish. The last
    is what the real fixture's qlever.dev/api/osm-planet is: its measurement
    row for sw:metric:classes carries dqv:value "indeterminate" with
    sw:elapsedMs "30003", the 30 second request budget running out. The graph
    distinguishes those cases on that measurement row, but this query does not
    read it, so this field does not report which one applied. Rendering
    ``sampled is False`` as "nobody has looked" states a confident wrong
    answer about an endpoint the monitor measured for thirty seconds.

    ``sampled is True`` with ``classes == []`` is a sample that reports a size
    and lists nothing. The prober does not emit one today: it skips a sample
    with no values on purpose, so that a size of 0 cannot be misread as "this
    endpoint has no classes". So that state is defence rather than a case seen
    in the wild, and web/tests/fixtures/run-zero-classes.nq is synthetic for
    exactly that reason.

    ``run`` and ``generated_at`` say which sweep the answer came from, so a
    reader can judge how current it is, and ``truncated`` says whether the
    sampler stopped short of the end, so nobody reads a cut-off list as the
    whole vocabulary.
    """

    endpoint: str
    # Which metric this is a sample OF. Added 2026-09-03, when the pointer became
    # per metric: without it a caller holding an EndpointContent cannot say what
    # it is a sample of, and two of them would be indistinguishable.
    #
    # NO DEFAULT, deliberately. A default would let a caller publish a sample
    # without saying what the sample is of, which is the whole thing this field
    # exists to prevent. It sits here rather than at the end because a field with
    # no default cannot follow one that has it.
    metric: str
    sampled: bool
    run: str | None = None
    generated_at: str | None = None
    size: int | None = None
    truncated: bool | None = None
    classes: list[str] = field(default_factory=list)


def endpoint_content(
    store: Store, endpoint: str, metric: str | None = None
) -> EndpointContent:
    """Return what the most recent run that sampled classes here found.

    ``metric`` names ONE sampling metric to ask about. Left unset, the newest
    sample across ``CLASS_SAMPLING_METRICS`` is the answer, which is what a
    caller asking "what classes are in this endpoint" means once more than one
    metric has sampled them over the store's history. See that constant for how
    the choice is made and why it is not a fixed preference.

    An endpoint no run sampled reports ``sampled is False`` under the current
    scheme's id, since that is the question that was asked and came back empty.

    Raises ValueError if two distinct runs tie for most recent on this
    endpoint, which means two run graphs carry the same
    prov:generatedAtTime. That is a corrupt store rather than a question with
    two answers, and picking one of them silently would hide it.
    """
    if metric is None:
        return _newest_across_metrics(store, endpoint)
    return _for_one_metric(store, endpoint, metric)


def _newest_across_metrics(store: Store, endpoint: str) -> EndpointContent:
    """The newest class sample for ``endpoint``, whichever metric took it.

    One query per candidate metric rather than one query binding ?metric free,
    because the free form would also return a sample from a metric that
    samples something other than classes, and there is no predicate in the
    graph that says "this metric samples classes" for it to filter on.
    """
    found = [
        content
        for content in (_for_one_metric(store, endpoint, m) for m in CLASS_SAMPLING_METRICS)
        if content.sampled
    ]
    if not found:
        return EndpointContent(
            endpoint=endpoint, metric=CLASS_SAMPLING_METRICS[0], sampled=False
        )
    # Lexicographic on the timestamp is chronological: every generatedAtTime the
    # prober writes is xsd:dateTime in UTC with a Z suffix, one fixed width. The
    # index is the documented tie-break, negated so a lower index (the current
    # scheme) sorts higher.
    return max(
        found,
        key=lambda c: (c.generated_at, -CLASS_SAMPLING_METRICS.index(c.metric)),
    )


def _for_one_metric(store: Store, endpoint: str, metric: str) -> EndpointContent:
    """``endpoint_content`` for exactly one sampling metric."""
    rows = list(
        store.query(
            _QUERY,
            substitutions={
                _ENDPOINT: NamedNode(endpoint),
                _METRIC: NamedNode(metric),
            },
        )
    )
    if not rows:
        return EndpointContent(endpoint=endpoint, metric=metric, sampled=False)

    runs = {row["run"].value for row in rows}
    if len(runs) > 1:
        raise ValueError(
            f"{endpoint}'s {metric} sample has {len(runs)} runs tied as most recent "
            f"({sorted(runs)}); two run graphs share a prov:generatedAtTime"
        )

    first = rows[0]
    return EndpointContent(
        endpoint=endpoint,
        metric=metric,
        sampled=True,
        run=first["run"].value,
        generated_at=first["generatedAt"].value,
        size=int(first["size"].value),
        # xsd:boolean's lexical space is "true"/"false"/"1"/"0"; the prober
        # writes the words, but accepting the digits costs nothing and is
        # what the datatype says.
        truncated=first["truncated"].value in ("true", "1"),
        # Sorted so a caller rendering the list gets a stable order: SPARQL
        # solution order is not specified, and an unstable list looks like
        # the endpoint changed between two identical questions.
        classes=sorted(
            row["class"].value for row in rows if row["class"] is not None
        ),
    )
