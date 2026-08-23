"""The HTTP surface: one endpoint resource, served as HTML or as RDF.

This is the first thing this project serves over HTTP. It exposes exactly
one resource, the current state of one monitored SPARQL endpoint, in two
representations chosen by the request's Accept header. The design spec
requires content negotiation on every resource ("a quality-measurement
service that is not itself machine-readable would be self-defeating"), so
the HTML and the RDF are two representations of one resource rather than two
resources.

The two representations are derived differently on purpose, and that is
worth stating plainly because it is a hazard as well as a design:

  * HTML comes from the SELECT queries, through endpoint_measurements.py and
    endpoint_content.py, which turn rows into Python objects a template can
    walk.
  * RDF comes from queries/endpoint_description.rq, a CONSTRUCT, serialised
    straight out of the store. Nothing rebuilds a triple in Python.

Building the RDF from the SELECT rows instead would mean re-deriving the
graph from a flattened copy of itself, which is precisely where two
representations of one resource start to disagree. Keeping the CONSTRUCT
means the RDF cannot state anything the store does not already hold. It also
means the agreement test in web/tests/test_negotiation.py compares two
genuinely independent derivations rather than two views of one list.

What is deliberately NOT here yet: the real page. Task 3 of this stage owns
web/templates/endpoint.html, the verdict encoding and the styling. The HTML
below is the minimum the negotiation tests need, and it carries the
data-metric / data-verdict / data-declined attributes that the agreement
test reads. Those attributes are a contract: a template that drops them
leaves this service with no way to check that what a person is shown matches
what a machine is served.
"""

from __future__ import annotations

import html
import os
from functools import lru_cache

from fastapi import Depends, FastAPI, Query, Request, Response
from pyoxigraph import NamedNode, RdfFormat, Store, Variable, serialize

from endpoint_content import EndpointContent, endpoint_content
from endpoint_measurements import EndpointMeasurements, endpoint_measurements
from queries import read_query

# ---------------------------------------------------------------------------
# The URL shape
# ---------------------------------------------------------------------------
#
# The thing being identified is itself a URL, so it has to travel either
# percent-encoded inside the path (/endpoint/https%3A%2F%2Fexample.org%2Fsparql)
# or as a query parameter (/endpoint?url=https%3A%2F%2Fexample.org%2Fsparql).
# This service uses the query parameter, for two measured reasons.
#
# First, the path form is decoded twice and therefore lossy. I checked, with
# Starlette 1.6.0 and this test client: a request for
# /e/https%253A%252F%252Fdata.kkg.kadaster.nl%252Fquery, which is the correct
# single encoding of the literal text "https%3A%2F%2Fdata.kkg.kadaster.nl%2Fquery",
# arrives in a {url:path} parameter as "https://data.kkg.kadaster.nl/query".
# The ASGI server unquotes the path and the path converter's value is
# unquoted again, so any endpoint URL containing a percent sequence of its
# own comes back as a different URL. The same request against the query
# parameter yields the percent sequence intact: a query string is unquoted
# exactly once, so the endpoint URL round-trips byte for byte.
#
# Second, an encoded slash in a path is not reliably deliverable. Apache's
# AllowEncodedSlashes defaults to Off (it answers 404), and nginx normalises
# and merges path slashes by default, so the resource would stop resolving
# behind a front end this project does not control, while the query string
# is passed through untouched.
#
# What that costs: the resource IRI carries a query string, so it is less
# pretty than a path and it cannot be extended by appending path segments
# (a future /endpoint/{...}/history has to become another parameter or
# another route). Caches key on the full query string, which is correct
# behaviour and not a cost. Losing the endpoint URL is a correctness bug;
# an unpretty IRI is a matter of taste, so the taste loses.
ENDPOINT_PATH = "/endpoint"

# ---------------------------------------------------------------------------
# Representations
# ---------------------------------------------------------------------------
HTML_MEDIA_TYPE = "text/html"

# Every RDF media type pyoxigraph 0.5.9 can serialise a triple stream into
# and parse back, verified by round-tripping this query's output through each
# one. The CONSTRUCT yields triples, so the dataset formats (N-Quads, TriG)
# are not offered: they would serialise the same triples into a default
# graph and imply, wrongly, that the run graph structure was preserved.
RDF_MEDIA_TYPES = (
    "text/turtle",
    "application/n-triples",
    "application/rdf+xml",
    "application/ld+json",
)

# In server preference order, HTML first. This is what decides `Accept: */*`
# and a missing Accept, and the choice is a choice rather than a deduction:
# "anything" genuinely does permit Turtle. HTML wins because a person
# exploring with curl sends exactly `*/*` (curl 8.7.1 does; the claim that a
# bare curl sends no Accept header at all is simply false) and is better
# served by something legible than by Turtle. A machine that wants RDF says
# so, and this file's whole job is to honour that when it does.
OFFERED_MEDIA_TYPES = (HTML_MEDIA_TYPE,) + RDF_MEDIA_TYPES


def _parse_accept(header: str) -> list[tuple[str, str, float]]:
    """Split an Accept header into (type, subtype, q) triples.

    Malformed entries are skipped rather than raising: an Accept header is
    client input, and a request with one unparseable entry among several
    still expressed a usable preference. A missing or unparseable q defaults
    to 1.0, as RFC 9110 requires.
    """
    parsed: list[tuple[str, str, float]] = []
    for entry in header.split(","):
        parts = entry.split(";")
        media_range = parts[0].strip().lower()
        if "/" not in media_range:
            continue
        type_, _, subtype = media_range.partition("/")
        if not type_ or not subtype:
            continue
        quality = 1.0
        for parameter in parts[1:]:
            name, _, value = parameter.partition("=")
            if name.strip().lower() != "q":
                continue
            try:
                quality = float(value.strip())
            except ValueError:
                quality = 1.0
        parsed.append((type_, subtype, max(0.0, min(1.0, quality))))
    return parsed


def _quality_for(media_type: str, ranges: list[tuple[str, str, float]]) -> float:
    """The q value an Accept header assigns to one of our representations.

    The q comes from the MOST SPECIFIC matching range, not from the highest
    one, which is what makes `*/*;q=1, text/html;q=0` mean "anything except
    HTML" rather than "anything". Reading it as the highest match would turn
    an explicit refusal into an offer.
    """
    type_, _, subtype = media_type.partition("/")
    best_specificity = -1
    quality = 0.0
    for range_type, range_subtype, range_quality in ranges:
        if range_type == "*" and range_subtype == "*":
            specificity = 0
        elif range_type == type_ and range_subtype == "*":
            specificity = 1
        elif range_type == type_ and range_subtype == subtype:
            specificity = 2
        else:
            continue
        if specificity > best_specificity:
            best_specificity = specificity
            quality = range_quality
    return quality


def choose_representation(accept: str | None) -> str | None:
    """Pick the media type to serve, or None if we can serve none of them.

    None means 406. Falling back to HTML there would hand a client that
    asked only for JSON a document it cannot read while telling it the
    request succeeded, and a wrong answer delivered confidently is worse
    than a refusal.
    """
    if accept is None or not accept.strip():
        # No preference expressed at all, so the same reasoning as `*/*`
        # applies: serve the legible one.
        return HTML_MEDIA_TYPE

    ranges = _parse_accept(accept)
    best_media_type: str | None = None
    best_quality = 0.0
    # OFFERED_MEDIA_TYPES is in server preference order and the comparison is
    # strict, so a tie (which is what `*/*` and `text/*` produce) is settled
    # by our preference and lands on HTML.
    for media_type in OFFERED_MEDIA_TYPES:
        quality = _quality_for(media_type, ranges)
        if quality > best_quality:
            best_quality = quality
            best_media_type = media_type
    return best_media_type


# ---------------------------------------------------------------------------
# The store, injected
# ---------------------------------------------------------------------------
STORE_PATH_VARIABLE = "SPARQLWATCH_STORE"


@lru_cache(maxsize=None)
def _opened_store(path: str) -> Store:
    """One Store object per path per process.

    Not a convenience: an on-disk Oxigraph store is a RocksDB database and
    cannot be opened twice at once, so opening it per request would fail
    under the second concurrent request. Caching here rather than at import
    time keeps the open lazy, which is what lets the tests replace this
    dependency without ever touching a real store path.
    """
    return Store(path)


def get_store() -> Store:
    """The store dependency.

    Nothing opens a store when this module is imported. Tests override this
    with `app.dependency_overrides[get_store]` so each test gets its own
    store: a module-level open would make every test share one, and then the
    order they ran in would start to change their results.
    """
    path = os.environ.get(STORE_PATH_VARIABLE)
    if not path:
        raise RuntimeError(
            f"no store to read: set {STORE_PATH_VARIABLE} to the path of a "
            "store built by web/load_run.py"
        )
    return _opened_store(path)


# ---------------------------------------------------------------------------
# The resource
# ---------------------------------------------------------------------------
_DESCRIPTION_QUERY = read_query("endpoint_description")
_ENDPOINT_VARIABLE = Variable("endpoint")

app = FastAPI(
    title="sparqlwatch",
    description="What this service observed of public SPARQL endpoints.",
)


def _endpoint_rdf(store: Store, endpoint: str, media_type: str) -> bytes:
    """Serialise the endpoint's facts, straight from the store."""
    triples = store.query(
        _DESCRIPTION_QUERY,
        substitutions={_ENDPOINT_VARIABLE: NamedNode(endpoint)},
    )
    return serialize(triples, format=RdfFormat.from_media_type(media_type))


def _endpoint_html(
    endpoint: str,
    measurements: EndpointMeasurements,
    content: EndpointContent,
) -> str:
    """The placeholder page. Task 3 replaces this with a real template.

    It states only what the store supports: one row per measured metric, one
    per declined metric, and the class sample's size and truncation flag in
    words rather than a list, because a list here would be the first step
    towards rendering a truncated sample as a complete vocabulary. The
    data- attributes are the contract described in the module docstring.
    """
    name = html.escape(endpoint)
    rows = []
    for verdict in measurements.verdicts:
        rows.append(
            f'    <li data-metric="{html.escape(verdict.metric)}" '
            f'data-verdict="{html.escape(verdict.verdict)}">'
            f"{html.escape(verdict.metric)}: {html.escape(verdict.verdict)}</li>"
        )
    for declined in measurements.declined:
        rows.append(
            f'    <li data-metric="{html.escape(declined.metric)}" '
            f'data-declined="{html.escape(declined.reason)}">'
            f"{html.escape(declined.metric)}: not measured "
            f"({html.escape(declined.reason)})</li>"
        )

    if content.sampled:
        sample = (
            f"{content.size} classes sampled"
            + (
                ", truncated: more may exist beyond the limit"
                if content.truncated
                else ""
            )
        )
    else:
        sample = "no class sample from this run"

    return "\n".join(
        [
            f"<title>{name}</title>",
            f"<h1>{name}</h1>",
            f'<p data-run="{html.escape(measurements.run or "")}">'
            f'{html.escape(measurements.generated_at or "no run")}</p>',
            "  <ul>",
            *rows,
            "  </ul>",
            f'<p data-sample="{html.escape(sample)}">{html.escape(sample)}</p>',
        ]
    )


@app.get(ENDPOINT_PATH)
def endpoint_resource(
    request: Request,
    url: str = Query(
        ...,
        description="The SPARQL endpoint URL this resource describes.",
    ),
    store: Store = Depends(get_store),
) -> Response:
    """What this service currently knows about one endpoint."""
    # Negotiation happens before the store is read. A 406 is a statement
    # about the request rather than about the resource, so it does not need
    # to know whether the endpoint exists, and answering it first avoids a
    # store query whose result would be thrown away.
    media_type = choose_representation(request.headers.get("accept"))
    if media_type is None:
        return Response(
            content=(
                "none of the requested media types can be served; this "
                "resource offers " + ", ".join(OFFERED_MEDIA_TYPES) + "\n"
            ),
            status_code=406,
            media_type="text/plain; charset=utf-8",
        )

    measurements = endpoint_measurements(store, url)
    content = endpoint_content(store, url)

    # "Known" is "the store holds any fact about this endpoint", not "the
    # store holds a measurement". web/tests/fixtures/run-truncated.nq is a
    # class sample with no measurement beside it, and 404ing that would deny
    # a resource this service demonstrably has facts about.
    if not measurements.assessed and not content.sampled:
        # The 404 body is not a representation of the resource (there is no
        # resource), so it is not negotiated. A person gets a sentence; a
        # machine gets the status code it actually reads.
        if media_type == HTML_MEDIA_TYPE:
            return Response(
                content=(
                    f"<title>Not found</title>\n<h1>Not found</h1>\n"
                    f"<p>No run in this store has recorded anything about "
                    f"{html.escape(url)}.</p>\n"
                ),
                status_code=404,
                media_type="text/html; charset=utf-8",
            )
        return Response(
            content=f"no run in this store has recorded anything about {url}\n",
            status_code=404,
            media_type="text/plain; charset=utf-8",
        )

    if media_type == HTML_MEDIA_TYPE:
        return Response(
            content=_endpoint_html(url, measurements, content),
            media_type="text/html; charset=utf-8",
        )
    return Response(
        content=_endpoint_rdf(store, url, media_type),
        media_type=media_type,
    )
