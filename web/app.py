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

The HTML is web/templates/endpoint.html, rendered from the view model
_page_context builds below. Two things about it are load-bearing rather than
cosmetic:

  * The verdict encoding comes from web/verdict_encoding.py, which is the
    single implementation of docs/design/verdict-encoding.md. Nothing in this
    file or in the template decides how a state is drawn.
  * The data-metric / data-verdict / data-declined attributes are a contract.
    web/tests/test_negotiation.py reads them to compare what a person is
    shown against what a machine is served, and a template that drops them
    leaves this service with no way to check that the two agree.
"""

from __future__ import annotations

import html
import os
from functools import lru_cache
from pathlib import Path

from fastapi import Depends, FastAPI, Query, Request, Response
from jinja2 import Environment, FileSystemLoader, select_autoescape
from pyoxigraph import NamedNode, RdfFormat, Store, Variable, serialize

import verdict_encoding
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


# ---------------------------------------------------------------------------
# The page
# ---------------------------------------------------------------------------
_TEMPLATES = Environment(
    loader=FileSystemLoader(Path(__file__).parent / "templates"),
    # Autoescaping on, because every value on this page is data from a
    # measured third party: endpoint URLs, class IRIs and metric ids all come
    # from somewhere this project does not control.
    autoescape=select_autoescape(default_for_string=True, default=True),
    trim_blocks=True,
    lstrip_blocks=True,
)

_METRIC_PREFIX = "urn:sparqlwatch:metric:"

# The metric whose query produces the class sample. This mirrors the pin
# inside web/queries/endpoint_content.rq: that query asks only about
# sw:metric:classes, so this is the metric whose measurement row explains an
# absent sample. If the query's pin ever moves, this moves with it.
_CLASSES_METRIC = _METRIC_PREFIX + "classes"

# The verdict that asserts the negative. It is one of the two assertive
# values in the closed vocabulary (docs/design/verdict-encoding.md), and the
# only one a missing class sample can carry, so it is the one verdict whose
# absence of a sample is a finding rather than a gap.
_ABSENT = "absent"

# The truncation state, in words. A truncated sample is the one thing on this
# page that must not be legible only as a border style: a reader who cannot
# see the dashed outline, or who is hearing the page read aloud, would
# otherwise take a cut-off list for a complete vocabulary.
TRUNCATED_TEXT = "truncated: more may exist beyond the limit"
COMPLETE_TEXT = "complete: not truncated"


def _metric_name(metric: str) -> str:
    """A metric id as the page shows it.

    The local name of a sparqlwatch metric IRI, and the whole IRI for anything
    else, so a metric from a prober this page has never heard of still renders
    as something a reader can look up rather than being dropped.
    """
    if metric.startswith(_METRIC_PREFIX):
        return metric[len(_METRIC_PREFIX) :]
    return metric


def _rows(measurements: EndpointMeasurements) -> list[dict]:
    """One row per metric the run recorded, verdicts and declines together.

    They are merged into one list, sorted by metric id, rather than shown as
    two sections: the reader's question is "what does this run say about this
    metric", and a metric the run declined belongs in the same place as the
    others, drawn in the state that says we did not look.
    """
    rows = []
    for verdict in measurements.verdicts:
        state = verdict_encoding.presentation(verdict.verdict)
        recognised = state is not verdict_encoding.UNRECOGNISED
        rows.append(
            {
                "metric": verdict.metric,
                "name": _metric_name(verdict.metric),
                "declined": False,
                "reason": None,
                "verdict": verdict.verdict,
                "slug": state.slug,
                "css_class": verdict_encoding.css_class(state.slug),
                "token": state.token,
                # An unrecognised value is shown verbatim: relabelling it
                # would hide which value the store actually holds.
                "state_text": state.label if recognised else verdict.verdict,
                "detail": _detail(verdict, recognised),
                "elapsed_ms": verdict.elapsed_ms,
            }
        )
    for declined in measurements.declined:
        state = verdict_encoding.presentation(verdict_encoding.NOT_MEASURED)
        rows.append(
            {
                "metric": declined.metric,
                "name": _metric_name(declined.metric),
                "declined": True,
                "reason": declined.reason,
                "verdict": None,
                "slug": state.slug,
                "css_class": verdict_encoding.css_class(state.slug),
                "token": state.token,
                # Not a verdict, and it does not read like one: no value is
                # stated, only the fact that we chose not to look and why.
                "state_text": f"{state.label} ({declined.reason})",
                "detail": "we declined to look, so this says nothing about the endpoint",
                "elapsed_ms": None,
            }
        )
    return sorted(rows, key=lambda row: row["metric"])


def _detail(verdict, recognised: bool) -> str | None:
    """The extra clause a row carries beside its state, or nothing."""
    if not recognised:
        return "unrecognised verdict, shown as the store recorded it"
    if verdict.level is not None:
        return f"conformance level {verdict.level}"
    return None


def _legend(rows: list[dict]) -> list[dict]:
    """The seven states, with how many rows on this page are in each.

    Built from verdict_encoding.STATES, and the swatch takes the same class as
    the chips, so a swatch cannot explain a drawing the chips do not use. All
    seven are listed even at a count of zero: the legend explains an encoding,
    not this endpoint, and a reader comparing two endpoints should not find the
    key changing shape between them.
    """
    counts: dict[str, int] = {}
    for row in rows:
        counts[row["slug"]] = counts.get(row["slug"], 0) + 1

    states = list(verdict_encoding.STATES)
    # The eighth entry appears only when something on the page needed it.
    if counts.get(verdict_encoding.UNRECOGNISED.slug):
        states.append(verdict_encoding.UNRECOGNISED)

    return [
        {
            "slug": state.slug,
            "label": state.label,
            "meaning": state.meaning,
            "css_class": verdict_encoding.css_class(state.slug),
            "count": counts.get(state.slug, 0),
        }
        for state in states
    ]


def _sample(
    measurements: EndpointMeasurements, content: EndpointContent
) -> dict:
    """The class sample, with the sweep that took it, and what this run says.

    Two facts can be in play here and they do not have to come from one
    sweep. endpoint_measurements.rq picks the newest run that MEASURED this
    endpoint; endpoint_content.rq picks the newest run that SAMPLED it. Those
    differ the moment a cheap sweep declines sw:metric:classes, which is the
    prober's default cost ceiling, so a store whose newest run declined the
    metric is the intended steady state rather than an edge case.

    Both facts are kept and each is attributed to its own sweep. Discarding
    the sample because a later cheap sweep did not repeat it would throw away
    a true observation; dating it to the later sweep would state one the
    graph contradicts. ``run`` and ``generated_at`` below are the SAMPLE's,
    never the page's, and ``provenance_text`` is the sentence that says so in
    words whenever the two differ, so a reader can tell which sweep saw what
    without knowing this codebase.

    ``this_run_text`` is this run's own account of sw:metric:classes, and it
    is read out of the graph rather than assumed. There is no single reason a
    run has no sample: the fixtures alone hold a probe that ran for thirty
    seconds and returned nothing (qlever, sw:metric:classes "indeterminate",
    sw:elapsedMs 30003), a run that declined the metric on its cost ceiling,
    and a verdict of "absent", which is the one case where the probe did
    establish that the endpoint holds no classes. Rendering any of them as an
    empty class list would tell a reader the endpoint holds no classes;
    naming the wrong cause, or denying the negative the graph does assert,
    would be another confident wrong answer.
    """
    from_another_sweep = (
        content.sampled
        and measurements.run is not None
        and content.run != measurements.run
    )

    sample = {
        "present": content.sampled,
        "size": content.size,
        "truncated": content.truncated,
        "truncation_text": (
            None
            if not content.sampled
            else TRUNCATED_TEXT
            if content.truncated
            else COMPLETE_TEXT
        ),
        "classes": content.classes,
        "run": content.run,
        "generated_at": content.generated_at,
        "from_another_sweep": from_another_sweep,
        "provenance_text": _provenance(measurements, content, from_another_sweep),
        "this_run_text": None,
    }

    if content.sampled and not from_another_sweep:
        # The single-sweep case: this run took the sample below, so there is
        # nothing further to say about what this run did or did not see.
        return sample

    declined = _declined_classes(measurements)
    if declined is not None:
        # Reachable with a sample on the page as well as without one: a run
        # that declined the metric published no class sample of its own
        # whatever older sweeps published, and the reader is entitled to know
        # that this run did not look.
        sample["this_run_text"] = (
            f"This run did not measure the classes metric at all, recording "
            f"the reason '{declined.reason}'. Nobody looked in this run, so "
            f"it reports nothing about what classes the endpoint holds."
        )
        return sample

    if content.sampled:
        # A sample from another sweep, and this run measured the metric
        # rather than declining it. Whether this run took a sample of its own
        # is not knowable from here (endpoint_content.rq answers with the
        # newest sample only), so nothing is claimed about it: the verdict is
        # already on the page in its own row, and the sample says which sweep
        # took it.
        return sample

    measured = _measured_classes(measurements)
    if measured is not None:
        elapsed = (
            f" after {measured.elapsed_ms} ms"
            if measured.elapsed_ms is not None
            else ""
        )
        if measured.verdict == _ABSENT:
            # The one assertive negative this metric can produce.
            # prober/src/resolve.rs records "absent" for the classes probe
            # only when the endpoint answered with a parsed SPARQL-JSON
            # result that bound nothing, and the prober writes no sample
            # beside it (it skips a sample with no values). Telling the
            # reader that this is not a report of an empty endpoint would
            # contradict the store and bury the finding.
            sample["this_run_text"] = (
                f"No class sample from this run: the classes metric read "
                f"'{measured.verdict}'{elapsed}. That verdict is recorded "
                f"only when the probe ran the classes query and the "
                f"endpoint's own answer listed nothing, so this run does "
                f"report that the endpoint holds no classes."
            )
        else:
            sample["this_run_text"] = (
                f"No class sample from this run: the classes metric read "
                f"'{measured.verdict}'{elapsed} and produced no list. "
                f"The probe looked and came back empty-handed, which is not a "
                f"report that the endpoint holds no classes."
            )
        return sample

    sample["this_run_text"] = (
        "No class sample from this run, and this run recorded no measurement "
        "of the classes metric either. This page therefore says nothing about "
        "what classes the endpoint holds."
    )
    return sample


def _declined_classes(measurements: EndpointMeasurements):
    """This run's decline of the classes metric, if it declined it."""
    for declined in measurements.declined:
        if declined.metric == _CLASSES_METRIC:
            return declined
    return None


def _measured_classes(measurements: EndpointMeasurements):
    """This run's measurement of the classes metric, if it measured it."""
    for verdict in measurements.verdicts:
        if verdict.metric == _CLASSES_METRIC:
            return verdict
    return None


def _provenance(
    measurements: EndpointMeasurements,
    content: EndpointContent,
    from_another_sweep: bool,
) -> str | None:
    """The sentence naming the sweep a sample came from, where it is needed.

    Nothing is said in the single-sweep case, which is most stores today: a
    clause explaining which sweep saw what would be noise on a page where
    one sweep saw everything, and the header sentence already dates it.

    The two timestamps are named, never ordered. Which of them is later is a
    comparison of xsd:dateTime values, and every claim on this page has to be
    one the graph makes; saying "an earlier sweep" of a sample that is
    actually the newer one would be a small confident wrong answer of the
    same kind as the rest.
    """
    if from_another_sweep:
        return (
            f"This class sample comes from a different sweep, at "
            f"{content.generated_at}, and not from the sweep at "
            f"{measurements.generated_at} that the verdicts above come from."
        )
    if content.sampled and measurements.run is None:
        return f"This class sample comes from the sweep at {content.generated_at}."
    return None


def _page_context(
    endpoint: str,
    measurements: EndpointMeasurements,
    content: EndpointContent,
) -> dict:
    """Everything the template renders, decided here rather than in the page.

    The template loops and formats; it makes no judgement about what a missing
    sample means or how a state is drawn. Both of those are decisions with a
    right answer, and they belong where they can be tested.
    """
    rows = _rows(measurements)
    return {
        "endpoint": endpoint,
        # The MEASURING sweep, and only that one. The class sample carries
        # its own run and timestamp (see _sample), because the two are
        # routinely different runs and one timestamp printed over both dates
        # a sample to a sweep that never took it. A sample-only endpoint
        # (web/tests/fixtures/run-truncated.nq) has no measuring sweep at
        # all, and saying so is the point: the page must not imply one that
        # did not happen.
        "run": measurements.run,
        "generated_at": measurements.generated_at,
        "rows": rows,
        "legend": _legend(rows),
        "sample": _sample(measurements, content),
        "chip_width": verdict_encoding.CHIP_WIDTH_PX,
        "chip_height": verdict_encoding.CHIP_HEIGHT_PX,
        "encoding_css": verdict_encoding.css_rules(),
    }


def _endpoint_html(
    endpoint: str,
    measurements: EndpointMeasurements,
    content: EndpointContent,
) -> str:
    """The page, rendered."""
    return _TEMPLATES.get_template("endpoint.html").render(
        **_page_context(endpoint, measurements, content)
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

    # A url that cannot be an absolute IRI cannot name an endpoint, and both
    # store queries would raise ValueError converting it. That is a malformed
    # request, not an identifier that missed: 400 rather than 404, because
    # this store holding nothing about the string is not the interesting
    # fact, and rather than the 500 an uncaught ValueError produced, which
    # tells the client the service is broken when the request was. An empty
    # url is what a submitted-but-empty form field sends and a trailing space
    # is what a copy-paste sends, so this is an ordinary request rather than
    # an attack. The value is NOT trimmed, case-folded or otherwise
    # repaired: the endpoint IRI round-trips byte for byte by design (see
    # "The URL shape" above), and silently normalising it would make this
    # resource describe a different endpoint from the one asked for.
    try:
        NamedNode(url)
    except ValueError:
        # Like the 404 below, this is not a representation of the resource,
        # so it is not negotiated. It says what was wrong with the request.
        return Response(
            content=(
                f"the url parameter is not a valid absolute IRI, so it "
                f"cannot name a SPARQL endpoint: {url!r}\n"
            ),
            status_code=400,
            media_type="text/plain; charset=utf-8",
        )

    measurements = endpoint_measurements(store, url)
    content = endpoint_content(store, url)

    # What is actually checked, and it is narrower than "the store holds any
    # fact about this endpoint": a measurement, a decline, or a class sample.
    # endpoint_content.rq pins sw:sampledBy sw:metric:classes, so an endpoint
    # known only by a sample from another metric reaches this branch while
    # the store describes it (web/tests/fixtures/run-properties-sample.nq is
    # exactly that shape). The body therefore says what was looked for
    # instead of claiming the store knows nothing.
    #
    # A knownness test that is not tied to one metric belongs to spec stage
    # 2b, where properties sampling lands and every properties-only endpoint
    # would otherwise 404. Widening it here would mean widening
    # endpoint_content.rq's pin, which is the line that keeps another
    # metric's values from being published as an endpoint's classes.
    #
    # It cannot be narrowed to "holds a measurement" either:
    # web/tests/fixtures/run-truncated.nq is a class sample with no
    # measurement beside it, and 404ing that would deny a resource this
    # service demonstrably has facts about.
    if not measurements.assessed and not content.sampled:
        # The 404 body is not a representation of the resource (there is no
        # resource), so it is not negotiated. A person gets a sentence; a
        # machine gets the status code it actually reads.
        checked = (
            "measurement, no decline and no class sample in this store "
            "mentions"
        )
        if media_type == HTML_MEDIA_TYPE:
            return Response(
                content=(
                    f"<title>Not found</title>\n<h1>Not found</h1>\n"
                    f"<p>No {checked} {html.escape(url)}.</p>\n"
                ),
                status_code=404,
                media_type="text/html; charset=utf-8",
            )
        return Response(
            content=f"no {checked} {url}\n",
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
