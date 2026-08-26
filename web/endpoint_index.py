"""Answers "what does this service know about every endpoint?" from the store.

This is the question the index page asks, and it is web/endpoint_measurements.py
asked of the whole registry rather than of one endpoint. All the selecting is
done by web/queries/index.rq, which is deliberately the same query shape as
web/queries/endpoint_measurements.rq with the ?endpoint substitution taken out;
this module does no filtering of its own, for the reason that module's header
gives, and it does no grouping either.

WHAT IT RETURNS IS THE SAME OBJECT the endpoint page reads: one
EndpointMeasurements per endpoint, built by that module's
``measurements_from_rows``. That is the load-bearing part of this file. The index
draws 543 rows of the same facts the endpoint page draws one row of, and if the
two had their own notion of what a row means then a chip on the index could
contradict the page it links to. Sharing the constructor means the only way they
can differ is if the two queries select different runs, which is why the run
selection in both is the same pointer read out of the derived
urn:sparqlwatch:current graph and the same newest-run subquery.

WHAT IS NOT HERE. No grouping by availability, no metric ordering, no
abbreviations, no counting: those are decisions about how the page reads, they
are made in web/app.py where the rest of the page's view model is built, and
they are tested there. This module answers the question; the page decides how to
say it.

There is no LIMIT and no pagination. The derived graph is O(endpoints) rather
than O(endpoints x history), so the whole registry is a flat scan: 543 endpoints
and 4,344 rows measured 146 ms on this machine, and that number does not grow as
sweeps accumulate. See web/queries/index.rq's header for the shapes that keep it
flat and what the slow ones cost.
"""

from __future__ import annotations

from pyoxigraph import Store

from endpoint_measurements import EndpointMeasurements, measurements_from_rows
from queries import read_query

_QUERY = read_query("index")


def endpoint_index(store: Store) -> list[EndpointMeasurements]:
    """Every endpoint the derived graph holds facts for, in endpoint order.

    Sorted by endpoint IRI, because SPARQL solution order is not specified and
    an index whose rows moved between two identical requests would look like the
    registry had changed. The page groups these rows afterwards and the sort
    survives inside each group.

    Raises ValueError on a store where two run graphs share a
    prov:generatedAtTime, naming the endpoint the tie was found on: that is a
    corrupt store rather than a question with two answers, and it is the same
    refusal web/endpoint_measurements.py makes on the same store, reached from
    the same check. Refusing the whole index rather than dropping the one
    ambiguous endpoint is deliberate: a page that quietly listed 542 of 543
    would report the corruption as an endpoint that has gone away.

    An endpoint the store holds no facts for is not in the list at all, rather
    than present with ``assessed`` False. ``assessed`` False is the answer to
    "what do you know about THIS endpoint", which is a question somebody asked
    about a named endpoint; nobody asks it of the index, and a row for an
    endpoint no run has mentioned would be a row this service cannot say
    anything about.
    """
    by_endpoint: dict[str, list] = {}
    for row in store.query(_QUERY):
        by_endpoint.setdefault(row["endpoint"].value, []).append(row)

    return [
        measurements_from_rows(endpoint, rows)
        for endpoint, rows in sorted(by_endpoint.items())
    ]
