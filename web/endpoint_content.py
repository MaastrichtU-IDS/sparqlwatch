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
    sampled: bool
    run: str | None = None
    generated_at: str | None = None
    size: int | None = None
    truncated: bool | None = None
    classes: list[str] = field(default_factory=list)


def endpoint_content(store: Store, endpoint: str) -> EndpointContent:
    """Return what the most recent run that sampled ``endpoint`` found in it.

    Raises ValueError if two distinct runs tie for most recent on this
    endpoint, which means two run graphs carry the same
    prov:generatedAtTime. That is a corrupt store rather than a question with
    two answers, and picking one of them silently would hide it.
    """
    rows = list(
        store.query(_QUERY, substitutions={_ENDPOINT: NamedNode(endpoint)})
    )
    if not rows:
        return EndpointContent(endpoint=endpoint, sampled=False)

    runs = {row["run"].value for row in rows}
    if len(runs) > 1:
        raise ValueError(
            f"{endpoint} has {len(runs)} runs tied as most recent "
            f"({sorted(runs)}); two run graphs share a prov:generatedAtTime"
        )

    first = rows[0]
    return EndpointContent(
        endpoint=endpoint,
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
