"""The HTTP surface: three resources, each served as HTML or as RDF.

This is what this project serves over HTTP: one endpoint's current state, an
index of every endpoint, and `/about`. Each is offered in two kinds of
representation chosen by the request's Accept header. The design spec
requires content negotiation on every resource ("a quality-measurement
service that is not itself machine-readable would be self-defeating"), so
the HTML and the RDF are two representations of one resource rather than two
resources.

`/about` is the odd one, and the section at the bottom of this file says why:
it takes no store dependency and its RDF is assembled here rather than
serialised out of the store, because it describes this service rather than
anything a sweep measured.

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
import re
from collections import Counter
from functools import lru_cache
from pathlib import Path
from urllib.parse import quote

from fastapi import Depends, FastAPI, Query, Request, Response
from jinja2 import Environment, FileSystemLoader, select_autoescape
from pyoxigraph import (
    Literal,
    NamedNode,
    RdfFormat,
    Store,
    Triple,
    Variable,
    serialize,
)

import verdict_encoding
from endpoint_content import EndpointContent, endpoint_content
from endpoint_index import endpoint_index
from endpoint_measurements import EndpointMeasurements, endpoint_measurements
from load_run import CURRENT_GRAPH, pointers_to_missing_runs
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

# The index lives at the root, because it is the way in: before it there was
# one route and reaching it meant knowing an endpoint URL and percent-encoding
# it by hand.
INDEX_PATH = "/"

# This path is not a choice. Every request the prober makes carries
# `sparqlwatch/<version> (+https://<host>/about)` in its User-Agent
# (prober/src/client.rs), so the URL is already published, in somebody else's
# server log, before this route exists. Renaming it would break the one
# promise this project has made to every host it has contacted.
ABOUT_PATH = "/about"

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


# RFC 9110's qvalue grammar, exactly:
#
#   qvalue = ( "0" [ "." 0*3DIGIT ] ) / ( "1" [ "." 0*3("0") ] )
#
# so "0", "0.5", "0.333", "1", "1.0" and "1.000" are q values and "2", "-1",
# "1.5", "0.1234", "abc" and the empty string are not. Matching the grammar
# rather than calling float() is what makes the malformed cases agree with
# each other: float("-1") succeeds, so clamping it to the 0..1 range read an
# explicit refusal out of a malformed value ("text/html;q=-1" was a 406)
# while "q=abc" and "q=2" defaulted to 1.0 and were served. The grammar also
# bounds the value, so nothing needs clamping afterwards.
_QVALUE = re.compile(r"(?:0(?:\.[0-9]{0,3})?|1(?:\.0{0,3})?)\Z")


def _parse_accept(header: str) -> list[tuple[str, str, float]]:
    """Split an Accept header into (type, subtype, q) triples.

    Malformed entries are skipped rather than raising: an Accept header is
    client input, and a request with one unparseable entry among several
    still expressed a usable preference. A q that is not a qvalue is not a
    preference at all, so the parameter is ignored and the range keeps the
    q=1.0 that a range with no q has, as RFC 9110 requires.
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
            if _QVALUE.match(value.strip()):
                quality = float(value.strip())
        parsed.append((type_, subtype, quality))
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
        elif specificity == best_specificity:
            # Two ranges of equal specificity matching one representation,
            # which RFC 9110 does not define. Take the LOWER q, so that
            # "text/html;q=1, text/html;q=0" and "text/html;q=0,
            # text/html;q=1" agree instead of one being a page and the other
            # a 406. That is the same reading as the most-specific-wins rule
            # above: an explicit refusal anywhere in the header must not be
            # turned into an offer.
            quality = min(quality, range_quality)
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
    if not ranges:
        # A header from which no range at all could be read expressed no
        # preference, so it falls back the way a missing header does. This is
        # "Accept: *" (invalid per RFC 9110, still sent by some clients),
        # "Accept: garbage" and "Accept: ,,,".
        #
        # This is NOT the case below, where the ranges parsed and none of
        # them can be served: that is a client saying what it wants, and
        # serving it something else while claiming success is the wrong
        # answer a 406 exists to avoid.
        return HTML_MEDIA_TYPE

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
    """One Store object per path per process, opened once and checked once.

    Not a convenience: an on-disk Oxigraph store is a RocksDB database and
    cannot be opened twice at once, so opening it per request would fail
    under the second concurrent request. Caching here rather than at import
    time keeps the open lazy, which is what lets the tests replace this
    dependency without ever touching a real store path.

    The path is required to exist, because Store() CREATES the RocksDB
    directory when it is missing, and the store it opens is required to
    hold at least one quad. Checking that the directory is non-empty is not
    enough: a typo that names the directory a run's .nq files were copied
    into, or a store directory left behind by a load that raised before
    inserting anything, are both non-empty directories that hold no run.
    Either one used to pass a directory-entries check and open as a fresh,
    empty store, so the server answered every request with "no measurement,
    no decline and no class sample in this store mentions ...", which is
    true of the empty store it had just created (or opened) and
    indistinguishable from a registry nobody has swept. Checking the store's
    own length, once opened, catches both, at the cost of one open of a
    store that is about to be opened anyway. Failing on the first open
    reports the operator's mistake as the operator's mistake.

    The last check is the same failure one step along, and it is not an
    operator's typo. Dropping a bad run graph wholesale is what one graph per
    run is for, and the spec advertises it. Since recency is decided once, in
    current, the endpoints of a dropped run keep an sw:currentRun naming a
    graph that is gone; index.rq and endpoint_measurements.rq both drop such a
    solution on FILTER (BOUND(?generatedAt)), so the index says no run in this
    store has recorded anything for any endpoint and each endpoint page says no
    run has measured it, out of a store whose older run graph still holds every
    verdict. That is the same wrong answer as the three above, out of a store
    that does hold the measurements, so it is refused here rather than answered.

    Refused, and deliberately not answered by falling back to the older run.
    Deciding recency in two places is exactly what moving it into current
    removed, and a fallback would put a second derivation back beside the one
    the read queries use. The repair is a rebuild, which derives current from
    the run graphs alone and so points every endpoint at the newest run that
    still measures it, and the message names it.
    """
    directory = Path(path)
    if not directory.is_dir():
        raise RuntimeError(
            f"{STORE_PATH_VARIABLE} is {path!r}, which is not an existing "
            f"directory. Opening it would create an empty store, and every "
            f"endpoint would then answer 404 as though no sweep had ever "
            f"run. Build the store first with web/load_run.py."
        )
    store = Store(path)
    if len(store) == 0:
        raise RuntimeError(
            f"{STORE_PATH_VARIABLE} is {path!r}, which opened as a store "
            f"holding no quads. That is either a store nothing has been "
            f"loaded into yet, or a path that is not the store at all (the "
            f"run directory load_run.py's second argument names, say, "
            f"rather than its first). Every endpoint would answer 404 as "
            f"though no sweep had ever run. Build the store first with "
            f"web/load_run.py."
        )
    if not store.contains_named_graph(CURRENT_GRAPH):
        raise RuntimeError(
            f"{STORE_PATH_VARIABLE} is {path!r}, which holds run graphs and "
            f"no {CURRENT_GRAPH.value} graph. All three read paths read that "
            f"graph, so every endpoint would answer as though no sweep had "
            f"ever measured it, which is the same wrong answer out of a store "
            f"that does hold the measurements. Every store built before the "
            f"derived graph existed looks like this. Build it with "
            f"'python web/load_run.py --rebuild {path}'."
        )
    missing = pointers_to_missing_runs(store)
    if missing:
        endpoints = sorted({endpoint for endpoint, _, _ in missing})
        runs = sorted({run for _, _, run in missing})
        raise RuntimeError(
            f"{STORE_PATH_VARIABLE} is {path!r}, in which {CURRENT_GRAPH.value} "
            f"points {len(endpoints)} endpoint(s) at {len(runs)} run graph(s) "
            f"this store does not hold: {runs}. "
            f"{endpoints[0]} is one of them. All three read queries reach an "
            f"endpoint's facts through that pointer, so each of those endpoints "
            f"would be answered as though no run had ever measured it, while an "
            f"older run graph in this same store may still hold every verdict "
            f"for it. A run graph has been dropped since current was written. "
            f"Rebuild with 'python web/load_run.py --rebuild {path}'."
        )
    return store


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
_INDEX_DESCRIPTION_QUERY = read_query("index_description")
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
                # stated, only the fact that nothing was measured and why.
                "state_text": f"{state.label} ({declined.reason})",
                "detail": _declined_detail(declined.reason),
                "elapsed_ms": None,
            }
        )
    return sorted(rows, key=lambda row: row["metric"])


# What each decline reason means in words, keyed by the slug the graph
# carries. The slugs are prober/src/emit.rs's NotMeasuredReason::slug, and the
# two sentences say opposite things about who is responsible: "cost-ceiling"
# is a decision this project made about budget, "prober-failed" is our own
# task panicking or being cancelled before it asked the endpoint anything.
# Reporting the second as the first would send an operator to --max-cost
# instead of to the crash.
_DECLINE_DETAILS = {
    "cost-ceiling": "we declined to look, so this says nothing about the endpoint",
    "prober-failed": (
        "the prober failed on this endpoint, so this run observed nothing "
        "about it"
    ),
}

# The detail for a reason this build has no sentence for, from a prober newer
# or older than this page. The same shape as _detail's unrecognised-verdict
# clause, and for the same reason: the row's state text already carries the
# value verbatim, so this says only that we have no reading of it. A reason
# has been added to the vocabulary once already, so the branch is reachable.
_UNRECOGNISED_DECLINE_DETAIL = "unrecognised reason, shown as the store recorded it"


def _declined_detail(reason: str) -> str:
    """The extra clause a declined row carries, read out of its reason."""
    return _DECLINE_DETAILS.get(reason, _UNRECOGNISED_DECLINE_DETAIL)


def _detail(verdict, recognised: bool) -> str | None:
    """The extra clause a row carries beside its state, or nothing."""
    if not recognised:
        return "unrecognised verdict, shown as the store recorded it"
    if verdict.level is not None:
        return f"conformance level {verdict.level}"
    return None


def _unfinished_run_text(measurements: EndpointMeasurements) -> str | None:
    """The sentence for a page whose facts come from a run that did not finish.

    Said where the page already names the sweep its facts came from, because
    that is the claim being qualified: the timestamp beside "Everything below
    is what one probe sweep observed" is a sweep that stopped, and a reader
    who takes it for a completed sweep has been told something untrue by
    omission. What it does NOT say is that anything above is missing: this
    endpoint's own chunk was written whole, which is what put its facts in the
    store at all, so the honest limit of the claim is that the sweep stopped
    and these are the facts it had written for this endpoint.
    """
    if not measurements.run_did_not_finish:
        return None
    return (
        f"The sweep at {measurements.generated_at} did not finish: it "
        f"recorded that it was being written one endpoint at a time and "
        f"never recorded that it was complete. What is above is what it had "
        f"written for this endpoint when it stopped."
    )


def _newer_unfinished_run_text(
    measurements: EndpointMeasurements,
) -> str | None:
    """The sentence for an endpoint a newer, unfinished run never reached.

    A different claim from the one above, and it has to be: nothing on this
    page is stale or partial, every verdict shown is the newest this store
    holds for this endpoint, and the sweep that recorded them may well have
    finished. What is true is that a later sweep exists, stopped, and never
    got here, so this page is not a report on that sweep.

    Both timestamps are named rather than ordered, the same way _provenance
    names them: which is later is a comparison the store makes, and both are
    facts it holds.
    """
    if not measurements.newer_run_did_not_reach_this_endpoint:
        return None
    return (
        f"A later sweep, at {measurements.newest_generated_at}, did not "
        f"finish and never recorded finishing this endpoint, so nothing "
        f"above comes from it. What is above is the newest this store holds "
        f"for this endpoint, from the sweep at "
        f"{measurements.generated_at}."
    )


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
        # The two things that can be wrong with the sweep named just above,
        # each said in its own sentence because they say different things: the
        # first that the sweep whose facts these are stopped, the second that
        # a later sweep stopped before it reached this endpoint. Both can hold
        # at once, in a store holding two crashed runs, and then both are
        # true and both are said.
        "unfinished_text": _unfinished_run_text(measurements),
        "newer_unfinished_text": _newer_unfinished_run_text(measurements),
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
    url: str | None = Query(
        None,
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

    # No url at all. FastAPI's own answer is a 422 with a JSON body, produced
    # before this function runs and therefore without negotiating: a person
    # following the README's curl example without the parameter got JSON
    # whatever they asked for. A missing url and a malformed one are the same
    # class of client error, so they get the same status and the same kind of
    # body, and the 406 above still comes first.
    if url is None:
        return Response(
            content=(
                "this resource describes one endpoint, named by a url query "
                "parameter, and the request carried none: try "
                + ENDPOINT_PATH
                + "?url=https://example.org/sparql\n"
            ),
            status_code=400,
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



# ---------------------------------------------------------------------------
# The index
# ---------------------------------------------------------------------------
#
# One row per endpoint the store holds facts for, grouped by the availability
# verdict's own value. Four things about it are load-bearing rather than
# cosmetic, and each is a mistake this project has written down.
#
# GROUPED BY THE VERDICT'S OWN VALUE, one group per value present, in
# verdict_encoding.STATES' order, plus one final group for endpoints whose
# newest run recorded no availability verdict at all. Not a boolean, and no
# "did not answer" heading over the last three: "absent" means the host
# answered with something that was not a SPARQL result (in the 543-endpoint
# sweep one of the four is a .ttl file on raw.githubusercontent.com) while
# "indeterminate" covers a timeout, a transport error, a DNS failure and an
# HTML front end. Collapsing those turns "we could not determine this" into a
# determined negative, which is the one thing the conformance model exists to
# prevent.
#
# THE METRIC COLUMNS COME FROM THE ROWS, not from a list of eight. See
# web/queries/index.rq's header: a store holding two runs at different metric
# revisions holds two metric sets at once, and a page built around today's
# eight draws chips no measurement stands behind the first time
# prober/metrics.toml changes.
#
# EVERY COUNT CARRIES ITS DENOMINATOR. Stage 1d-a published a wrong number by
# conflating three quantities that were each true of something else, and the
# correction was to state each distinctly.
#
# EVERY ROW WHOSE FACTS ARE NOT CURRENT SAYS SO, with the same two conditions
# the endpoint page states and for the same reason: 543 rows of a crashed
# sweep's facts presented as current is the same wrong answer 543 times.

# The metric the page groups by. Named rather than spelled at the call site so
# that the one place the index turns a verdict into a heading is traceable, the
# same way _CLASSES_METRIC is for the sample.
_AVAILABILITY_METRIC = _METRIC_PREFIX + "availability"

# The two words a qualified row carries, and they are words rather than a
# colour or a title attribute: a marker only a mouse can find qualifies
# nothing. Each is explained in a full sentence in the page's key, which is
# where the reasoning that will not fit in three words lives.
ROW_UNFINISHED_TEXT = "from a sweep that stopped"
ROW_NEVER_REACHED_TEXT = "a later sweep never got here"

# What an empty cell in a metric's column means. It is NOT one of the seven
# states and it is deliberately not drawn as one: a metric this endpoint's
# newest run recorded neither a measurement nor a decline for is a gap in what
# the store holds, and drawing it as "not measured" would claim the run said so.
EMPTY_CELL_TEXT = (
    "A cell holding only a dot means this endpoint's newest run recorded "
    "nothing at all about that metric, neither a measurement nor a decline. It "
    "is a gap in what this service holds, not a verdict about the endpoint, "
    "which is why it is not drawn as one of the states below."
)


def _abbreviation(name: str, width: int) -> str:
    """One metric's abbreviation: ``width`` letters of its first word, then the
    initial of each word after it."""
    words = name.split("-")
    return (words[0][:width] + "".join(word[:1] for word in words[1:])).upper()


def metric_abbreviations(metrics: list[str]) -> dict[str, str]:
    """An abbreviation per metric, unique across the ones given.

    The chips on this page carry an abbreviation rather than the metric's name,
    which is what design/Main.dc.html does (``c.abbr``) and what the size budget
    requires: 543 rows times a full metric name is a third of the budget spent
    on repeating eight words. The endpoint page's chips carry no text at all
    because the metric is named beside them; here there is no room to name it
    beside them, so the abbreviation is the only thing identifying the column
    and it has to be unique and it has to be written out somewhere. The metric
    key on the page is where it is written out.

    A single-word metric gets two letters and a hyphenated one gets the initials
    of its words, which is what keeps "classes" (CL) apart from "cors" (CO) and
    both apart from "cors-preflight" (CP). Where that is still not enough the
    first word lengthens by a letter at a time, and if two metrics cannot be
    told apart that way at all they fall back to their whole names, which are
    unique because they are the metrics' own ids.
    """
    ordered = list(dict.fromkeys(metrics))
    names = {metric: _metric_name(metric) for metric in ordered}
    widths = {
        metric: (2 if "-" not in names[metric] else 1) for metric in ordered
    }

    while True:
        abbreviations = {
            metric: _abbreviation(names[metric], widths[metric])
            for metric in ordered
        }
        counts = Counter(abbreviations.values())
        colliding = [
            metric for metric in ordered if counts[abbreviations[metric]] > 1
        ]
        if not colliding:
            return abbreviations
        grew = False
        for metric in colliding:
            first_word = names[metric].split("-")[0]
            if widths[metric] < len(first_word):
                widths[metric] += 1
                grew = True
        if not grew:
            # Two metrics whose names cannot produce distinct abbreviations,
            # which takes a metric id that is a prefix-free duplicate of
            # another's. The name itself is the abbreviation then: long, and
            # unique, and the page stays readable rather than showing two
            # different metrics under one label.
            return {
                metric: (
                    abbreviations[metric]
                    if counts[abbreviations[metric]] == 1
                    else names[metric].upper()
                )
                for metric in ordered
            }


def _index_metrics(entries: list[EndpointMeasurements]) -> list[dict]:
    """Every metric any row has a fact for, in metric id order.

    Sorted rather than left in the order the store returned them, for the same
    reason EndpointMeasurements sorts its verdicts: SPARQL solution order is
    not specified, and columns that moved between two identical requests would
    look like the metric set had changed.
    """
    metrics = sorted(
        {verdict.metric for entry in entries for verdict in entry.verdicts}
        | {declined.metric for entry in entries for declined in entry.declined}
    )
    abbreviations = metric_abbreviations(metrics)
    return [
        {
            "metric": metric,
            "name": _metric_name(metric),
            "abbr": abbreviations[metric],
        }
        for metric in metrics
    ]


def _index_chips(entry: EndpointMeasurements, metrics: list[dict]) -> list[dict]:
    """One cell per metric column, in the page's column order.

    A cell is a chip when this endpoint's newest run recorded something about
    that metric and an empty placeholder when it did not, so the columns line
    up across rows without a gap being drawn as one of the seven states. See
    EMPTY_CELL_TEXT.
    """
    verdicts = {verdict.metric: verdict for verdict in entry.verdicts}
    declined = {decline.metric: decline for decline in entry.declined}

    cells = []
    for column in metrics:
        metric = column["metric"]
        if metric in verdicts:
            state = verdict_encoding.presentation(verdicts[metric].verdict)
            cells.append(
                {
                    "present": True,
                    "name": column["name"],
                    "abbr": column["abbr"],
                    # The value the store holds, verbatim, including a value
                    # this build has no encoding for: relabelling it would hide
                    # which value the store actually holds.
                    "verdict": verdicts[metric].verdict,
                    "reason": None,
                    "css_class": verdict_encoding.css_class(state.slug),
                    "slug": state.slug,
                }
            )
        elif metric in declined:
            state = verdict_encoding.presentation(verdict_encoding.NOT_MEASURED)
            cells.append(
                {
                    "present": True,
                    "name": column["name"],
                    "abbr": column["abbr"],
                    # No verdict at all, which is the point: the run said it did
                    # not look. The reason travels instead, so a reader and a
                    # test can tell "we priced it out" from "our own probe
                    # died".
                    "verdict": None,
                    "reason": declined[metric].reason,
                    "css_class": verdict_encoding.css_class(state.slug),
                    "slug": state.slug,
                }
            )
        else:
            cells.append({"present": False})
    return cells


def _index_row(entry: EndpointMeasurements, metrics: list[dict]) -> dict:
    """One row: the endpoint, its link, its cells, and any qualification.

    The link is percent-encoded with nothing left safe, because the endpoint
    resource names its endpoint in a url query parameter and a query string is
    unquoted exactly once: an endpoint URL holding a '&' or a '#' left bare
    would arrive truncated, and one holding a percent sequence of its own would
    arrive as a different URL. See "The URL shape" at the top of this file.
    """
    return {
        "endpoint": entry.endpoint,
        "href": ENDPOINT_PATH + "?url=" + quote(entry.endpoint, safe=""),
        "cells": _index_chips(entry, metrics),
        "run_unfinished": entry.run_did_not_finish,
        "never_reached": entry.newer_run_did_not_reach_this_endpoint,
    }


def _index_groups(
    entries: list[EndpointMeasurements], metrics: list[dict]
) -> list[dict]:
    """The rows, grouped by the availability verdict's own value.

    One group per value PRESENT, in verdict_encoding.STATES' order, then any
    value this build has no encoding for, then one final group for endpoints
    whose newest run recorded no availability verdict at all.

    The order is the encoding table's and not any notion of better or worse.
    Ranking the groups would be this service's opinion about the endpoints; the
    table's order is a fact about the vocabulary.

    The final group is merged with nothing, and its key is not a verdict value:
    an endpoint whose run declined every metric it applied has no availability
    verdict, and web/tests/fixtures/run-prober-failed.nq is a whole run of
    exactly that shape, which stage 1c-b3 makes the normal outcome for a host
    group whose probe task panicked. Merging it into "absent" would turn
    "nobody looked" into "we established nothing was there"; merging it into
    "indeterminate" would claim a measurement nobody took.
    """
    availability: dict[str | None, list[EndpointMeasurements]] = {}
    for entry in entries:
        verdict = next(
            (
                measured.verdict
                for measured in entry.verdicts
                if measured.metric == _AVAILABILITY_METRIC
            ),
            None,
        )
        availability.setdefault(verdict, []).append(entry)

    order = {state.slug: index for index, state in enumerate(verdict_encoding.STATES)}
    # A value the table has no entry for sorts after every value it has, and
    # ties among such values are broken by the value itself so the page is
    # stable. None sorts last of all, and it is not a value: see the docstring.
    values = sorted(
        (value for value in availability if value is not None),
        key=lambda value: (order.get(value, len(order)), value),
    )
    if None in availability:
        values.append(None)

    total = len(entries)
    groups = []
    for value in values:
        rows = availability[value]
        if value is None:
            state = verdict_encoding.presentation(verdict_encoding.NOT_MEASURED)
            label = state.label
            # What the grouping criterion IS, and not one of the two ways of
            # meeting it. There are two, they are opposite claims, and both are
            # reachable: a run that DECLINED availability recorded an
            # sw:NotMeasured fact naming the metric and a reason, and a run at a
            # different metric revision recorded nothing about availability in
            # either direction. Saying every metric was declined rather than
            # measured is false of the second on both counts, and it contradicts
            # EMPTY_CELL_TEXT above, which says a metric a run recorded nothing
            # about is a gap in what this service holds and not a verdict. So the
            # sentence says which two cases are here and sends a reader to the
            # row, where the availability column is a chip in the first case and
            # a gap in the second.
            meaning = (
                "The newest run for these endpoints recorded no availability "
                "verdict at all: either it declined the metric, or it recorded "
                "nothing about it. Each row's availability column says which, a "
                "chip for a decline and a dot for a gap. Neither is a verdict "
                "about the endpoint, and neither is merged with one."
            )
        else:
            state = verdict_encoding.presentation(value)
            recognised = state is not verdict_encoding.UNRECOGNISED
            # An unrecognised value is shown verbatim in the heading, the same
            # way the endpoint page shows it in a row's state text.
            label = state.label if recognised else value
            # What the group IS, and deliberately not what the state means in
            # general. verdict_encoding's meanings are written for a metric that
            # can be declared ("works, and the endpoint declares it"), which is
            # true of cors and of the service description and is not true of
            # availability: nothing declares that it answers queries. Printing
            # that generic gloss under an availability heading would explain the
            # group with a sentence about a different metric. The legend at the
            # foot of the page explains the drawing, which is what it is for.
            meaning = (
                f"These are the endpoints whose availability metric read "
                f"\"{value}\" in the newest run that measured them. Nothing "
                f"about their other metrics follows from it: every metric "
                f"carries its own verdict, and the rows are where those are."
            )
        groups.append(
            {
                # Empty for the final group, so that a group keyed on a verdict
                # and the group keyed on no verdict stay distinguishable to a
                # reader of the markup even where a store carried the literal
                # value "not-measured" as a dqv:value.
                "availability": "" if value is None else value,
                "label": label,
                "meaning": meaning,
                "css_class": verdict_encoding.css_class(state.slug),
                "count": len(rows),
                "of": total,
                # The heading a reader sees. It names the metric, because
                # "verified: 3 of 9" on a page of eight metrics does not say
                # verified at what, and it carries the denominator, because
                # "verified 3" invites the question stage 1d-a got wrong.
                "heading": (
                    f"availability {label}: {len(rows)} of {total} endpoints"
                ),
                "rows": [_index_row(entry, metrics) for entry in rows],
            }
        )
    return groups


def _newest_sweep_note(entries: list[EndpointMeasurements]) -> str | None:
    """The sentence for a store whose newest sweep did not finish.

    Said once, at the top, because it is one fact about the store rather than
    543 facts about endpoints: the rows below say which of them it leaves
    qualified and how. Both halves of the condition are required for the reason
    EndpointMeasurements.run_did_not_finish gives: a run from before stage
    1c-b4 promised nothing about finishing, so its missing sw:finalised says
    nothing either.
    """
    if not entries:
        return None
    first = entries[0]
    if first.newest_emission is None or first.newest_finalised:
        return None
    return (
        f"The newest sweep in this store, at {first.newest_generated_at}, did "
        f"not finish: it recorded that it was being written one endpoint at a "
        f"time and never recorded that it was complete. Every row below says "
        f"whether that leaves it qualified, and how."
    )


def _index_context(entries: list[EndpointMeasurements]) -> dict:
    """Everything the index template renders, decided here rather than in the
    page.

    The template loops and formats. What a group is, what order the groups come
    in, what a missing metric means and how a state is drawn are all decisions
    with a right answer, and they belong where they can be tested.
    """
    metrics = _index_metrics(entries)
    groups = _index_groups(entries, metrics)
    # The legend counts the chips on this page, and it is built by the same
    # function as the endpoint page's legend from the same table, so the two
    # pages cannot explain the encoding differently. A cell that is a gap
    # rather than a chip is not counted: it is not one of the states.
    drawn = [
        {"slug": cell["slug"]}
        for group in groups
        for row in group["rows"]
        for cell in row["cells"]
        if cell["present"]
    ]
    return {
        "endpoint_count": len(entries),
        "metrics": metrics,
        "metric_count": len(metrics),
        "groups": groups,
        "newest_generated_at": (
            entries[0].newest_generated_at if entries else None
        ),
        "newest_sweep_note": _newest_sweep_note(entries),
        "row_unfinished_text": ROW_UNFINISHED_TEXT,
        "row_never_reached_text": ROW_NEVER_REACHED_TEXT,
        "empty_cell_text": EMPTY_CELL_TEXT,
        "legend": _legend(drawn),
        "chip_width": verdict_encoding.CHIP_WIDTH_PX,
        "chip_height": verdict_encoding.CHIP_HEIGHT_PX,
        "encoding_css": verdict_encoding.css_rules(),
    }


def _index_html(entries: list[EndpointMeasurements]) -> str:
    """The index, rendered."""
    return _TEMPLATES.get_template("index.html").render(**_index_context(entries))


def _index_rdf(store: Store, media_type: str) -> bytes:
    """Serialise every endpoint's facts, straight from the store."""
    triples = store.query(_INDEX_DESCRIPTION_QUERY)
    return serialize(triples, format=RdfFormat.from_media_type(media_type))


@app.get(INDEX_PATH)
def index_resource(
    request: Request,
    store: Store = Depends(get_store),
) -> Response:
    """Every endpoint this service knows about, in one representation or the
    other.

    Negotiated the same way and by the same function as the endpoint resource,
    because the design spec requires content negotiation of every resource: an
    index a person can read and a machine cannot would make the fleet the one
    thing this service will not publish as data.

    There is no 404 branch. This resource exists whatever the store holds: an
    index of no endpoints is an answer, and _opened_store already refuses a
    store that holds no quads or no derived graph, which is the mistake a 404
    here would be reporting as an empty registry.
    """
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

    if media_type == HTML_MEDIA_TYPE:
        return Response(
            content=_index_html(endpoint_index(store)),
            media_type="text/html; charset=utf-8",
        )
    return Response(
        content=_index_rdf(store, media_type),
        media_type=media_type,
    )


# ---------------------------------------------------------------------------
# /about: the page our User-Agent points at
# ---------------------------------------------------------------------------
#
# Every request the prober makes tells the server it is querying where to
# find out who we are. That makes this the one page in this service whose
# reader did not come looking for it: they found an unfamiliar agent in a log
# and followed the URL. Three consequences are built into the code below.
#
# It takes NO store dependency. get_store raises on a missing store, an empty
# one, and one holding run graphs but no derived current graph. Every one of
# those is a mistake on our side, and none of them is a reason to fail the
# request of somebody asking why we contacted them. The index and the
# endpoint page are representations of measurements and are right to require
# a store; this page is a representation of this service, and a service can
# describe itself with no data loaded.
#
# Its numbers come from named constants below, each one carrying the file and
# constant in prober/ that decides it, and web/tests/test_about.py reads those
# files and compares. A politeness figure on this page is a promise made to
# somebody else's server, so a page saying "two seconds" while
# DEFAULT_MIN_GAP said otherwise would be a confident wrong answer about this
# project's own behaviour. The test is the only thing that keeps the two
# together, because nothing at run time can see the Rust source.
#
# Its RDF is assembled HERE, in Python, and it is the only representation in
# this file that is. endpoint_description.rq and index_description.rq are
# CONSTRUCTs precisely so that neither can state anything the store does not
# hold; that argument does not apply to a document about this service, because
# no run graph holds a triple about who we are or how fast we probe. The
# hazard the CONSTRUCTs avoid is still real here, so it is closed the other
# way: both representations read the same constants, and
# test_the_rdf_and_the_html_state_the_same_numbers_and_the_same_address
# compares them field by field.

# Supplied by the user for this purpose. It is published deliberately: the
# page's whole reason to exist is to give a stranger a way to reach a person,
# and an address nobody can see is not one. Exactly one address, and no form,
# alias or ticket queue beside it, because each of those would be a channel a
# reader would use and nobody would read.
CONTACT_ADDRESS = "michel.dumontier@maastrichtuniversity.nl"

# The string a reader searched their logs for, built by
# prober/src/client.rs's `.user_agent(concat!(...))` out of
# env!("CARGO_PKG_VERSION"). Quoted in full rather than described, because
# matching it against the line in front of them is how a reader confirms this
# page is about the agent they came here for. The version is part of the
# quote, so test_the_user_agent_shown_is_the_one_the_prober_sends reds on a
# version bump and this constant has to move with it.
PROBER_USER_AGENT = (
    "sparqlwatch/0.1.0 (+https://sparqlwatch.dev.k8s.semanticscience.org/about)"
)

# prober/src/registry.rs's DEFAULT_EXCLUSIONS: the file both binaries read at
# every run, relative to their working directory. Named on the page because it
# is checkable from outside: a reader can look and see whether their host is
# on it, which is the only way this promise can be verified by the person it
# was made to.
EXCLUSION_FILE = "registry/exclusions.toml"

# How politely the prober behaves, as the prober's own defaults.
#
#   min-gap-seconds          politeness.rs DEFAULT_MIN_GAP, the default of
#                            main.rs's --min-gap-ms
#   hosts-in-flight          main.rs DEFAULT_CONCURRENCY, the default of
#                            --concurrency. HOSTS, not endpoints
#   requests-per-second      hosts-in-flight / min-gap-seconds, which is the
#                            aggregate rate main.rs's own comment on
#                            DEFAULT_CONCURRENCY derives
#   retry-after-cap-seconds  politeness.rs DEFAULT_RETRY_AFTER_CAP, the
#                            default of --retry-after-cap-s
#   *-budget-seconds         budget.rs's impl Default for Budget
#   requests-per-endpoint    one per metric that is cheap at the default cost
#                            ceiling (main.rs defaults --max-cost to
#                            Cost::Cheap), counted from prober/metrics.toml
#
# Written out as literals rather than computed, because nothing in this
# process can read Rust. web/tests/test_about.py reads every one of those
# files and fails on a mismatch, which is what makes these numbers a
# statement about the prober rather than about this dictionary.
POLITENESS = {
    "min-gap-seconds": 2,
    "hosts-in-flight": 4,
    "requests-per-second": 2,
    "retry-after-cap-seconds": 20,
    "request-budget-seconds": 30,
    "metric-budget-seconds": 60,
    "endpoint-budget-seconds": 600,
    "requests-per-endpoint": 7,
}

# Where the list of endpoints came from. Every value is in
# prober/registry/lod-cloud.provenance.toml, which the seeder writes beside
# the registry it generated, except endpoint-count, which is the length of
# prober/registry/lod-cloud.toml itself.
#
# On the page because it answers the reader's second question. The first is
# "who are you"; the second is "why me", and the answer is that a public dump
# of dataset metadata listed their endpoint and nobody asked them. That is
# also the whole reason the exclusion mechanism has to exist.
REGISTRY = {
    "endpoint-count": 543,
    "dump": "https://lod-cloud.net/versions/2026-06-15/lod-data.json",
    "dump-version": "2026-06-15",
    "entries": 725,
    "distinct": 548,
}

# The one full sweep of that registry this project has run, from
# prober/README.md under Sweep cost. Quoted rather than rounded because it is
# a measurement and the README is where it is recorded; the same section says
# every other figure there is an estimate.
FULL_SWEEP_DURATION = "1h26m21s"

# The lead sentence, in one place because both representations state it. A
# machine that asks this resource for RDF gets the same sentence a person
# reads, rather than a document that describes the page without saying what
# the service does.
SUMMARY = (
    "sparqlwatch measures public SPARQL endpoints and publishes what it "
    "measured: it sends a few small read-only queries to each endpoint on a "
    "public list, and records what came back, per endpoint and per check."
)

# The subject of the RDF representation, and the namespace its predicates sit
# in. urn:sparqlwatch: is the scheme every IRI this project mints already
# uses (sw:metric:..., sw:activity:...), so sw:service and sw:about:... are
# that convention continued rather than a second one.
#
# These predicates are minted here and appear in no run graph, which is the
# opposite of the rule endpoint_description.rq and index_description.rq
# follow. The difference is what the document is about: a triple about an
# endpoint must come from a measurement, and there is no measurement of who we
# are. Kept deliberately few, and each one is pinned by the test that pins the
# prose beside it.
_SERVICE = NamedNode("urn:sparqlwatch:service")
_ABOUT = "urn:sparqlwatch:about:"
_XSD_INTEGER = NamedNode("http://www.w3.org/2001/XMLSchema#integer")


def _about_context() -> dict:
    """Everything web/templates/about.html renders, and nothing derived.

    The template writes the prose; this hands it the numbers and the strings
    that have to agree with something outside the template.
    """
    return {
        "summary": SUMMARY,
        "contact": CONTACT_ADDRESS,
        "user_agent": PROBER_USER_AGENT,
        "exclusion_file": EXCLUSION_FILE,
        "politeness": POLITENESS,
        "registry": REGISTRY,
        "full_sweep": FULL_SWEEP_DURATION,
        "index_path": INDEX_PATH,
        "endpoint_path": ENDPOINT_PATH,
    }


def _about_html() -> str:
    """The page, rendered."""
    return _TEMPLATES.get_template("about.html").render(**_about_context())


def _about_rdf(media_type: str) -> bytes:
    """The same statements, for a machine.

    A crawler or an operator's tooling wants two things from this resource:
    where to complain, and what rate to expect. Both are here as data, so
    neither has to be read out of prose. The politeness figures are typed
    xsd:integer in seconds and in counts, which is what the flags they come
    from are.
    """
    triples = [
        Triple(_SERVICE, NamedNode(_ABOUT + "summary"), Literal(SUMMARY)),
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + "contact"),
            NamedNode("mailto:" + CONTACT_ADDRESS),
        ),
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + "user-agent"),
            Literal(PROBER_USER_AGENT),
        ),
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + "exclusion-list"),
            Literal(EXCLUSION_FILE),
        ),
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + "registry-endpoint-count"),
            Literal(str(REGISTRY["endpoint-count"]), datatype=_XSD_INTEGER),
        ),
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + "registry-source"),
            NamedNode(REGISTRY["dump"]),
        ),
    ]
    triples.extend(
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + name),
            Literal(str(value), datatype=_XSD_INTEGER),
        )
        for name, value in POLITENESS.items()
    )
    return serialize(iter(triples), format=RdfFormat.from_media_type(media_type))


@app.get(ABOUT_PATH)
def about_resource(request: Request) -> Response:
    """Who is querying your endpoint, how often, and how to make it stop.

    No store parameter, and that absence is the feature. See the section
    comment above.

    Negotiated by the same function as the other two resources, because the
    design spec requires content negotiation of every resource and this one is
    no exception: an operator's tooling should be able to read our contact
    address and our rate without parsing English.
    """
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

    if media_type == HTML_MEDIA_TYPE:
        return Response(
            content=_about_html(),
            media_type="text/html; charset=utf-8",
        )
    return Response(content=_about_rdf(media_type), media_type=media_type)
