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

import hashlib
import html
import json
import os
import re
from collections import Counter
from functools import lru_cache
from pathlib import Path
from urllib.parse import quote, urlparse

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
from endpoint_content import CLASS_SAMPLING_METRICS, EndpointContent, endpoint_content
from explore_payload import build_payload, endpoint_vocabulary
from void_document import void_summary, void_triples
from endpoint_index import endpoint_index
from endpoint_history import EndpointHistory, endpoint_history
from fleet import FleetHistory, fleet_history, fleet_stats
from endpoint_measurements import EndpointMeasurements, endpoint_measurements
from load_run import CURRENT_GRAPH, pointers_to_missing_runs
from queries import read_query
from registry_names import Name, display, load_names

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
# The documentation section. Three pages, and the third is /about, which keeps
# its own url because the prober's User-Agent points at it; see the /docs
# section comment further down.
DOCS_PATH = "/docs"
DOCS_METRICS_PATH = "/docs/metrics"
DOCS_STATES_PATH = "/docs/states"
# The derived description's own vocabulary. A `urn:` term resolves to
# nothing, so a consumer who meets one has the word and nowhere to look.
DOCS_VOID_PATH = "/docs/void"

# This path is not a choice. Every request the prober makes carries
# `sparqlwatch/<version> (+https://<host>/about)` in its User-Agent
# (prober/src/client.rs), so the URL is already published, in somebody else's
# server log, before this route exists. Renaming it would break the one
# promise this project has made to every host it has contacted.
EXPLORE_PATH = "/explore"
# The derived description. Its OWN path rather than another representation of
# /endpoint, because the two are different documents about the same thing:
# /endpoint publishes what we MEASURED (verdicts, timings, declines) and this
# publishes what we OBSERVED OF ITS CONTENT, shaped as VoID. A tool that wants
# a description to autocomplete against needs a url it can point at and quote,
# and "the RDF you get from /endpoint if you ask for turtle" is not one.
VOID_PATH = "/void"

ABOUT_PATH = "/about"

# The tab icon, and the separate monochrome mask a Safari pinned tab uses.
ICON_PATH = "/icon.svg"
ICON_MASK_PATH = "/icon-mono.svg"

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
# The vocabulary explorer's data was a committed 190 KB file here until
# 2026-09-05: one preserved probe of two endpoints taken 2026-08-28, read at
# import and handed to the browser as text. It existed because the store held no
# content samples, `classes` being declined at the default cost ceiling on every
# sweep, so there was nothing to compute a payload from.
#
# It is computed from the store now. See web/explore_payload.py, and
# explore_endpoints below for the index link that depended on the file's own
# endpoint list and so went missing for every row once the registry changed.
def _explore_probe_note(payload: dict) -> str:
    """What this page is a reading of, counted rather than asserted.

    Was the literal string "prototype: 2 endpoints, probed 2026-08-28" while a
    static file backed this page. A hardcoded provenance note outlives the data
    it describes, and this one had: it still said two endpoints after the
    registry was replaced with three.

    TAKES THE PAYLOAD, NOT THE STORE, since 2026-09-14. It used to build its
    own, which meant /explore built the same payload twice for one render --
    once for the note and once for the JSON beside it. Measured at the time,
    that was the difference between a 19 s page and a 48 s one.
    """
    n = len(payload["endpoints"])
    terms = len(payload["terms"])
    return (
        f"{n} endpoint{'' if n == 1 else 's'} with a content profile, "
        f"{terms} term{'' if terms == 1 else 's'}"
    )


def explore_endpoints(store: Store) -> frozenset[str]:
    """The endpoints the explorer has vocabulary for.

    The index links a row to /explore only when this holds its endpoint. A
    `content` link that opens an explorer with nothing in it tells a reader the
    endpoint has no vocabulary, when what happened is that nobody looked, and on
    these pages an absent qualifier is a positive claim. So is a link that leads
    somewhere empty.

    Read from the STORE since 2026-09-05. It read a static payload file until
    then, listing the two endpoints a 2026-08-28 prototype probe covered, so
    every other row lost its link no matter what the store knew. The file's own
    comment predicted this: "the set will grow to the whole registry when the
    content-profile work lands, at which point this stops being a filter and
    becomes a formality". It is still the right shape rather than a formality:
    an endpoint whose content sweep failed has no profile and still should not
    get the link.
    """
    return frozenset(e["url"] for e in build_payload(store)["endpoints"])


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
    # READ-ONLY, which is both what this tier does and what lets anything else
    # write. RocksDB allows one writer, and the site held that lock until
    # 2026-09-13: a loader running beside it could not open the store at all,
    # so every load meant stopping the site first. That is workable by hand and
    # not workable for a nightly sweep in a cluster.
    #
    # It is also a guard. Opening read-write is what let a store be replaced
    # underneath a running site on 2026-09-05, which came back as
    # "Corruption: mismatch in unique ID on table file 10". A reader cannot do
    # that.
    #
    # THE COST, stated because it shapes the deployment: a read-only handle
    # takes its snapshot at open and never sees a later write. A fresh handle
    # sees them. So a site serving from this must be restarted after a load,
    # which is what the CronJob does in ids3/projects/sparqlwatch/dev.
    try:
        store = Store.read_only(path)
    except (OSError, ValueError) as exc:
        # A read-only open of a directory that is not a RocksDB store fails
        # with the store's own message about a missing CURRENT file, which
        # names neither the variable nor the mistake. Read-write used to create
        # a store here instead and fall through to the empty-store refusal
        # below, so the explanation lived there; it has to be raised here now.
        raise RuntimeError(
            f"{STORE_PATH_VARIABLE} is {path!r}, which is a directory but not "
            f"a store this can open ({exc}). That is either a store nothing "
            f"has been loaded into yet, or a path that is not the store at all "
            f"(the run directory load_run.py's second argument names, say, "
            f"rather than its first). Build the store first with "
            f"web/load_run.py."
        ) from exc
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
            f"{endpoints[0]} is one of them. index.rq and "
            f"endpoint_measurements.rq reach an endpoint's verdicts through "
            f"sw:currentRun and endpoint_content.rq reaches its class sample "
            f"through its sample pointer, and each drops a solution whose run "
            f"graph is gone, so those endpoints would be answered as though no "
            f"run had ever measured them, while an older run graph in this same "
            f"store may still hold every verdict for them. A run graph has been "
            f"dropped from this store since current was written, which is the "
            f"operation one graph per run exists to make possible. Rebuild with "
            f"'python web/load_run.py --rebuild {path}'."
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
    # FastAPI serves a Swagger UI at /docs and a ReDoc at /redoc unless told
    # not to, and both are off for two reasons.
    #
    # The first is that /docs is this site's documentation section as of
    # 2026-08-28 and the framework's route wins a collision silently: the page
    # rendered, the tests failed, and what came back was an API explorer.
    #
    # The second is the better reason and would stand on its own. That explorer
    # is a third-party script: the served page carries
    # `src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"`,
    # so every reader who opened it fetched code from a CDN this project has no
    # relationship with and told that CDN which page they were on. This site puts
    # `rel="noreferrer"` on its one outward link so that a person looking up
    # their own endpoint does not announce to it that they read us first, and
    # shipping a CDN bundle nobody asked for is the same leak with the argument
    # reversed. The schema itself stays at /openapi.json, which is ours, static
    # and machine-readable.
    docs_url=None,
    redoc_url=None,
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

# One filter, so a template never decides how a state is drawn. The mapping from
# a verdict slug to its class is verdict_encoding's alone, and a template that
# built the class name itself would be a second place that has to be right.
_TEMPLATES.filters["enc_class"] = lambda slug: verdict_encoding.css_class(
    verdict_encoding.presentation(slug).slug
)

_METRIC_PREFIX = "urn:sparqlwatch:metric:"

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
    # Verdictless metrics are excluded here, as they are from the index's
    # columns and for the same reason, found by reading a real timeline. A
    # `class-profiles` row drew `not measured` on the days its pass was
    # declined and a GAP on the day it SUCCEEDED, because a successful pass
    # publishes no measurement and no decline. A gap means "this run recorded
    # nothing", so the one day the pass worked was the one day the row looked
    # like nothing happened.
    #
    # Nothing is lost by dropping the row: the class sample section below
    # already states this run's account of the pass in words, including which
    # reason it was declined for, and the `content` link says whether there is
    # a profile to read.
    """One row per metric the run recorded, verdicts and declines together.

    They are merged into one list, sorted by metric id, rather than shown as
    two sections: the reader's question is "what does this run say about this
    metric", and a metric the run declined belongs in the same place as the
    others, drawn in the state that says we did not look.
    """
    rows = []
    for verdict in measurements.verdicts:
        if not _yields_measurement(verdict.metric):
            continue
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
        if not _yields_measurement(declined.metric):
            continue
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
    # The SAME order the index draws its columns in. Sorted by metric id until
    # 2026-09-05, which meant the listing and the detail page disagreed about
    # what order these ten things come in, and a reader moving between them had
    # to find each metric twice.
    return sorted(rows, key=lambda row: _column_rank(row["metric"]))


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
    # The third party in the same argument about responsibility: not our budget
    # and not our crash, but a question the endpoint did not answer. The pass
    # asked which classes are in there and got no readable result, so it
    # profiled none of them, and that is a fact about this exchange rather than
    # a finding about the endpoint's content.
    "enumeration-failed": (
        "we asked which classes the endpoint holds and got no readable "
        "answer, so nothing was profiled"
    ),
    # The fourth party, and the only one that blames the endpoint outright.
    # Note what it does NOT say: nothing about being unreachable, because it
    # covers a host that answers too slowly as well as one that does not answer
    # at all. And a host that answered with HTML or a 500 was probed in full,
    # so this never appears for an endpoint that merely refused the query.
    # The fifth, and the only one that is about cost rather than about failure
    # of any kind. The pass is up to 200 queries; when the endpoint's triple and
    # class counts are where they were, repeating it re-derives what the store
    # already holds. What is shown for that endpoint's vocabulary is the
    # previous pass's, which is why the decline is published rather than the
    # row silently left out.
    "unchanged": (
        "its content has not moved since we last looked, so we did not look "
        "again"
    ),
    "liveness-failed": (
        "the endpoint did not answer a trivial query, so the rest of the "
        "checks were never sent"
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


def _history_view(history: EndpointHistory, rows: list[dict]) -> dict:
    """The interactive timeline: metrics down, sweeps across.

    Ordered by `rows`, so the chart and the list above it name the metrics in
    the same order. A chart that sorted itself would make a reader match ten
    labels twice.
    """
    names = {r["metric"]: r["name"] for r in rows}
    return {
        "has_history": history.has_history,
        "runs": history.runs,
        "metrics": [
            {
                "metric": r["metric"],
                "name": r["name"],
                "cells": _history_cells(history, r["metric"]),
            }
            for r in rows
            if any(c["present"] for c in _history_cells(history, r["metric"]))
        ],
    }


def _history_cells(history: EndpointHistory, metric: str) -> list[dict]:
    """One cell per run for `metric`, oldest first, aligned to the run list.

    Drawn with the SAME encoding the grid uses, because a reading in a timeline
    is the same fact as a reading in a cell and a second visual language for it
    would be a second thing to learn. A run that said nothing is a gap, exactly
    as an empty cell on the index is: this run recorded nothing about that
    metric, which is not a verdict about the endpoint.
    """
    for m in history.metrics:
        if m.metric != metric:
            continue
        cells = []
        for at, reading in zip(history.runs, m.readings):
            if reading is None:
                cells.append({"present": False, "at": at})
                continue
            slug = reading.verdict or verdict_encoding.NOT_MEASURED
            state = verdict_encoding.presentation(slug)
            # `presentation` returns UNRECOGNISED rather than None for a slug
            # this build does not know, so the check is against that object and
            # not against None.
            recognised = state is not verdict_encoding.UNRECOGNISED
            cells.append(
                {
                    "present": True,
                    "at": at,
                    "verdict": reading.verdict,
                    "reason": reading.reason,
                    "css_class": verdict_encoding.css_class(state.slug),
                    # The store's own word where this build does not know it,
                    # the same way a row's state text does.
                    "label": state.label if recognised else (reading.verdict or ""),
                }
            )
        return cells
    return []


def _detail(verdict, recognised: bool) -> str | None:
    """The extra clause a row carries beside its state, or nothing."""
    if not recognised:
        return "unrecognised verdict, shown as the store recorded it"
    if verdict.level is not None:
        return f"conformance level {verdict.level}"
    return _counts_detail(verdict)


def _counts_detail(verdict) -> str | None:
    """The numbers a counting metric compared, in words.

    The verdict alone is a grade of a claim and never the claim: `verified`
    says a description was right without saying what it said, and a reader
    asking how big an endpoint is wants the number. The prober publishes both
    numbers beside the verdict for exactly this, and they went unread on this
    page until 2026-09-05.

    Thousands separators, because these are the numbers on this site anybody
    reads as a magnitude rather than a value: 12510784 and 1251078 are one
    glance apart and an order of magnitude different.
    """
    declared, observed = verdict.declared_count, verdict.observed_count
    if declared is None and observed is None:
        return None
    if declared is not None and observed is not None:
        # Both, so the comparison is the story. Named in the order the verdict
        # grades them: the claim first, then what we found.
        return f"declares {declared:,}, counted {observed:,}"
    if observed is not None:
        return f"counted {observed:,}, declared nothing"
    # A claim we could not check. Said as a claim rather than as a fact, since
    # nothing here confirmed it.
    return f"declares {declared:,}, not counted"


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


# What each dormancy reason means in words, keyed by the slug the graph
# carries. The slugs are prober/src/dormancy.rs's SkipReason::slug, and the
# sentences differ in WHO decided and in what follows: "automatic" is this
# project's cost policy relegating an endpoint that proved expensive and silent
# over consecutive sweeps, "operator-hold" is a person putting it aside by hand.
# Reporting the second as the first would tell a reader the machine did
# something a person did, and would send an operator looking for a threshold to
# change.
#
# "not-in-this-sweep" is neither, and it is the reason this map cannot collapse
# into one sentence: a sweep re-running an instant that already ran asks exactly
# the endpoints that instant asked, so an endpoint it did not reach was not
# relegated and nothing about it was decided. Rule 3 of the admission policy
# used to publish "automatic" for those, which every one of these pages then
# rendered as a relegation that never happened.
#
# No sentence here says anything about the endpoint. Dormancy is a fact about
# this service's rotation, which is why the vocabulary calls it dormant rather
# than unresponsive and why it is not one of the six verdicts.
_DORMANCY_REASONS = {
    "automatic": (
        "this service relegated it after consecutive sweeps that cost a great "
        "deal and returned nothing"
    ),
    "operator-hold": "an operator put it aside by hand",
    "not-in-this-sweep": (
        "that sweep re-ran an instant that had already run, so it asked "
        "exactly the endpoints that instant asked before and this endpoint "
        "was not one of them, which is not a relegation and not a judgement "
        "about the endpoint"
    ),
}

# The clause for a reason this build has no sentence for, from a prober newer
# or older than this page. The same shape and the same reason as
# _UNRECOGNISED_DECLINE_DETAIL: the value is carried verbatim so a reader can
# see which value the store holds, and nothing is claimed about what it means.
_UNRECOGNISED_DORMANCY_REASON = "is one this page has no reading of"

# And the clause for a declaration with no sw:dormancyReason beside it. Every
# dormancy group the prober writes carries one, so this is the branch that
# exists because the DECLARATION is the fact the page turns on: a run that said
# it declined to ask and did not say why has still said the first half, and
# that half is what makes the crash sentence false.
_NO_DORMANCY_REASON = "it recorded no reason for the skip"


def _dormancy_clause(reason: str | None) -> str:
    """How the sentence below names the reason the newest sweep did not ask."""
    if reason is None:
        return _NO_DORMANCY_REASON
    if reason in _DORMANCY_REASONS:
        return f"{_DORMANCY_REASONS[reason]} ({reason})"
    return (
        f"the reason it recorded, {reason}, {_UNRECOGNISED_DORMANCY_REASON}"
    )


def _newest_sweep_silence_text(
    measurements: EndpointMeasurements,
) -> str | None:
    """The sentence for an endpoint the NEWEST sweep recorded nothing for.

    Said in the same place as the two sentences above, and for the same
    reason: the header sentence dates every verdict on this page to one sweep,
    and on this site an absent qualifier is a positive claim. Saying nothing
    here asserts that the sweep named up there is the newest one this service
    ran, and for a page that reaches this function that is false.

    Two claims, and which one is made is decided by the store rather than by
    preference:

    * the newest sweep DECLARED this endpoint dormant. Then it did not ask on
      purpose, it published why, and that is true whether or not it finished:
      the dormancy section is written whole, before the first chunk. This
      claim comes first because it is the more specific one and because the
      other two would both be saying less about the same sweep.
    * the newest sweep FINISHED and recorded nothing here, with no dormancy in
      the store. A url dropped from the registry, one added to
      registry/exclusions.toml and a deliberately narrowed sweep all produce
      this, so the sentence says what happened and refuses to say why.

    The third claim, a newer sweep that STOPPED before it got here, is
    _newer_unfinished_run_text's and is mutually exclusive with the second on
    newest_finalised. It is excluded from the first by
    newer_run_did_not_reach_this_endpoint's own dormancy gate, so no page can
    print a crash claim and a dormancy claim about one sweep.

    BOTH TIMESTAMPS ARE NAMED, and the verdicts' one comes from
    measurements.generated_at, which is this endpoint's own sw:currentRun. Not
    from newest_generated_at, ever: the sweep that declined to look is
    routinely the newest run in the store, so dating the verdicts by the
    store's greatest prov:generatedAtTime would report them as fresher than
    they are, by exactly the gap this sentence exists to disclose.
    """
    if measurements.newest_sweep_declined_to_ask_this_endpoint:
        return (
            f"The newest sweep, at {measurements.newest_generated_at}, did "
            f"not ask this endpoint: it recorded the endpoint as dormant and "
            f"{_dormancy_clause(measurements.newest_dormancy_reason)}. That "
            f"is a fact about this service's rotation and not a verdict about "
            f"the endpoint. Nothing above comes from that sweep. What is "
            f"above is the newest this store holds for this endpoint, from "
            f"the sweep at {measurements.generated_at}."
        )
    if measurements.newest_sweep_recorded_nothing_for_this_endpoint:
        return (
            f"The newest sweep, at {measurements.newest_generated_at}, "
            f"finished and recorded nothing at all for this endpoint, and no "
            f"run in this store says why. Nothing above comes from that "
            f"sweep. What is above is the newest this store holds for this "
            f"endpoint, from the sweep at {measurements.generated_at}."
        )
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
            f"This run did not look at this endpoint's classes at all, "
            f"recording the reason '{declined.reason}'. Nobody looked in this "
            f"run, so it reports nothing about what classes the endpoint holds."
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
    # These two sentences DO name the classes metric, unlike the decline and
    # fall-through above, and that is deliberate: only the retired metric can
    # reach here (see _measured_classes), so a run taking this branch really did
    # measure it and naming it is the precise thing to say.
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
        "No class sample from this run, and this run recorded no account of "
        "looking for one either. This page therefore says nothing about what "
        "classes the endpoint holds."
    )
    return sample


def _declined_classes(measurements: EndpointMeasurements):
    """This run's decline of a class-sampling metric, if it declined one.

    Across CLASS_SAMPLING_METRICS and not the one retired id, because which
    metric a cheap sweep declines changed on 2026-09-04: it is class-profiles
    now, and every committed fixture predates that and declines classes. Keyed
    on one id, a run that HAD declined the sampling metric and recorded why
    fell through to the sentence reserved for a run that never looked.

    In definition order, so the current scheme's decline is the one reported
    when a transitional run declined both.
    """
    by_metric = {d.metric: d for d in measurements.declined}
    for metric in CLASS_SAMPLING_METRICS:
        if metric in by_metric:
            return by_metric[metric]
    return None


def _measured_classes(measurements: EndpointMeasurements):
    """This run's measurement of a class-sampling metric, if it measured one.

    Only the retired classes metric can ever match: class-profiles publishes no
    measurement row at all (prober's ProbeKind::yields_measurement), so a run
    that ran the pass has no verdict here for any caller to read. That is not a
    gap to work around, it is Ruling 2, and the list is walked anyway so this
    function needs no separate notion of which metrics are in play.
    """
    by_metric = {v.metric: v for v in measurements.verdicts}
    for metric in CLASS_SAMPLING_METRICS:
        if metric in by_metric:
            return by_metric[metric]
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
    history: EndpointHistory,
    vocabulary: list[dict],
    void: dict | None,
) -> dict:
    """Everything the template renders, decided here rather than in the page.

    The template loops and formats; it makes no judgement about what a missing
    sample means or how a state is drawn. Both of those are decisions with a
    right answer, and they belong where they can be tested.
    """
    rows = _rows(measurements)
    return {
        **_nav_context(),
        # The timeline is its OWN section rather than a span on each row. The
        # rows answer "what is true now" and the history answers "what has
        # changed", which is the same split the index makes, and a sparkline
        # squeezed onto a row cannot carry an axis, a hover readout, or dates.
        "history": _history_view(history, rows),
        # The vocabulary this endpoint holds, searchable in the page. Passed as
        # data rather than pre-filtered markup because the search is the point:
        # a reader types a name and the list narrows without a round trip.
        "vocabulary": vocabulary,
        # The derived description this endpoint does not publish for itself,
        # and the one thing a reader has to know before using it. Read back out
        # of the document rather than worked out again here: two answers to
        # "is this complete" would eventually differ in front of somebody
        # deciding whether to depend on it.
        "void": void,
        "void_path": VOID_PATH,
        "vocabulary_json": json.dumps(vocabulary, separators=(",", ":")),
        # Oldest first, and only where there is more than one: a single-cell
        # timeline implies a trend from one observation.
        "history_runs": history.runs if history.has_history else [],
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
        #
        # The second and the third are mutually exclusive: a later sweep that
        # stopped and a later sweep that recorded nothing on purpose are two
        # readings of one sweep, and only one of them can be right.
        "unfinished_text": _unfinished_run_text(measurements),
        "newer_unfinished_text": _newer_unfinished_run_text(measurements),
        # The third thing that can be wrong with the sweep named above, and it
        # is not about that sweep at all: a NEWER sweep recorded nothing here,
        # so the timestamp above is not the newest this service holds. Its own
        # sentence, for the same reason as the two above, and the two dormancy
        # keys beside it are the machine-readable half of it. The reason slug
        # is passed through rather than the flag alone, so a consumer of the
        # page reads the value the store holds and not this build's reading of
        # it; the sentence carries the reading.
        "newest_silence_text": _newest_sweep_silence_text(measurements),
        "newest_declined_to_ask": (
            measurements.newest_sweep_declined_to_ask_this_endpoint
        ),
        "newest_dormancy_reason": measurements.newest_dormancy_reason,
        # And where a reader of that sentence goes next. The sentence says what
        # happened; /about is the only surface that says what the mark claims,
        # what it does not, and who changes it, and until this stage no page
        # linked to it. In the header for every reader, and once more inside the
        # dormancy sentence for the one it is addressed to.
        "about_path": ABOUT_PATH,
        # The way home, on every page. The logo carries it, so a reader who
        # arrived on one endpoint from a search engine has somewhere to go
        # other than the back button.
        "index_path": INDEX_PATH,
        "docs_path": DOCS_PATH,
        "explore_path": EXPLORE_PATH,
        # The endpoint itself, in a new tab, when its scheme is one a browser
        # should follow. See _outward_link: this is the only href on this site
        # holding a string a third party chose.
        "outward_link": _outward_link(endpoint),
        "rows": rows,
        "legend": _legend(rows),
        "sample": _sample(measurements, content),
        "chip_width": verdict_encoding.CHIP_WIDTH_PX,
        "chip_height": verdict_encoding.CHIP_HEIGHT_PX,
    }


def _endpoint_html(
    endpoint: str,
    measurements: EndpointMeasurements,
    content: EndpointContent,
    history: EndpointHistory,
    vocabulary: list[dict],
    void: dict | None,
) -> str:
    """The page, rendered."""
    return _TEMPLATES.get_template("endpoint.html").render(
        **_page_context(endpoint, measurements, content, history, vocabulary, void)
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
            content=_endpoint_html(
                url,
                measurements,
                content,
                endpoint_history(store, url),
                endpoint_vocabulary(store, url),
                void_summary(store, url),
            ),
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
# same way CLASS_SAMPLING_METRICS is for the sample.
_AVAILABILITY_METRIC = _METRIC_PREFIX + "availability"

# The two words a qualified row carries, and they are words rather than a
# colour or a title attribute: a marker only a mouse can find qualifies
# nothing. Each is explained in a full sentence in the page's key, which is
# where the reasoning that will not fit in three words lives.
ROW_UNFINISHED_TEXT = "from a sweep that stopped"
ROW_NEVER_REACHED_TEXT = "a later sweep never got here"

# The third qualifier, and it is a QUALIFIER and not an eighth state. Dormancy
# is a fact about this service's rotation: the newest sweep published this
# endpoint as one it declined to ask, and said why. So it gets no chip, no
# metric column and no entry in _legend, which counts chip states from the
# closed table in verdict_encoding; it sits here beside the other two things
# that can be wrong with the instant in the page's header, and the panel that
# explains those explains it.
#
# What it licenses is a sentence about how old the verdicts on the row are, and
# nothing about the endpoint. The row stays in the group its last real probe put
# it in, with the chips that probe produced: moving it out would file a measured
# endpoint under a heading about us.
ROW_DORMANT_TEXT = "the newest sweep did not ask"

# The reason, twice: the clause a row carries, and the gloss the page's panel
# explains it with. Three spellings today, and each is a different fact:
# "automatic" is the admission policy relegating an endpoint that cost more than
# its ceiling while answering nothing in two consecutive sweeps, "operator-hold"
# is a person setting it aside, and "not-in-this-sweep" is a sweep replaying an
# instant that had already run and so asking exactly the set that instant asked.
# A row that read the same for any two of them would say less than the attribute
# beside it already does.
#
# ONE MAP AND NOT TWO, because the panel used to spell these slugs out in
# the template. The prober owns them (prober/src/dormancy.rs's
# SkipReason::slug), so a rename there left every row telling the truth through
# the verbatim fallback while the panel went on naming a value no run graph could
# carry, with every test green. The keys are now pinned to that match in
# test_about.py's test_the_dormancy_reasons_the_pages_read_are_the_probers_own,
# and the panel renders the keys of this map rather than repeating them, so the
# chain runs from the Rust arm to the words a reader sees.
# The gloss carries the whole explanation, so that the panel needs no sentence of
# its own about a named reason. It used to have one ("Automatic means the
# endpoint cost more than its ceiling ..."), which is prose about one slug that
# nothing pins: a third reason, or a rename, left it standing. The strike count
# in it is a format field for the same reason the cadence is, see below.
#
# The cadence lives in these glosses too, and no longer in a sentence of the
# template's own. The panel used to end "Either way the endpoint is asked at
# most one sweep in every N days", which is true of exactly one of the three
# reasons: a hold is asked by no sweep at all until a person lifts it, and a
# replay that did not reach an endpoint changed nothing about how often it is
# asked. One sentence covering three reasons is how that got written, so each
# reason now carries what follows from it and the template covers none of them.
_ROW_DORMANCY_REASONS = {
    "automatic": (
        "on cost and silence",
        "the admission policy setting it aside after {strikes} sweeps in a row "
        "that cost more than its ceiling and answered nothing, after which it "
        "is asked at most one sweep in every {cadence} days and only when a "
        "person starts one",
    ),
    "operator-hold": (
        "by hand",
        "a person setting it aside by hand, after which no sweep asks it at "
        "all until a person lifts the hold",
    ),
    "not-in-this-sweep": (
        "on a replay of an earlier sweep",
        "a sweep re-running an instant that had already run, which asks "
        "exactly the endpoints that instant asked before and so did not reach "
        "this one, leaving how often it is asked exactly as it was",
    ),
}

# The fourth thing a row can say, and it is provenance rather than a warning:
# WHICH sweep measured this row, said whenever that is not the sweep the page's
# header names. Until this stage nothing on a row said whether the header's
# instant had anything to do with the verdicts beside it, and on this site an
# absent qualifier is a positive claim, so a store whose newest sweep asked a
# narrow subset presented every other row's older verdicts as measured then.
#
# WORDED AGAINST THE SWEEP, NEVER AS AN AGE FROM TODAY. Nothing in this service
# runs on a schedule, so "six days ago" is a claim the store cannot support: it
# holds two instants and nothing about how long is expected between two sweeps.
# _provenance sets the rule this follows, that the two timestamps are named and
# never ordered.
ROW_MEASURED_BY_TEXT = "measured by the sweep at"

# What an empty cell in a metric's column means. It is NOT one of the seven
# states and it is deliberately not drawn as one: a metric this endpoint's
# newest run recorded neither a measurement nor a decline for is a gap in what
# the store holds, and drawing it as "not measured" would claim the run said so.
EMPTY_CELL_TEXT = ""
# (
    # "A cell holding only a dot means this endpoint's newest run recorded "
    # "nothing at all about that metric, neither a measurement nor a decline. It "
    # "is a gap in what this service holds, not a verdict about the endpoint, "
    # "which is why it is not drawn as one of the states below."
#)


def _row_dormancy_text(reason: str | None) -> str:
    """The words a dormant row carries, reason included.

    Three branches, and they are the three _dormancy_clause has on the endpoint
    page, said in fewer words. A reason this build has no reading of is carried
    VERBATIM rather than dropped or relabelled, because the value the store
    holds is the fact and this build's reading of it is not; a declaration with
    no reason at all still made the declaration, which is the half that matters
    here, so it gets the marker and says the reason is missing.
    """
    if reason is None:
        return f"{ROW_DORMANT_TEXT}, and gave no reason"
    if reason in _ROW_DORMANCY_REASONS:
        return f"{ROW_DORMANT_TEXT}, {_ROW_DORMANCY_REASONS[reason][0]}"
    return f"{ROW_DORMANT_TEXT}, for a reason this page cannot read: {reason}"


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


# The order the matrix draws its columns in, left to right.
#
# NOT alphabetical, which is what it was until 2026-09-05 and which put
# `class-count` second and `cors` third: a reading order decided by spelling.
# This groups by what a reader is asking. Does it answer at all, does it
# describe itself, how big is it, does it describe what it holds, what kind of
# data, and can a browser reach it.
#
# A metric missing from here still gets a column, appended alphabetically after
# these. A newer prober's metric must not vanish from the grid because this
# build has never heard of it; an unexplained column at the end is the lesser
# error, and the same choice `_yields_measurement` makes about an unknown kind.
METRIC_COLUMN_ORDER = (
    "availability",
    "service-description",
    "triple-count",
    "graph-count",
    "class-count",
    "vocabulary-described",
    "geo-data",
    "geo-functions",
    "cors",
    "cors-preflight",
)


def _column_rank(metric: str) -> tuple[int, str]:
    """Where this metric's column sits, and the tiebreak for one not listed.

    The second half of the key is the metric id, so two unlisted metrics keep a
    stable order between two identical requests: SPARQL solution order is not
    specified, and columns that moved would look like the metric set changed.
    """
    name = metric.removeprefix(_METRIC_PREFIX)
    try:
        return (METRIC_COLUMN_ORDER.index(name), "")
    except ValueError:
        return (len(METRIC_COLUMN_ORDER), name)


def _yields_measurement(metric: str) -> bool:
    """Whether this metric can produce a verdict, and so deserves a column.

    The matrix is a grid of verdicts. A metric that publishes none has nothing
    to draw in it, and drawing a gap instead states that the run recorded
    nothing about that metric, which for a successful profile pass is false.

    Unknown metrics are assumed to measure. A metric this build has never heard
    of is one from a newer prober, and hiding its column would lose a real
    verdict; showing an unexplained one is the lesser error and matches how
    _detail treats an unrecognised verdict. The prober says the same thing on
    its side in ProbeKind::yields_measurement.
    """
    name = metric.removeprefix(_METRIC_PREFIX)
    return METRIC_DOCS.get(name, {}).get("yields_measurement", True)


def _index_metrics(entries: list[EndpointMeasurements]) -> list[dict]:
    """Every metric any row has a fact for, in metric id order.

    Sorted rather than left in the order the store returned them, for the same
    reason EndpointMeasurements sorts its verdicts: SPARQL solution order is
    not specified, and columns that moved between two identical requests would
    look like the metric set had changed.
    """
    metrics = sorted(
        {
            metric
            for metric in (
                {verdict.metric for entry in entries for verdict in entry.verdicts}
                | {declined.metric for entry in entries for declined in entry.declined}
            )
            if _yields_measurement(metric)
        },
        key=_column_rank,
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


# ---------------------------------------------------------------------------
# The three facet groups above the rows
# ---------------------------------------------------------------------------
# Every count here is computed over the same rows the page renders, and every
# chip filters by reading attributes the rows ALREADY carry: a cell's state is
# its `enc-<slug>` class, a measured cell carries `data-verdict` and a declined
# one carries `data-declined`, and a chip's text is the metric's abbreviation.
# So faceting cost this page nothing per row, which matters because
# web/README.md's table shows how little headroom 543 rows leave.
#
# WHAT EACH GROUP SELECTS was decided by the plan owner on 2026-08-27, against
# the counts these functions return over the 2026-08-24 sweep. Two of the three
# select very little on that data and the reason is coverage rather than the
# facet: `classes` is the only expensive metric and every sweep so far ran at
# the cheap ceiling, so seven metric chips match all 543 rows and one matches
# none. That is the gap the metric chips exist to show.

# The availability facet is TWO chips and not three, which is a decision with a
# cost the plan owner took knowingly. 482 of 543 endpoints read `indeterminate`,
# meaning no answer arrived inside the budget, and this page files them under
# "not available" along with the 4 that answered `absent`. The six-verdict
# vocabulary exists precisely to keep those apart, so the chip DISCLOSES ITS
# COMPOSITION rather than leaving the reader to assume 486 servers are down:
# the filter is what was asked for, and the label is what stops it lying.
_POSITIVE_VERDICTS = ("verified", "undeclared-but-verified")

# The one verdict inside "not available" that is not a finding about the server
# at all: no answer arrived inside the time budget. Named here rather than
# spelled at the one place it is counted, because the disclosure sentence below
# is entirely about this value and a rename that missed it would leave the
# sentence saying nothing while the chip went on counting.
_UNREACHED_VERDICT = "indeterminate"


# The one place this service links OUT to an endpoint, and the only place any
# page puts a third party's string in an href. That is why it is gated on the
# scheme rather than rendered straight from the store.
#
# Autoescaping does not help here. `javascript:alert(1)` contains no character
# an HTML escaper touches, so it reaches the attribute unchanged and a browser
# runs it on click. The spec's own note is what makes this reachable rather than
# theoretical: the scheme allowlist deferred at stage 1d-a was found
# "unnecessary FOR THIS DUMP, which holds only http (472) and https (76)", and
# explicitly "not retired in general: a later source may differ". Stage 5
# accepts public submissions, and `load_run` loads whatever run file it is
# given. So this link is what would have turned that deferral into a hole.
#
# An endpoint whose scheme is anything else keeps its page and its verdicts and
# simply gets no link. Saying nothing is right: this service has no opinion on
# such a URL, and the page already prints it in full as text where a reader can
# see it and decide for themselves.
LINKABLE_SCHEMES = ("http://", "https://")


def _outward_link(endpoint: str) -> str | None:
    """The endpoint's own URL when it is safe to put in an href, else None."""
    lowered = endpoint.lower()
    if any(lowered.startswith(scheme) for scheme in LINKABLE_SCHEMES):
        return endpoint
    return None


def _state_facets(rows: list[dict]) -> list[dict]:
    """One chip per encoding state, counting rows with AT LEAST ONE chip in it.

    Changed from uniform-in-that-state on 2026-08-28 by the plan owner, and the
    numbers are why. Over the 2026-08-24 sweep the uniform reading gave
    `indeterminate` 402 and every other state 0, because no endpoint on this
    registry is uniform in anything else: six of the seven chips were dead.
    Counting a row that has the state anywhere gives verified 84,
    undeclared-but-verified 18, declared-but-wrong 7, absent 115, indeterminate
    532, not-measured 543, declared-only 0. So one press now answers the
    question a reader actually brings to this page, which is "who gets anything
    wrong" rather than "who gets everything wrong".

    EVERY DRAWN CHIP COUNTS, a declined one included, which is the other half of
    the change. Under the uniform reading declines had to be excluded or every
    count was 0; under this one they are the whole point of the `not-measured`
    chip, and its 543 says every endpoint here has a metric no sweep has run.
    That also makes this number and the legend's chip count populations of the
    same thing, which is what lets the two sit in one button.

    Built from the same table as `_legend`, so a chip and its swatch cannot
    disagree about what a state looks like or is called, and it emits the
    unrecognised state on the same condition `_legend` lists it: a cell was
    DRAWN in it. A store carrying a verdict this build has no encoding for used
    to give that row a blank count that then revealed rows when pressed.
    """
    holding: dict[str, int] = {}
    for row in rows:
        for slug in {cell["slug"] for cell in row["cells"] if cell["present"]}:
            holding[slug] = holding.get(slug, 0) + 1
    states = list(verdict_encoding.STATES)
    if holding.get(verdict_encoding.UNRECOGNISED.slug):
        states.append(verdict_encoding.UNRECOGNISED)
    return [
        {
            "slug": state.slug,
            "label": state.label,
            "css_class": verdict_encoding.css_class(state.slug),
            "count": holding.get(state.slug, 0),
        }
        for state in states
    ]


# What each metric asks, for the tooltip on its name in the grid.
#
# THE SOURCE OF TRUTH IS prober/metrics.toml, whose `label` field these are, and
# the run graphs do not carry it: a measurement names its metric by IRI and
# nothing publishes a label for that IRI, so the web tier cannot read these out
# of the store the way it reads everything else it says. The choices were to
# teach the prober to publish them, which shows nothing until a new sweep runs,
# to read the prober's file from the web tier at runtime, which couples the
# service to a layout it otherwise never touches, or this: keep the words here
# and PIN THEM WITH A TEST that reads prober/metrics.toml and fails when the two
# drift. The last is what test_about.py already does for the politeness numbers
# and the dormancy slugs, so it is this project's answer to exactly this shape.
#
# A metric with no entry here gets no tooltip rather than an invented one.
METRIC_DESCRIPTIONS = {
    "vocabulary-described": "Describes its own vocabulary",
    "triple-count": "States how many triples it holds",
    "graph-count": "States how many graphs it holds",
    "class-count": "States how many classes it holds",
    "availability": "Answers a trivial query",
    "cors": "Sends access-control-allow-origin on a simple GET",
    "cors-preflight": "Answers a CORS preflight for a cross-origin GET",
    "geo-functions": "GeoSPARQL relation functions",
    "geo-data": "Holds WKT geometry",
    "service-description": "Service description informativeness",
    "has-classes": "Holds typed resources",
    "classes": "Distinct classes",
}


def _metric_state_matrix(
    rows: list[dict], metrics: list[dict]
) -> list[dict]:
    """One row per metric, one cell per state, counting the endpoints in each.

    The layout the plan owner asked for on 2026-08-28, and the shape of the real
    registry is the argument for it. Over the 2026-08-24 sweep 21 of the 56
    cells hold anything, and the 21 are not spread evenly: `geo-functions` is
    the ONLY metric with an undeclared-but-verified column (18) or a
    declared-but-wrong one (7), `classes` is entirely not-measured, and
    `declared-only` is empty everywhere. That is the survey finding this project
    was built to reproduce, and two separate chip strips could not show it: one
    said 18 endpoints work undeclared somewhere, the other said 543 endpoints
    have a geo-functions verdict, and neither said the two were the same fact.

    A cell counts the endpoints whose newest run put THAT metric in THAT state,
    so the cells of one row sum to that metric's own total and never to the page
    total. A metric the run declined lands in the not-measured column rather
    than being left out, because the reader's question there is "did anyone
    look", and the column answers it.
    """
    counted: dict[tuple[str, str], int] = {}
    for row in rows:
        for cell in row["cells"]:
            if cell["present"]:
                key = (cell["name"], cell["slug"])
                counted[key] = counted.get(key, 0) + 1
    states = list(verdict_encoding.STATES)
    if any(
        slug == verdict_encoding.UNRECOGNISED.slug for _, slug in counted
    ):
        states.append(verdict_encoding.UNRECOGNISED)
    return [
        {
            "name": metric["name"],
            "abbr": metric["abbr"],
            "description": METRIC_DESCRIPTIONS.get(metric["name"]),
            # The row's own total, so a reader can see at a glance that the
            # cells beside it account for every endpoint and none twice.
            "total": sum(
                n for (name, _), n in counted.items() if name == metric["name"]
            ),
            "cells": [
                {
                    "slug": state.slug,
                    "label": state.label,
                    "css_class": verdict_encoding.css_class(state.slug),
                    "count": counted.get((metric["name"], state.slug), 0),
                    # "AV|verified". The script's predicate needs the pair and
                    # the abbreviation is what a chip on a row already carries,
                    # so nothing per row has to grow to support this.
                    "value": f"{metric['abbr']}|{state.slug}",
                }
                for state in states
            ],
        }
        for metric in metrics
    ]


def _matrix_states(matrix: list[dict]) -> list[dict]:
    """The column headers, taken from the matrix so the two cannot disagree."""
    if not matrix:
        return []
    return [
        {
            "slug": cell["slug"],
            "label": cell["label"],
            "css_class": cell["css_class"],
            # The state's own colour, for the label text. See
            # verdict_encoding.text_class on why it is a second class and not a
            # `color` folded into the chip rule.
            "text_class": verdict_encoding.text_class(cell["slug"]),
            # The meaning verdict_encoding already carries. It was printed as
            # prose under the legend until the grid replaced it, and the prose
            # under the grid went on 2026-08-28, so this is where it lives now:
            # on the thing it describes rather than in a paragraph beneath it.
            "description": verdict_encoding.presentation(cell["slug"]).meaning,
        }
        for cell in matrix[0]["cells"]
    ]


# The counting metrics whose numbers a row summarises, and the word each one
# counts. Ordered largest unit first, which is the order somebody sizing up an
# endpoint reads them in.
_ROW_SIZES = (
    ("triple-count", "triples"),
    ("graph-count", "graphs"),
    ("class-count", "classes"),
)


def _row_size(entry: EndpointMeasurements) -> list[dict]:
    """What this endpoint holds, for the row, as far as anything counted it.

    THE COUNTED NUMBER AND NEVER THE DECLARED ONE. A row is this service's own
    reading, and an endpoint's claim about its size belongs to the endpoint:
    showing 1,000,000 in the listing because a description said so, beside a
    verdict saying that claim is wrong, would put the wrong number in the place
    a reader actually looks. The endpoint page shows both, which is where the
    comparison is the subject.

    Absent rather than zero where nothing counted. A row that reads "0 triples"
    for an endpoint nobody counted is the confident false negative this project
    exists to prevent, and at the default cheap ceiling nothing counts anything.
    """
    counted = {
        v.metric.removeprefix(_METRIC_PREFIX): v.observed_count
        for v in entry.verdicts
        if v.observed_count is not None
    }
    return [
        {"n": counted[metric], "unit": unit}
        for metric, unit in _ROW_SIZES
        if metric in counted
    ]


def _index_row(
    entry: EndpointMeasurements, metrics: list[dict], explorable: frozenset[str]
) -> dict:
    """One row: the endpoint, its link, its cells, and any qualification.

    The link is percent-encoded with nothing left safe, because the endpoint
    resource names its endpoint in a url query parameter and a query string is
    unquoted exactly once: an endpoint URL holding a '&' or a '#' left bare
    would arrive truncated, and one holding a percent sequence of its own would
    arrive as a different URL. See "The URL shape" at the top of this file.
    """
    dormant = entry.newest_sweep_declined_to_ask_this_endpoint
    # The condition for naming this row's own sweep, and it is deliberately the
    # WIDEST of the four conditions on this row: any row whose facts did not
    # come from the newest run in the store, for any reason at all. The two
    # properties about a NEWER sweep each say something further about why that
    # run holds nothing here, and a row can satisfy neither and still be a row
    # of older facts under a header dated later. store_later_sample is that
    # store: its newest run sampled one endpoint and measured nothing, so no
    # crash claim and no "recorded nothing" claim holds of any of its three
    # rows and all three of them are the 16:00 sweep's. Naming the sweep is true
    # of every one of these cases, so it is asked as one question.
    #
    # Both halves of the guard are required, and the first is not redundant:
    # None != entry.run is True, so a store whose newest run is unbound would
    # otherwise read as "a run other than this one" and every row would claim a
    # second sweep exists. This is the same guard the three properties on
    # EndpointMeasurements state, for the same reason.
    older_sweep = entry.newest_run is not None and entry.newest_run != entry.run
    # The registry's word, not the store's: see registry_names.py on why a
    # catalogue title is read from the registry files and never from a run
    # graph. name is None for an endpoint no loaded registry names at all,
    # which display() and the host/datasets/domain fallbacks below all handle.
    name = _NAMES.get(entry.endpoint)
    return {
        "endpoint": entry.endpoint,
        "href": ENDPOINT_PATH + "?url=" + quote(entry.endpoint, safe=""),
        "name": display(name, entry.endpoint),
        "host": name.host if name else entry.endpoint,
        "datasets": name.datasets if name else None,
        "domain": name.domain if name else None,
        # Present only where the explorer has something to show. See
        # explore_endpoints on why a link to an empty explorer would be a claim
        # rather than a convenience.
        "content_href": (
            EXPLORE_PATH + "?endpoint=" + quote(entry.endpoint, safe="")
            if entry.endpoint in explorable
            else None
        ),
        "size": _row_size(entry),
        "cells": _index_chips(entry, metrics),
        "run_unfinished": entry.run_did_not_finish,
        "never_reached": entry.newer_run_did_not_reach_this_endpoint,
        "dormant": dormant,
        # The value the store holds, passed through beside the sentence, the
        # same way the endpoint page passes it: a consumer of the markup reads
        # the store's word and not this build's reading of it.
        "dormancy_reason": entry.newest_dormancy_reason if dormant else None,
        "dormant_text": (
            _row_dormancy_text(entry.newest_dormancy_reason) if dormant else None
        ),
        # This endpoint's OWN sweep, from its own sw:currentRun pointer, and
        # never newest_generated_at: the sweep that declined to look is
        # routinely the newest run in the store, so dating these verdicts by the
        # store's greatest prov:generatedAtTime would report them as fresher
        # than they are by exactly the gap this marker exists to disclose.
        "measured_by": entry.generated_at if older_sweep else None,
    }


def _index_rows(
    entries: list[EndpointMeasurements], metrics: list[dict], explorable: frozenset[str]
) -> list[dict]:
    """Every row, alphabetical by endpoint. One listing.

    This grouped rows by the availability verdict's own value until 2026-08-28,
    one section per value present in verdict_encoding.STATES' order, with a
    heading naming the value and its denominator. The plan owner removed the
    headings, and with nothing rendering the grouping the whole apparatus was
    vestigial: the template used none of the eight fields a group carried except
    its rows, and no reader could see the availability value a row was filed
    under. What survives the removal is where that information now comes from,
    which is better than a heading: the grid's availability ROW states 57
    verified, 482 indeterminate and 4 absent, keeping apart the two the
    headings' own available-or-not reading merged.

    ALPHABETICAL, which the removal forces as a decision rather than leaving as
    a residue. The old comment here said the group order "is the encoding
    table's and not any notion of better or worse", because "ranking the groups
    would be this service's opinion about the endpoints". That was true while
    the groups were labelled and the page said as much. Unlabelled, the same
    order is an unexplained ranking with the sentence that excused it deleted,
    so the honest flattening is the one that ranks nothing: the endpoint's own
    url, which is also what the search box above filters on and what a reader
    scanning for one is scanning for.
    """
    return sorted(
        (_index_row(entry, metrics, explorable) for entry in entries),
        key=lambda row: row["endpoint"],
    )


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


def _short_endpoint(url: str) -> str:
    """Host and enough path to tell two services on one host apart.

    The grid's row labels are a column of urls, and the scheme and a repeated
    `/sparql` are the parts none of them differ by.
    """
    parsed = urlparse(url)
    host = parsed.hostname or url
    if parsed.port:
        host = f"{host}:{parsed.port}"
    path = parsed.path.rstrip("/")
    return host if path in ("", "/sparql", "/query") else f"{host}{path}"


def _fleet_view(history: FleetHistory) -> dict:
    """The overview grid, with every cell already resolved to how it is drawn.

    Resolved here rather than in the template for the reason every other
    decision on this page is: how a state is drawn has a right answer and
    belongs where it can be tested.
    """
    def cell(at: str, verdict: str | None) -> dict:
        if verdict is None:
            return {"present": False, "at": at}
        state = verdict_encoding.presentation(verdict)
        known = state is not verdict_encoding.UNRECOGNISED
        return {
            "present": True,
            "at": at,
            "css_class": verdict_encoding.css_class(state.slug),
            # The store's own word where this build does not know the state, as
            # every other reading on this site does.
            "label": state.label if known else verdict,
        }

    return {
        "runs": history.runs,
        "has_history": history.has_history,
        # ONLY THE ENDPOINTS THAT MOVED, with the rest counted beside them.
        # This grid is 543 rows at registry scale, and the great majority of
        # them are one state repeated: a reader looking for what changed would
        # be looking for it among rows that did not. Every endpoint is still in
        # the listing below, with its own page and its own full timeline.
        "steady": len(history.steady),
        "rows": [
            {
                "endpoint": row.endpoint,
                "short": _short_endpoint(row.endpoint),
                "href": ENDPOINT_PATH + "?url=" + quote(row.endpoint, safe=""),
                "changed": row.changed,
                "cells": [cell(at, v) for at, v in zip(history.runs, row.cells)],
            }
            for row in history.changed
        ],
    }


def _matches_query(endpoint: str, needle: str | None) -> bool:
    """Whether one endpoint answers to a search.

    Case-insensitive substring over the endpoint URL, its host, and the title
    the registry carries for it.

    This is ONE function on purpose, and it now reads _NAMES for the same
    reason: both representations of the index filter through it, so the page
    and the data agree by construction. A title that only the HTML could match
    would make ?q= mean two different things at one URL.
    """
    if not needle:
        return True
    needle = needle.strip().lower()
    if needle in endpoint.lower():
        return True
    name = _NAMES.get(endpoint)
    if name is None:
        return False
    return bool(
        (name.title and needle in name.title.lower())
        or needle in name.host.lower()
    )


# Stands in for an endpoint no registry names at all, so _matches_domain never
# branches on whether _NAMES holds an entry: an undomained endpoint's .domain
# reads None either way, exactly like the 118 of the real registry's 552 that
# no catalogue placed in a domain. A domain pill can therefore never be the
# only way to narrow the list -- those 118 would vanish from every domain at
# once -- which is a fact about the data, not a rule this function enforces.
_NO_NAME = Name(None, None, None, "")


def _matches_domain(endpoint: str, domain: str | None) -> bool:
    """Whether one endpoint carries a given registry domain.

    Reads _NAMES for the reason _matches_query does: this is the ONE place
    that decides what `?domain=` means, so the RDF branch (_only_matching_
    endpoints) and the HTML branch (_index_context) share it and cannot
    disagree about which endpoints a domain names.
    """
    if not domain:
        return True
    return (_NAMES.get(endpoint) or _NO_NAME).domain == domain


# The metric each named, non-domain facet grades, and the label its pill
# shows. Kept as two small tables rather than separate booleans, so a reader
# sees at a glance that a facet is either a real metric, read positive-or-not
# ("void"), or a fact this page already names elsewhere, reused rather than
# renamed ("answering").
#
# A THIRD FACET, "federates", stood here until fix-round-1 and was removed.
# sparqlwatch measures eleven things (see prober/metrics.toml) and federation
# -- one endpoint's query reaching into another -- is not one of them. The
# first draft read a positive `cors` verdict as "federates", on the argument
# that access-control-allow-origin is a precondition a browser-based
# federator needs. That is a real fact about `cors`, but a pill LABELLED
# "federates" tells a reader this service established something it never
# measured, which is the exact assertion the page's own "measured rather
# than asserted" sentence -- and the reason a catalogue's title is
# attributed rather than claimed -- exists to rule out. If a filter on `cors`
# is wanted here later, it is honest under a label that says what it reads:
# "CORS", not what a reader might infer from it.
_FACET_METRICS = {
    "void": _METRIC_PREFIX + "vocabulary-described",
}
_FACET_LABELS = {
    "answering": "answering",
    "void": "declares VoID",
}


def _matches_facet(entry: EndpointMeasurements, facet: str | None) -> bool:
    """Whether one endpoint answers a fixed, named question, from the
    verdicts its newest run already recorded -- no query the prober has not
    already run.

    Two questions:

      "answering"  the newest sweep did not decline to ask this endpoint at
                    all. The same fact the strip above this listing already
                    labels "answering" (see `summary`); this reuses it rather
                    than naming the same thing twice.

      "void"       its `vocabulary-described` verdict is positive: the
                    endpoint's own description is confirmed to name the
                    classes its data actually holds -- which is exactly what
                    the metric measures, so "declares VoID" is not a claim
                    beyond it (see `_FACET_METRICS`'s comment on why
                    "federates" failed this same check and was removed).

    `_POSITIVE_VERDICTS` is the same table the availability facet above was
    built from: "verified" and "undeclared-but-verified" both mean the fact
    holds, declared or not, and only the declared HALF of that -- verified
    alone -- would undercount an endpoint this project's own philosophy says
    to credit: see prober/metrics.toml's note on geo-functions, "the rare
    honest endpoint is credited".
    """
    if not facet:
        return True
    if facet == "answering":
        return not entry.newest_sweep_declined_to_ask_this_endpoint
    metric = _FACET_METRICS.get(facet)
    if metric is None:
        return False
    return any(
        v.metric == metric and v.verdict in _POSITIVE_VERDICTS
        for v in entry.verdicts
    )


def _index_pills(
    entries: list[EndpointMeasurements], domain: str | None, facet: str | None
) -> list[dict]:
    """The registry's most common questions, as links above the rows.

    Built from `entries` AFTER `?q=`, `?domain=` and `?facet=` have already
    narrowed it, so a pill's count states what is on the page in front of the
    reader right now, not a fact about the whole registry a search has
    already cut away from. See the "Global constraints" note this task was
    written against: two chip strips on this page have printed a count that
    outlived the page it described before, and this is the fix repeated
    rather than a fresh idea.

    Five pills: the three commonest registry domains this filtered set holds
    (fewer than three where fewer than three are present -- store_registry_
    sample's nine endpoints, for instance, split across five), plus the two
    fixed questions _matches_facet answers. Both groups share one shape,
    {label, param, value, count, on}, so the template loops over them once.
    """
    domains = Counter(
        d for e in entries if (d := (_NAMES.get(e.endpoint) or _NO_NAME).domain)
    )
    pills = [
        {
            "label": value,
            "param": "domain",
            "value": value,
            "count": count,
            "on": domain == value,
        }
        for value, count in domains.most_common(3)
    ]
    pills += [
        {
            "label": label,
            "param": "facet",
            "value": value,
            "count": sum(1 for e in entries if _matches_facet(e, value)),
            "on": facet == value,
        }
        for value, label in _FACET_LABELS.items()
    ]
    return pills


def _no_match_description(q: str | None, domain: str | None, facet: str | None) -> str:
    """What to tell a reader when every active filter together matched
    nothing, naming each one that is actually set.

    Fix-round-1: the message used to be fixed to `?q=` alone -- "No endpoint
    in this registry matches “{{ query }}”" -- because `?q=` was the
    only filter that could ever narrow a page to zero rows. Once `?domain=`
    and `?facet=` can too, that sentence is wrong twice over for a page a
    domain or a facet alone emptied: it would print an empty pair of quotes
    (`query` is None) rather than naming the domain or facet that actually
    matched nothing, and a combination of two active filters would name only
    one of them. Every filter this call was given a value for is named, and
    none that was not.
    """
    clauses = []
    if q:
        clauses.append(f"the text “{q}”")
    if domain:
        clauses.append(f"the domain “{domain}”")
    if facet:
        clauses.append(f"the facet “{facet}”")
    if not clauses:
        return ""
    if len(clauses) == 1:
        return clauses[0]
    return ", ".join(clauses[:-1]) + " and " + clauses[-1]


def _index_context(
    entries: list[EndpointMeasurements],
    store: Store,
    q: str | None = None,
    domain: str | None = None,
    facet: str | None = None,
) -> dict:
    """Everything the index template renders, decided here rather than in the
    page.

    The template loops and formats. What order the rows come in, what a missing
    metric means, how a state is drawn and what each cell of the grid counts are
    all decisions with a right answer, and they belong where they can be tested.
    """
    # Filtered before anything else derives from `entries`, so the grid, the
    # legend, the facet counts and the listing itself all describe the subset
    # a ?q= narrowed to -- the same filter the page's own JS re-applies to the
    # rows this already returned. `unfiltered_total` is kept aside because the
    # strip states a denominator ("18 of 212") rather than letting the reader
    # infer the fleet size from a number that is no longer it.
    unfiltered_total = len(entries)
    entries = [e for e in entries if _matches_query(e.endpoint, q)]
    if domain:
        entries = [
            e for e in entries if (_NAMES.get(e.endpoint) or _NO_NAME).domain == domain
        ]
    if facet:
        entries = [e for e in entries if _matches_facet(e, facet)]
    metrics = _index_metrics(entries)
    # One pass over the store for every row, rather than one per row: the
    # payload is built from a single query and 543 rows asking it 543 times
    # would be the same answer 543 times.
    rows = _index_rows(entries, metrics, explore_endpoints(store))
    history = fleet_history(store)
    # `matching` is exactly the endpoint set `entries` above narrowed to,
    # after all three filters -- q, domain and facet -- so history.rows is
    # filtered by testing membership in it rather than by re-running each
    # predicate a second time. That matters for `facet`: it reads a
    # verdict, which a FleetHistoryRow does not carry, so a predicate
    # re-run here could not ask it the same question `entries` already did.
    # `not facet` guards the case that matters most: with no facet active,
    # membership in `matching` must reduce to exactly the q/domain answer
    # below and never quietly drop a history row `entries` itself does not
    # happen to list (fleet_history and endpoint_index are two separate
    # reads of the store and this task does not audit that they always
    # agree on every endpoint).
    #
    # `history.runs` is NOT filtered -- a sweep is service-level, not
    # per-endpoint, exactly as the RDF representation leaves its activity
    # nodes alone. Without the row filter below, the grid, its
    # "moved"/"read the same way" sentence and its data-fleet-endpoint links
    # named and linked endpoints the filtered rows above had already
    # dropped.
    matching = {e.endpoint for e in entries}
    history = FleetHistory(
        runs=history.runs,
        rows=[
            r
            for r in history.rows
            if _matches_query(r.endpoint, q)
            and _matches_domain(r.endpoint, domain)
            and (not facet or r.endpoint in matching)
        ],
    )
    # The legend counts the chips on this page, and it is built by the same
    # function as the endpoint page's legend from the same table, so the two
    # pages cannot explain the encoding differently. A cell that is a gap
    # rather than a chip is not counted: it is not one of the states.
    drawn = [
        {"slug": cell["slug"]}
        for row in rows
        for cell in row["cells"]
        if cell["present"]
    ]
    # Read once so the strip's freshness figure and the change grid's own
    # sweep count agree about what "last sweep" means: two calls here have
    # produced two FleetStats before and there is no reason a second one
    # would answer differently, only a chance it silently could.
    stats = fleet_stats(store, history, entries)
    return {
        **_nav_context(),
        "here": "index",
        # The overview, above the listing: what has changed, before what is.
        "fleet": _fleet_view(history),
        "stats": stats,
        # UNFILTERED, unlike every neighbour in this dict: it answers "does
        # this store hold anything at all", a fact about the store rather
        # than about the query. It is what guards the page's one-sentence
        # description of itself (index.html:334) and the facet-empty note
        # (index.html:549) -- a `?q=` that matches nothing must not also hide
        # the sentence explaining what this page is, or make a store with 543
        # endpoints look, on its own no-match page, like a store with none.
        # The no-match state gets its own message instead; see `"query"` and
        # `summary.matching` below.
        "endpoint_count": unfiltered_total,
        # The facet pills, above the rows -- domain and metric questions
        # both, one list so the template loops over it once. See
        # _index_pills for what each one counts and why.
        "pills": _index_pills(entries, domain, facet),
        "metrics": metrics,
        "metric_count": len(metrics),
        # The three facet groups, above the rows. Each filters by reading
        # attributes the rows already carry, so none of them costs a byte per
        # row; see the block above _index_row for what each one selects and why.

        # The grid: metrics down, states across, a count in every intersection.
        # It subsumes the two chip strips it replaced, because a row header
        # filters on the metric alone and a column header on the state alone,
        # which is exactly what those strips did.
        # ONE LISTING, alphabetical by endpoint, replacing the three sections
        # headed "availability verified: 57 of 543 endpoints" and so on. The
        # plan owner removed those headings on 2026-08-28 because the grid's
        # availability row states the same three counts and states them better:
        # 57, 482 and 4 rather than the two-way split the headings implied a
        # reader should care about.
        #
        # ALPHABETICAL AND NOT IN THE ENCODING TABLE'S ORDER, which is a
        # decision the removal forces. _index_groups' own comment says the
        # group order "is the encoding table's and not any notion of better or
        # worse", because "ranking the groups would be this service's opinion
        # about the endpoints". That held while the groups were LABELLED and the
        # page said so. Unlabelled, the same order is an unexplained ranking
        # with the sentence that excused it deleted, so the honest flattening is
        # the one that ranks nothing.
        "rows": rows,
        # The dormancy note, once for the page where it was once per group. A
        # group that held no marked row correctly carried none, so three notes
        # could say three different things; one listing says it once.
        "matrix": _metric_state_matrix(rows, metrics),
        "matrix_states": _matrix_states(_metric_state_matrix(rows, metrics)),
        # The column headers' counts. A header carries one for the same reason
        # every other chip does: it is the invariant that caught a chip printing
        # 402 above an empty page. The ROW headers stopped being chips on
        # 2026-08-28 and carry no count, because every cell in the row already
        # filters on that metric and a chip on the name could only widen what
        # they narrow.
        "state_counts": {
            facet["slug"]: facet["count"] for facet in _state_facets(rows)
        },
        # Keyed by slug rather than a list, because the legend it feeds is
        # already looping over the states to draw the swatches and must not loop
        # over a second sequence beside it. The legend shows this beside its own
        # chip count: two numbers answering two questions, which its note
        # explains.
        #
        # KEYING BY SLUG IS NOT WHAT KEEPS THE TWO IN STEP, and a comment here
        # claimed it was until 2026-08-28. A mapping is only total over the
        # legend if it holds a key for every entry the legend lists, and
        # `_legend` lists an eighth when this page drew a verdict this build has
        # no encoding for. Where it did and `_state_facets` returned seven, the
        # template's `state_rows[state.slug]` rendered as nothing at all, which
        # is a chip claiming to select no endpoint that then reveals one. What
        # keeps them in step is that both functions decide on that eighth entry
        # from the same condition over the same rows; see `_state_facets`.
        "state_rows": {
            facet["slug"]: facet["count"] for facet in _state_facets(rows)
        },
        "newest_generated_at": (
            entries[0].newest_generated_at if entries else None
        ),
        "newest_sweep_note": _newest_sweep_note(entries),
        # The one link to /about on this page, in the page header where it costs
        # one occurrence rather than one per row. Nothing on this site linked
        # here until this stage: a row said the newest sweep did not ask and
        # stopped, so the contact address that changes it, and the four numbers
        # behind the mark, were reachable only by pasting the prober's
        # User-Agent URL into a browser.
        "about_path": ABOUT_PATH,
        # The way home, on every page. The logo carries it, so a reader who
        # arrived on one endpoint from a search engine has somewhere to go
        # other than the back button.
        "index_path": INDEX_PATH,
        "docs_path": DOCS_PATH,
        "explore_path": EXPLORE_PATH,
        "row_unfinished_text": ROW_UNFINISHED_TEXT,
        "row_never_reached_text": ROW_NEVER_REACHED_TEXT,
        # The two new markers' words, for the panel that explains them. The
        # dormant marker's reason clause is per row and is built there; what the
        # panel shows is the marker itself, so the words in the key and the words
        # on a row cannot come apart.
        "row_dormant_text": ROW_DORMANT_TEXT,
        "row_measured_by_text": ROW_MEASURED_BY_TEXT,
        # The reasons the panel names, rendered from the map the ROWS are built
        # from rather than written out in the template. See
        # _ROW_DORMANCY_REASONS: the slugs belong to the prober, and a template
        # that spelled them out could go on naming one after a rename.
        # The gloss is formatted here rather than in the template, because the
        # numbers in it belong to DORMANCY and a template holding them would be
        # a second copy of a constant. A gloss with no field formats to itself.
        "dormancy_reasons": [
            {
                "slug": slug,
                "gloss": gloss.format(
                    strikes=DORMANCY["dormant-strikes"],
                    cadence=DORMANCY["dormant-cadence-days"],
                ),
            }
            for slug, (_, gloss) in _ROW_DORMANCY_REASONS.items()
        ],
        # No cadence of its own beside them, and that absence is the fix: the
        # panel used to end one sentence with the cadence for all the reasons at
        # once, which is true of the automatic one and false of the other two.
        # The numbers now travel inside the gloss of the reason they belong to,
        # formatted above from DORMANCY, which is defined with the /about page's
        # constants below and held to prober/src/dormancy.rs's
        # DEFAULT_CADENCE_DAYS by test_about.py. The template states no number
        # of its own either way: it used to say "seven days" in words, so
        # changing the constant self-corrected /about and left the index
        # promising a cadence no sweep uses.
        "empty_cell_text": EMPTY_CELL_TEXT,
        "legend": _legend(drawn),
        "chip_width": verdict_encoding.CHIP_WIDTH_PX,
        "chip_height": verdict_encoding.CHIP_HEIGHT_PX,
        # The fleet in four figures, above the search. New in the 2026-09-15
        # redesign: the page led with rows, which answers "what is here" only
        # after the reader has counted. `answering` and `not_answering` are
        # derived from the FILTERED `entries`, so a filtered page reports the
        # filtered set; `total` stays the unfiltered fleet size so the strip
        # can state a denominator instead of letting the reader guess it from
        # a number that quietly stopped meaning the whole registry.
        "summary": {
            # None when unfiltered, so the template can tell "18 of 212" from
            # "212" without comparing two numbers and guessing. Widened in
            # fix-round-1 to any of the three filters: `?domain=` and
            # `?facet=` can narrow a page exactly as `?q=` can, and a page
            # narrowed by either must state its denominator too.
            "matching": None if not (q or domain or facet) else len(entries),
            "total": unfiltered_total,
            "answering": sum(
                1 for e in entries
                if not e.newest_sweep_declined_to_ask_this_endpoint
            ),
            "not_answering": sum(
                1 for e in entries
                if e.newest_sweep_declined_to_ask_this_endpoint
            ),
            "last_sweep": stats.last_sweep,
        },
        # The submitted ?q=, echoed into the input's value so the client-side
        # enhancement narrows the rows the server already returned instead of
        # re-filtering from an empty box and instantly widening the list.
        "query": q,
        # What the no-match message (below `summary.matching == 0`) names.
        # Built here rather than in the template so the sentence is one
        # decision with a right answer, not three conditionals threaded
        # through Jinja: see _no_match_description.
        "no_match": _no_match_description(q, domain, facet),
    }


def _index_html(
    entries: list[EndpointMeasurements],
    store: Store,
    q: str | None = None,
    domain: str | None = None,
    facet: str | None = None,
) -> str:
    """The index, rendered.

    Takes the store because a row's `content` link depends on whether the
    explorer has vocabulary for that endpoint, which only the store knows. It
    read a static file until 2026-09-05 and needed no store at all.
    """
    return _TEMPLATES.get_template("index.html").render(
        **_index_context(entries, store, q, domain, facet)
    )


_COMPUTED_ON = NamedNode("http://www.w3.org/ns/dqv#computedOn")
_NOT_MEASURED_ON = NamedNode("urn:sparqlwatch:notMeasuredOn")


def _only_matching_endpoints(
    triples: list, known_endpoints: set[str], matching_endpoints: set[str]
) -> list:
    """The constructed index, narrowed to the endpoints a query names.

    Closes over the ENDPOINT SET rather than over a fixed predicate list.
    An earlier version of this filter watched only dqv:computedOn and
    sw:notMeasuredOn, on the measured fact that neither of 439 triples from a
    dormancy-free store had an endpoint IRI as subject. That fact does not
    hold once a store carries a dormancy: a dormant endpoint is the SUBJECT
    of its own sw:dormancyReason / sw:dormantSince, and the activity that
    declined it names an endpoint it DID complete as the OBJECT of
    sw:completedEndpoint (and the one it declined as the object of
    sw:dormantEndpoint). None of those four runs through the two predicates
    the old filter read, so a query that dropped an endpoint everywhere the
    HTML looks could still leave the RDF naming it through one of these.

    So two rules, applied together:

    Rule A drops any triple that mentions a non-matching endpoint directly,
    in EITHER position. This erases the dormancy facts (endpoint as
    subject) and the completedEndpoint/dormantEndpoint links (endpoint as
    object) without touching the activity node's OTHER triples -- its own
    dating, its sw:finalised, its links to endpoints that DO match -- so a
    sweep's provenance survives even when one endpoint it swept does not
    match the query.

    Rule B drops every triple whose subject is a measurement or decline node
    linked, by dqv:computedOn / sw:notMeasuredOn, to a non-matching
    endpoint. Those triples (dqv:value, rdf:type, ...) describe an endpoint
    WITHOUT ever naming it in the triple itself, so Rule A cannot see them.

    Rule B's subject set is derived from ONLY those two predicates,
    deliberately: deriving it from "any triple whose object is a
    non-matching endpoint" would also catch the activity node -- it links to
    a non-matching endpoint through sw:completedEndpoint -- and dropping
    every triple with that subject would take the whole sweep's provenance
    with it.

    `known_endpoints` is the set the HTML lists from (`entries`, i.e.
    endpoint_index(store)), so an arbitrary object IRI -- a vocabulary term,
    an ontology -- is never mistaken for an endpoint: only exact membership
    in that set is ever tested, never a substring match against arbitrary
    graph content.

    `matching_endpoints` is `known_endpoints` already narrowed by whichever of
    `_matches_query`, `_matches_domain` and `_matches_facet` the caller applied
    -- computed there and not here, because `_matches_facet` reads a verdict
    and this function only ever sees an endpoint's URL. Passing the already-
    decided set rather than a needle and re-deciding it here is what keeps
    `?q=`, `?domain=` and `?facet=` a single predicate each representation
    reads once, instead of three predicates spelled twice.
    """
    non_matching = {
        NamedNode(endpoint)
        for endpoint in known_endpoints
        if endpoint not in matching_endpoints
    }
    unwanted_subjects = {
        t.subject
        for t in triples
        if t.predicate in (_COMPUTED_ON, _NOT_MEASURED_ON)
        and t.object in non_matching
    }
    return [
        t
        for t in triples
        if t.subject not in unwanted_subjects
        and t.subject not in non_matching
        and t.object not in non_matching
    ]


def _index_rdf(
    store: Store,
    media_type: str,
    entries: list[EndpointMeasurements],
    q: str | None = None,
    domain: str | None = None,
    facet: str | None = None,
) -> bytes:
    """Serialise every endpoint's facts, straight from the store.

    `q`, `domain` and `facet`, when given, narrow this the same way they
    narrow the HTML: through `_matches_query`, `_matches_domain` and
    `_matches_facet`, tested against `entries` -- the SAME endpoint_index(store)
    call `index_resource` already made for the HTML branch, passed in rather
    than repeated here, so the two branches read one call to the store and
    cannot drift by reading two. See _only_matching_endpoints for why the
    matching SET, rather than a fixed predicate list, is what the filter
    closes over. The query runs unchanged and the filter is applied to what
    it returns; the triples are only materialised into a list when there is
    filtering to do, so an unfiltered request still streams straight into
    serialize() as before.

    queries/__init__.py:5-9 is binding: a .rq file stays runnable as pasted,
    and parameters reach a query through pyoxigraph variable substitution or
    not at all. A substring test is not expressible that way, so filtering
    here is the only route that does not bend that rule -- and it means the
    page and the data share `_matches_query`, `_matches_domain` and
    `_matches_facet` rather than a second spelling of any of them.
    """
    triples = store.query(_INDEX_DESCRIPTION_QUERY)
    if q or domain or facet:
        known_endpoints = {entry.endpoint for entry in entries}
        matching_endpoints = {
            entry.endpoint
            for entry in entries
            if _matches_query(entry.endpoint, q)
            and _matches_domain(entry.endpoint, domain)
            and _matches_facet(entry, facet)
        }
        triples = _only_matching_endpoints(
            list(triples), known_endpoints, matching_endpoints
        )
    return serialize(triples, format=RdfFormat.from_media_type(media_type))


@app.get(INDEX_PATH)
def index_resource(
    request: Request,
    q: str | None = Query(
        None,
        description=(
            "Narrow the index to endpoints whose URL, host, or registry "
            "title contains this text."
        ),
    ),
    domain: str | None = Query(
        None,
        description="Narrow the index to endpoints the registry files as this domain.",
    ),
    facet: str | None = Query(
        None,
        description=(
            "Narrow the index to endpoints answering one fixed question: "
            "answering or void."
        ),
    ),
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

    `q`, `domain` and `facet`, each when given, narrow both representations
    through the same predicate -- `_matches_query`, `_matches_domain` and
    `_matches_facet` respectively; see each for what it matches. The HTML
    branch filters the SELECT bindings before the template ever sees them;
    the RDF branch filters the CONSTRUCT's triples after the fact, in
    `_only_matching_endpoints`, because `index_description.rq` stays an
    unparameterised query and a SPARQL FILTER is not the same operation as a
    Python predicate to apply "the same way". Both branches read ONE call to
    `endpoint_index(store)`, made here and passed to each, so there is only
    one place a filter could ever narrow the two representations to
    different endpoint sets: inside the predicates themselves, which both
    branches already share.
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

    entries = endpoint_index(store)
    if media_type == HTML_MEDIA_TYPE:
        return Response(
            content=_index_html(entries, store, q, domain, facet),
            media_type="text/html; charset=utf-8",
        )
    return Response(
        content=_index_rdf(store, media_type, entries, q, domain, facet),
        media_type=media_type,
    )


# ---------------------------------------------------------------------------
# /docs: the documentation section
# ---------------------------------------------------------------------------
# Three pages, and the third one is not here. `/docs/metrics` describes what
# each metric asks, `/docs/states` what each verdict means and how it is drawn,
# and monitoring stays at `/about`.
#
# WHY MONITORING KEEPS ITS OWN URL rather than moving under /docs. Every request
# this prober makes to a stranger's server carries
# `sparqlwatch/<version> (+https://<host>/about)` in its User-Agent
# (prober/src/client.rs:169), and the spec records that stage 3 "owed an /about"
# at that address. That URL is a promise printed in traffic we have already
# sent, so it is the one URL on this site that is not ours to tidy. The docs
# index names it Monitoring and links to it, which costs a reader nothing and
# costs the promise nothing either.
#
# WHAT IS PINNED AND WHAT IS NOT. `label`, `dimension` and `cost` are
# prober/metrics.toml's own values, and a test reads that file and fails when
# they drift, the same way test_about.py pins the politeness numbers. `explains`
# is documentation: prose nothing can check, written here rather than implied by
# a label. The run graphs carry none of this, because a measurement names its
# metric by IRI and nothing publishes a description for that IRI, which is why
# this table exists at all.
METRIC_DOCS = {
    "availability": {
        "label": "Answers a trivial query",
        "dimension": "availability",
        "cost": "cheap",
        "explains": (
            "Whether a query reaches the endpoint and comes back. The probe is "
            "one SELECT for a single triple, which is the smallest question a "
            "SPARQL endpoint can be asked, so a failure here is about reaching "
            "the service rather than about anything in its data. A timeout is "
            "indeterminate and never absent: what was observed is that no "
            "answer arrived inside the budget, which is not the same as an "
            "endpoint that answered and had nothing."
        ),
    },
    "cors": {
        "label": "Sends access-control-allow-origin on a simple GET",
        "dimension": "interoperability",
        "cost": "cheap",
        "explains": (
            "Whether a script in a browser could read this endpoint's answer. "
            "This is what a curl user sees: the header on a plain GET. It is "
            "deliberately separate from the preflight below, because an "
            "endpoint can have one and not the other and neither implies the "
            "other."
        ),
    },
    "cors-preflight": {
        "label": "Answers a CORS preflight for a cross-origin GET",
        "dimension": "interoperability",
        "cost": "cheap",
        "explains": (
            "Whether a browser would even attempt a real query. Before sending "
            "a cross-origin request that is not simple, a browser asks the "
            "target for permission with an OPTIONS request naming the method "
            "and headers it intends to use, and sends nothing if the answer "
            "does not allow them. That question is the preflight, and it is "
            "what decides whether an in-page query editor can talk to this "
            "endpoint at all. The probe records the header VALUES and not just "
            "their presence, because a header that is there and says no is not "
            "permission."
        ),
    },
    "geo-functions": {
        "label": "GeoSPARQL relation functions",
        "dimension": "capability",
        "cost": "cheap",
        "explains": (
            "Whether the engine evaluates GeoSPARQL relation functions, asked "
            "with a filter over constants so the answer is about the engine "
            "rather than about the data. This is the metric where the "
            "vocabulary earns its keep: an endpoint can evaluate these "
            "functions without declaring them, which is undeclared but "
            "verified, and an endpoint can answer a point-in-polygon test with "
            "the wrong answer, which is declared but wrong. Both are true of "
            "real endpoints in this registry."
        ),
    },
    "geo-data": {
        "label": "Holds WKT geometry",
        "dimension": "content",
        "cost": "cheap",
        "explains": (
            "Whether any geometry is actually stored, asked separately from the "
            "functions above because holding geometry and being able to reason "
            "over it are different facts. The probe guards against a literal "
            "that is present and empty, because one endpoint in the survey this "
            "project reproduces passed a naive check while every geometry it "
            "held was nil."
        ),
    },
    "service-description": {
        "label": "Service description informativeness",
        "dimension": "documentation",
        "cost": "cheap",
        "explains": (
            "What the endpoint says about itself when asked with no query at "
            "all. Graded rather than yes or no, because a description that "
            "exists and names nothing useful is not the same as one that names "
            "its dataset, its graphs and the languages it supports. Most of the "
            "descriptions in this registry are at the lowest level, which is "
            "the engine's default stub rather than anything a publisher wrote."
        ),
    },
    # RETIRED 2026-08-28, and still here because the data is still published.
    # A run graph records what one sweep observed and nothing rewrites one, so
    # 543 has-classes measurements stand in the store, the index derives a
    # column from them, and a column with no description would be a chip a
    # reader cannot look up. `retired` is what keeps that honest: described,
    # and marked as a thing no new sweep will produce.
    "has-classes": {
        "label": "Holds typed resources",
        "dimension": "content",
        "cost": "cheap",
        "retired": "2026-08-28",
        "explains": (
            "Whether anything in the endpoint carried a type at all, asked with "
            "one row as the whole answer. No longer measured, because the "
            "question it answered was not the one it looked like: every RDF "
            "dataset worth monitoring has types, so as a fact about CONTENT "
            "this was close to worthless, and 54 verified against 489 "
            "indeterminate on the 2026-08-24 sweep says what it was really "
            "reporting was whether a query came back. Availability asks that, "
            "with a smaller query. Measurements already taken are still shown "
            "and still true of the sweep that took them."
        ),
    },
    # The three counts. Each states a number the endpoint claims about itself
    # and grades it against one we counted, so `declared-but-wrong` is
    # reachable here and nowhere else on this page.
    "triple-count": {
        "label": "States how many triples it holds",
        "dimension": "content",
        "cost": "expensive",
        "explains": (
            "How many triples the endpoint says it holds, in "
            "void:triples, set against a count of them. A description that is "
            "a few percent out is still 'verified': a VoID file is written "
            "once and the dataset keeps growing, so calling that wrong would "
            "be crying wolf on nearly every real endpoint. An order of "
            "magnitude out reads 'declared but incorrect', which is the most "
            "useful thing this service can tell a consumer about a "
            "description. The count unions the default and named graphs, "
            "because the default-graph-only form answered 0 against a store "
            "holding 12.5 million triples across 45 named graphs, measured "
            "2026-09-05."
        ),
    },
    "graph-count": {
        "label": "States how many graphs it holds",
        "dimension": "content",
        "cost": "expensive",
        "explains": (
            "How many named graphs the endpoint says it has, against a count "
            "of them. No VoID or service-description term states a NUMBER of "
            "graphs, so the claim is the length of the sd:namedGraph list, "
            "which means an endpoint listing none reads as declaring none "
            "rather than as declaring zero."
        ),
    },
    "class-count": {
        "label": "States how many classes it holds",
        "dimension": "content",
        "cost": "expensive",
        "explains": (
            "How many distinct classes the endpoint says it holds, in "
            "void:classes, against a count of them. Distinct from 'describes "
            "its own vocabulary', which asks WHICH classes were named rather "
            "than how many: an endpoint can state the right number and name "
            "none of them."
        ),
    },
    # The content dimension's only verdict, and the one that replaced the two
    # retired below. It sends NOTHING: both halves are already in hand when it
    # is graded, so it costs an operator no request at all.
    #
    # Tiered `exhaustive` all the same, and the tier here is not a claim about
    # this metric's own cost. It grades what `class-profiles` found, so a sweep
    # that declined the profile pass leaves it nothing to grade. Sharing the
    # pass's tier makes the two decline together and publish one honest "not
    # measured" instead of an hourly "indeterminate" about an endpoint nobody
    # asked.
    "vocabulary-described": {
        "label": "Describes its own vocabulary",
        "dimension": "content",
        "cost": "exhaustive",
        "explains": (
            "Whether the endpoint's own description names the classes it "
            "actually holds. The class profile pass finds what is there; a "
            "VoID class partition is how a publisher says what should be "
            "there; this is the two set against each other. 'Confirmed, not "
            "declared' is the common reading and not a fault in the data: an "
            "endpoint can hold hundreds of classes and describe none of them, "
            "which is exactly the gap this service exists to measure. It "
            "reads 'not determined' wherever the profile pass did not run, "
            "which at the default cheap ceiling is everywhere, because there "
            "is nothing to set the description against. It never reports a "
            "description as WRONG: the class enumeration is capped and the "
            "profile pass samples, so a declared class missing from the "
            "profiles may simply never have been asked about, and that is not "
            "evidence enough for the harshest verdict in the vocabulary."
        ),
    },
    # RETIRED 2026-09-04, as a VERDICT, and still here for the reason
    # has-classes is: 597 declines stand in the store across four preserved
    # runs, the index derives a column from them, and a column with no
    # description is a chip a reader cannot look up.
    #
    # Its QUERY was not retired. It is the class enumeration, and it is now the
    # first step of class-profiles below, which is why this entry says the
    # question moved rather than that it was dropped.
    "classes": {
        "label": "Distinct classes",
        "dimension": "content",
        "cost": "expensive",
        "retired": "2026-09-04",
        "explains": (
            "Which types the endpoint holds, sampled rather than counted. No "
            "longer measured as a verdict, and it never produced one: it is "
            "expensive, every sweep so far ran at the cheap ceiling, and all "
            "597 of its mentions across the preserved runs are declines. A "
            "'verified' here would have meant only 'we enumerated some "
            "classes', which is the same near-worthless fact that retired "
            "'holds typed resources'. The question itself was worth asking, so "
            "its query survives as the first step of 'properties per class' "
            "below, which publishes the class list as a sample instead of a "
            "judgement. Declines already recorded are still shown and still "
            "true of the sweep that recorded them."
        ),
    },
    # NO VERDICT AND NO COLUMN, which is what makes this entry different from
    # every other one on this page. The pass publishes a sample of the classes
    # it found and a profile of each, so what it produces is a description of
    # the endpoint's content rather than a judgement about it, and there is no
    # threshold it could be measured against. The index derives its columns
    # from verdicts, so this metric contributes none.
    "class-profiles": {
        "label": "Properties per class",
        "dimension": "content",
        "cost": "exhaustive",
        # NO COLUMN, and the flag is read rather than the comment above it.
        # _index_metrics unions every metric carrying a verdict OR a decline, so
        # before this existed one endpoint's failed pass was enough to draw a
        # column in which the two SUCCESSFUL passes rendered as gaps, whose
        # documented meaning is that the run recorded nothing about the metric.
        # The page stated the opposite of what happened.
        "yields_measurement": False,
        "explains": (
            "For each class the endpoint holds, which properties its instances "
            "carry, how many instances carry each one, and whether the values "
            "are IRIs or one datatype or several. This is the question the "
            "content metrics were reaching for and could not answer: 'holds "
            "typed resources' and 'distinct classes' both reported that a "
            "query came back, while this reports what is in there. The most "
            "expensive thing this service does to a stranger's server, at one "
            "query per class, so it is declined at the default cheap ceiling "
            "and a sweep has to ask for it. It publishes no verdict, because a "
            "profile is a description and not a judgement: there is no "
            "threshold at which 'this class has four properties' is a pass or "
            "a failure."
        ),
    },
}


def _nav_context() -> dict:
    """The header's links, from one list.

    The header carried two links and a conditional until 2026-09-15. It is a
    list here for the reason _docs_context is a table: a page added to the site
    should not mean editing the chrome of every page that already exists.

    Also carries index_path, docs_path and void_path: base.html's footer and
    logo need them, and void_path was set in only two of eight page contexts
    before this function existed, which is a silent href="" everywhere else
    Jinja's default Undefined does not raise. Every context that renders
    base.html merges this one first, so every page has all three.
    """
    return {
        "nav": [
            {"path": INDEX_PATH, "label": "Registry", "slug": "index"},
            {"path": EXPLORE_PATH, "label": "Explore", "slug": "explore"},
            {"path": DOCS_PATH, "label": "Docs", "slug": "docs"},
            {"path": ABOUT_PATH, "label": "About", "slug": "about"},
        ],
        "index_path": INDEX_PATH,
        "docs_path": DOCS_PATH,
        "void_path": VOID_PATH,
    }


def _docs_context() -> dict:
    """What every page in this section needs: the way home, and its siblings.

    One table, so a fourth page is added in one place and every page's nav
    learns about it. Monitoring is a full entry with a `path` like the others,
    which is what lets the index list three pages without knowing that one of
    them lives outside /docs.
    """
    return {
        **_nav_context(),
        # Every docs page is "docs" for aria-current's sake: the section has
        # one entry in the header nav and all four pages sit under it.
        "here": "docs",
        "index_path": INDEX_PATH,
        "docs_path": DOCS_PATH,
        "explore_path": EXPLORE_PATH,
        "about_path": ABOUT_PATH,
        "pages": [
            {
                "path": DOCS_METRICS_PATH,
                "title": "Metrics",
                "blurb": (
                    "The eight things this service asks an endpoint, what each "
                    "question is, and what an answer to it does and does not "
                    "establish."
                ),
            },
            {
                "path": DOCS_STATES_PATH,
                "title": "States",
                "blurb": (
                    "The seven verdicts a measurement can carry, what each one "
                    "means, and how each is drawn without relying on colour."
                ),
            },
            {
                "path": DOCS_VOID_PATH,
                "title": "The derived description",
                "blurb": (
                    "The VoID this service derives for an endpoint that "
                    "publishes none, the terms of our own it carries, and how "
                    "to tell a count that is the endpoint's from one that is a "
                    "sample's."
                ),
            },
            {
                "path": ABOUT_PATH,
                "title": "Monitoring",
                "blurb": (
                    "Who queried your server, how often, how politely, why "
                    "that endpoint, and how to ask to be left alone. This page "
                    "keeps its own address because every request this service "
                    "makes carries it."
                ),
            },
        ],
    }


def _docs_void_context() -> dict:
    """One entry per term the derived description publishes.

    Read from `void_document.TERM_DOCS` rather than restated here, so the page
    cannot describe a vocabulary the document does not emit, nor miss one it
    does. Sorted so the order is stable between renders. void_path itself now
    comes from _docs_context (by way of _nav_context, merged in first at the
    call site) rather than from here: docs_void_resource renders
    **_docs_context(), **_docs_void_context() as two separate keyword
    expansions, and a key both dicts set is a TypeError there, not a silent
    override.
    """
    from void_document import TERM_DOCS

    return {
        "void_terms": [
            {"name": name, "means": TERM_DOCS[name]} for name in sorted(TERM_DOCS)
        ],
    }


def _docs_metrics_context() -> dict:
    """One entry per metric, in the order prober/metrics.toml declares them."""
    return {
        # Measured first, retired last, because a reader scanning this page is
        # looking for what the service does now and a retired entry is a
        # footnote to that rather than one of the eight things it asks.
        "metrics": [
            {"id": metric, **facts}
            for metric, facts in sorted(
                METRIC_DOCS.items(), key=lambda pair: bool(pair[1].get("retired"))
            )
        ],
    }


def _docs_states_context() -> dict:
    """One entry per state, from the encoding table itself.

    Nothing is written twice here: the label, the meaning and all three drawing
    channels come from verdict_encoding, which docs/design/verdict-encoding.md
    is canonical for and a test compares against.
    """
    return {
        "states": [
            {
                "slug": state.slug,
                "label": state.label,
                "meaning": state.meaning,
                "css_class": verdict_encoding.css_class(state.slug),
                "border": state.border,
                "fill": "filled" if state.fill else "empty",
                "weight": state.weight,
            }
            for state in verdict_encoding.STATES
        ],
    }


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
    "sparqlwatch/0.2.0 (+https://sparqlwatch.dev.k8s.semanticscience.org/about)"
)

# The version this build is, for the footer, READ OUT OF THE USER-AGENT rather
# than written down a second time. That string is already pinned to
# prober/Cargo.toml by test_the_user_agent_shown_is_the_one_the_prober_sends,
# so deriving from it means the footer inherits that check for free and a
# version bump cannot leave the page claiming the old one.
#
# The alternative was reading prober/Cargo.toml at runtime, and it does not
# work: the runtime image ships web/ and the prober's data files, not its
# sources. See the Dockerfile, and the 18 tests that skip inside the image for
# the same reason.
VERSION = PROBER_USER_AGENT.removeprefix("sparqlwatch/").split(" ", 1)[0]

# A GLOBAL RATHER THAN A CONTEXT KEY, because the footer is on every page and
# seven render calls would be seven chances to forget one. A page missing its
# version would not fail, it would just quietly say nothing, which is the
# failure mode a global removes entirely.
#
# Registered here and not beside `_TEMPLATES` because PROBER_USER_AGENT, which
# this reads, is declared with the /about page it documents and that is far
# below the environment.
_TEMPLATES.globals["version"] = VERSION

# prober/src/client.rs's MAX_REDIRECT_HOPS: how long a redirect chain the
# prober follows before it gives up. On the page because "follows a redirect
# from it if there is one, and stops" understated what a server's log will
# show, and the bullet about requests per endpoint already concedes the plural
# ("plus one more for each redirect followed"). web/tests/test_about.py reads
# the constant.
MAX_REDIRECT_HOPS = 5

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
    "requests-per-endpoint": 6,
}

# What the admission policy costs an endpoint that has proved expensive and
# silent, as the prober's own defaults. From prober/src/dormancy.rs:
#
#   dormant-cost-seconds      DEFAULT_COST_MS, the default of the sweeper's
#                             --dormant-cost-ms, DIVIDED BY 1000. The constant
#                             is 60_000 because the flag it defaults takes
#                             milliseconds; the number a stranger reading a
#                             sentence about their own server is owed is 60
#                             seconds. web/tests/test_about.py does the division
#                             and fails on a constant that is not whole seconds.
#   dormant-strikes           DEFAULT_STRIKES, the default of --dormant-strikes
#   dormant-cadence-days      DEFAULT_CADENCE_DAYS, the default of
#                             --dormant-every-days
#   dormant-wake-grace-days   DEFAULT_GRACE_DAYS, the default of the OTHER
#                             binary's `dormancy wake --grace-days`. It is not a
#                             flag on the sweeper at all: main.rs fills the field
#                             in from the constant and nothing there reads it,
#                             because its one reader is dormancy::wake. See
#                             thresholds_from's doc comment, which says why a
#                             --dormant-grace-days on the sweeper was removed
#                             rather than left to be parsed and ignored.
#
# A SECOND DICT rather than four more entries in POLITENESS, because these are
# read from a different file, in a different unit, by a different reader:
# test_about.py's prober_defaults() matches Duration::from_secs and NonZeroUsize
# and matches none of these, which are bare u64 and u32. Merging them would put
# a millisecond constant behind a name that says seconds. Both dicts reach the
# page AND the RDF, which is what the union in
# test_the_politeness_numbers_on_the_page_are_the_prober_defaults asserts.
#
# THESE ARE POLITENESS FIGURES, which is why the page renders them under the
# same data-politeness attribute: they are how many requests somebody's server
# gets. An endpoint the admission policy set aside itself is asked at most one
# sweep in every seven days instead of one per sweep, and that is the strongest
# single statement this service makes about the load it puts on a host that
# never answers. All four numbers are that case and only that case: an operator
# hold is asked by no sweep at all until a person lifts it, so no threshold here
# describes it, which is what /about's three paragraphs on the reasons separate.
DORMANCY = {
    "dormant-cost-seconds": 60,
    "dormant-strikes": 2,
    "dormant-cadence-days": 7,
    "dormant-wake-grace-days": 7,
}

# Two endpoints of prober/registry/lod-cloud.toml on ONE machine behind
# different ports, in the order the page names them.
#
# On the page because the politeness figures above are keyed on a host AND a
# port. prober/src/politeness.rs's host_key folds a scheme-default port and
# keeps every other one, deliberately (its own doc comment and the assert_ne! on
# two ports in its unit tests say why), and prober/src/lib.rs groups endpoints
# on that same key. So the gap and the concurrency bound are per host-and-port,
# and the page said "one host" full stop, which is false for exactly the reader
# it is written for: a server operator running two engines on one machine.
#
# Named rather than described, because a promise about somebody's server has to
# be checkable by them. prober/tests/politeness.rs runs the pair through the
# real gate and web/tests/test_about.py checks both are still on the shipped
# list and still share a host and differ in port.
SAME_HOST_DIFFERENT_PORTS = (
    "http://eculture2.cs.vu.nl:8890/sparql",
    "http://eculture2.cs.vu.nl:5020/sparql/",
)

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
_XSD_BOOLEAN = NamedNode("http://www.w3.org/2001/XMLSchema#boolean")


def _about_context() -> dict:
    """Everything web/templates/about.html renders, and nothing derived.

    The template writes the prose; this hands it the numbers and the strings
    that have to agree with something outside the template.

    **_nav_context() is merged first, the way _docs_context merges it, so
    base.html's header nav and footer void_path reach this page too: before
    2026-09-16 this dict set index_path, docs_path and explore_path by hand
    and never set nav or void_path at all, which was invisible only because
    the page still rendered its own two-link header instead of base.html's.
    """
    return {
        **_nav_context(),
        "here": "about",
        "summary": SUMMARY,
        "contact": CONTACT_ADDRESS,
        "user_agent": PROBER_USER_AGENT,
        "exclusion_file": EXCLUSION_FILE,
        "max_redirect_hops": MAX_REDIRECT_HOPS,
        "politeness": POLITENESS,
        "dormancy": DORMANCY,
        "same_host_different_ports": SAME_HOST_DIFFERENT_PORTS,
        "registry": REGISTRY,
        "full_sweep": FULL_SWEEP_DURATION,
        "index_path": INDEX_PATH,
        "docs_path": DOCS_PATH,
        "explore_path": EXPLORE_PATH,
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
    # BOTH dicts, and the dormancy figures are here rather than only in the
    # prose for the reason the section comment above gives: the HTML renders them
    # under data-politeness, and
    # test_the_rdf_and_the_html_state_the_same_numbers_and_the_same_address
    # enforces agreement by iterating exactly those elements. Left out here, the
    # machine-readable representation of this resource would say nothing at all
    # about the admission policy while that test kept passing.
    triples.extend(
        Triple(
            _SERVICE,
            NamedNode(_ABOUT + name),
            Literal(str(value), datatype=_XSD_INTEGER),
        )
        for name, value in (*POLITENESS.items(), *DORMANCY.items())
    )
    return serialize(iter(triples), format=RdfFormat.from_media_type(media_type))


# ---------------------------------------------------------------------------
# The docs routes
# ---------------------------------------------------------------------------
# Negotiated like every other resource here, because the spec's rule is
# "content negotiation on every resource: HTML for people, RDF for machines"
# and these two are not an exception in the way it might first look. A metric
# and a state are VOCABULARY: what this service measures and what its verdicts
# mean are exactly the things a client integrating with it needs without
# parsing English, and the encoding table is already canonical in
# docs/design/verdict-encoding.md. The prose is the part only a person reads.
_DOCS = "urn:sparqlwatch:docs:"
_METRIC = "urn:sparqlwatch:metric:"
_STATE = "urn:sparqlwatch:state:"


def _docs_rdf(media_type: str) -> bytes:
    """The three documents this section holds, and nothing about their prose."""
    section = NamedNode(_DOCS + "section")
    triples = [
        Triple(section, NamedNode(_DOCS + "page"), NamedNode(_DOCS + name))
        for name in ("metrics", "states", "monitoring")
    ]
    return serialize(iter(triples), format=RdfFormat.from_media_type(media_type))


def _docs_metrics_rdf(media_type: str) -> bytes:
    """Each metric with the three facts prober/metrics.toml states about it.

    The label, the dimension and the cost class, which are the prober's own
    values and are pinned against its file by a test. Not the prose: an
    explanation is for a reader and putting it here would invite a consumer to
    treat a paragraph as data.
    """
    triples = []
    for metric, facts in METRIC_DOCS.items():
        subject = NamedNode(_METRIC + metric)
        triples.append(
            Triple(subject, NamedNode(_DOCS + "label"), Literal(facts["label"]))
        )
        triples.append(
            Triple(
                subject,
                NamedNode(_DOCS + "dimension"),
                Literal(facts["dimension"]),
            )
        )
        triples.append(
            Triple(subject, NamedNode(_DOCS + "cost"), Literal(facts["cost"]))
        )
    return serialize(iter(triples), format=RdfFormat.from_media_type(media_type))


def _docs_states_rdf(media_type: str) -> bytes:
    """Each state with its label, its meaning and all three drawing channels.

    The channels are here because they are the encoding, not decoration: a
    client rendering these verdicts itself needs to know that the difference
    between "declared but wrong" and "verified" is a border weight and not a
    colour, which is the property that keeps the drawing legible without colour.
    """
    triples = []
    for state in verdict_encoding.STATES:
        subject = NamedNode(_STATE + state.slug)
        triples.append(
            Triple(subject, NamedNode(_DOCS + "label"), Literal(state.label))
        )
        triples.append(
            Triple(subject, NamedNode(_DOCS + "meaning"), Literal(state.meaning))
        )
        triples.append(
            Triple(subject, NamedNode(_DOCS + "border"), Literal(state.border))
        )
        triples.append(
            Triple(
                subject,
                NamedNode(_DOCS + "fill"),
                Literal("true" if state.fill else "false", datatype=_XSD_BOOLEAN),
            )
        )
        triples.append(
            Triple(
                subject,
                NamedNode(_DOCS + "border-weight-px"),
                Literal(str(state.weight), datatype=_XSD_INTEGER),
            )
        )
    return serialize(iter(triples), format=RdfFormat.from_media_type(media_type))


def _negotiated(request: Request, html, rdf) -> Response:
    """One negotiation for the three docs resources.

    The other three routes each spell this out, and each had a reason to: they
    differ in what they do when the store cannot answer. These three read no
    store and cannot 404, so one helper is the honest shape rather than three
    copies of an identical branch.
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
        return Response(content=html(), media_type="text/html; charset=utf-8")
    return Response(content=rdf(media_type), media_type=media_type)


@app.get(DOCS_PATH)
def docs_resource(request: Request) -> Response:
    """What this service measures, what its verdicts mean, and why it queried."""
    return _negotiated(
        request,
        lambda: _TEMPLATES.get_template("docs.html").render(**_docs_context()),
        _docs_rdf,
    )


@app.get(DOCS_METRICS_PATH)
def docs_metrics_resource(request: Request) -> Response:
    """One section per metric: what it asks and what a verdict on it means."""
    return _negotiated(
        request,
        lambda: _TEMPLATES.get_template("docs-metrics.html").render(
            **_docs_context(), **_docs_metrics_context()
        ),
        _docs_metrics_rdf,
    )


@app.get(DOCS_STATES_PATH)
def docs_states_resource(request: Request) -> Response:
    """One section per state: what it means and how it is drawn."""
    return _negotiated(
        request,
        lambda: _TEMPLATES.get_template("docs-states.html").render(
            **_docs_context(), **_docs_states_context()
        ),
        _docs_states_rdf,
    )


@app.get(DOCS_VOID_PATH)
def docs_void_resource(request: Request) -> Response:
    """What the derived description is, and what its own terms mean."""
    return Response(
        content=_TEMPLATES.get_template("docs-void.html").render(
            **_docs_context(), **_docs_void_context()
        ),
        media_type="text/html; charset=utf-8",
    )


@app.get(EXPLORE_PATH)
def explore(request: Request, store: Store = Depends(get_store)) -> Response:
    """The vocabulary explorer, computed from the store.

    Served a static payload until 2026-09-05, captured from a probe of two
    endpoints and unable to describe any others. That file existed because the
    store held no content samples: `classes` was declined at the default cost
    ceiling on every sweep. The content-profile work publishes both halves of
    the declared/observed axis now, so this reads them.

    HTML only. The other resources negotiate, and this one does not, because
    there is no RDF here that the store could be asked for: the payload is a
    derived view of one run's facts, and publishing it as RDF would assert it as
    a fact about the endpoints rather than as a reading of one probe.
    """
    # ONE BUILD, read twice. The note counts what the payload holds, so
    # deriving both from the same dict is also the only way they cannot
    # disagree about how many endpoints this page is showing.
    payload = build_payload(store)
    return Response(
        content=_TEMPLATES.get_template("explore.html").render(
            # **_nav_context() first, the way every other page context merges
            # it, so base.html's header nav and footer void_path reach this
            # page: before 2026-09-16 this call set index_path, docs_path and
            # explore_path by hand and never set nav or void_path at all,
            # invisible only because the page rendered its own two-link
            # header instead of base.html's.
            **_nav_context(),
            here="explore",
            payload=json.dumps(payload, sort_keys=False),
            probe_note=_explore_probe_note(payload),
            explore_path=EXPLORE_PATH,
        ),
        media_type="text/html; charset=utf-8",
    )


# The icon, and the mask Safari's pinned tab wants instead.
#
# Read once at import and served from memory. They are under a kilobyte each,
# and a per-request file read for something every page requests is work the
# store's own queries should have.
#
# Served from routes rather than a StaticFiles mount because these two files are
# the whole of this site's static content: a mount would publish the directory
# they sit in, and the directory they sit in is the source tree.
_ICON = (Path(__file__).resolve().parent / "icon.svg").read_bytes()
_ICON_MONO = (Path(__file__).resolve().parent / "icon-mono.svg").read_bytes()
_SVG = "image/svg+xml"
# A year, and immutable: the bytes never change under a given URL, and a pinned
# tab that refetched its icon on every page load would be asking for a file it
# already has, forever.
_ICON_CACHE = "public, max-age=31536000, immutable"


@app.get(ICON_PATH)
def icon() -> Response:
    """The tab icon."""
    return Response(
        content=_ICON, media_type=_SVG, headers={"Cache-Control": _ICON_CACHE}
    )


@app.get(ICON_MASK_PATH)
def icon_mask() -> Response:
    """The monochrome mask Safari paints for a pinned tab."""
    return Response(
        content=_ICON_MONO, media_type=_SVG, headers={"Cache-Control": _ICON_CACHE}
    )


# The registry files, read once. The Dockerfile copies prober/registry/ into
# the image, so these are present at runtime; in a checkout they are the same
# files seed-registry writes.
#
# A missing directory yields an empty map rather than an error: the site serves
# before anything is seeded, and every row falls back to its URL.
_REGISTRY_DIR = Path(__file__).resolve().parents[1] / "prober" / "registry"
_NAMES: dict[str, Name] = load_names(sorted(_REGISTRY_DIR.glob("*.toml")))


# The one stylesheet. Assembled at import from the static file plus the verdict
# rules verdict_encoding generates, so a change to either moves the URL.
#
# Served from a route rather than a StaticFiles mount for the reason the icons
# are (app.py:3311): a mount would publish the directory they sit in, and the
# directory they sit in is the source tree.
_STYLESHEET = (
    (Path(__file__).resolve().parent / "static" / "site.css").read_text()
    + "\n\n/* Generated from web/verdict_encoding.py. */\n"
    + verdict_encoding.css_rules()
    + "\n"
).encode()
# Content-addressed, because the header below promises a year. The bytes never
# change under this URL; a new build gets a new URL and browsers refetch nothing.
_STYLESHEET_HASH = hashlib.sha256(_STYLESHEET).hexdigest()[:12]
STYLESHEET_PATH = f"/static/site.{_STYLESHEET_HASH}.css"
_STYLESHEET_CACHE = "public, max-age=31536000, immutable"


@app.get("/static/site.{digest}.css")
def stylesheet(digest: str) -> Response:
    """The site's stylesheet, at a URL that changes when its bytes do.

    Any other digest 404s rather than redirecting to the current one. A stale
    URL that answered with current bytes would be a lie about immutability, and
    the one thing a year-long cache header may not do is lie.
    """
    if digest != _STYLESHEET_HASH:
        return Response(status_code=404)
    return Response(
        content=_STYLESHEET,
        media_type="text/css",
        headers={"Cache-Control": _STYLESHEET_CACHE},
    )


_TEMPLATES.globals["stylesheet_path"] = STYLESHEET_PATH


# The vocabulary search script, served the same way the stylesheet is: read at
# import, hashed, published at a content-addressed URL that a year-long cache
# header can promise never changes. web/vocab_match.py is the specification
# this is a transliteration of; see that module's header comment.
_SCRIPT = (Path(__file__).resolve().parent / "static" / "vocab-search.js").read_bytes()
_SCRIPT_HASH = hashlib.sha256(_SCRIPT).hexdigest()[:12]
SCRIPT_PATH = f"/static/vocab-search.{_SCRIPT_HASH}.js"
_SCRIPT_CACHE = "public, max-age=31536000, immutable"


@app.get("/static/vocab-search.{digest}.js")
def vocab_search_script(digest: str) -> Response:
    """The vocabulary search script, at a URL that changes when its bytes do.

    Any other digest 404s rather than redirecting to the current one, for the
    same reason the stylesheet route does (app.py:3587): the one thing a
    year-long cache header may not do is lie about the bytes at a URL.
    """
    if digest != _SCRIPT_HASH:
        return Response(status_code=404)
    return Response(
        content=_SCRIPT,
        media_type="application/javascript",
        headers={"Cache-Control": _SCRIPT_CACHE},
    )


_TEMPLATES.globals["script_path"] = SCRIPT_PATH


@app.get("/favicon.ico")
def favicon_ico() -> Response:
    """The path browsers ask for without being told to.

    A 404 here is harmless and also noise in every log, and some contexts (a
    bookmark, a reader that does not parse the head) only ever try this one. It
    answers with the SVG, which every browser this decade renders.
    """
    return Response(
        content=_ICON, media_type=_SVG, headers={"Cache-Control": _ICON_CACHE}
    )


@app.get(VOID_PATH)
def void_resource(
    request: Request,
    url: str | None = None,
    store: Store = Depends(get_store),
) -> Response:
    """A VoID description of one endpoint, derived from what we observed.

    RDF ONLY, and deliberately no HTML. This resource exists for a tool: a
    query editor that needs class and property partitions to autocomplete
    against, or a consumer choosing an endpoint to build on. A person who wants
    to read what is in an endpoint has the endpoint page and the explorer, both
    of which say it in words. Offering HTML here would mean maintaining a third
    rendering of the same facts.

    404 WHEN WE HAVE NOT PROFILED IT, rather than an empty description. A
    document that says only "this is a description of X" asserts that X was
    profiled and found to hold nothing, which is a claim about the endpoint
    rather than about us. The 404 says the true thing: there is no such
    description here.
    """
    if not url:
        return Response(
            content="this resource describes one endpoint; name it with ?url=\n",
            status_code=400,
            media_type="text/plain; charset=utf-8",
        )
    media_type = choose_representation(request.headers.get("accept"))
    if media_type is None or media_type == HTML_MEDIA_TYPE:
        # An Accept of text/html reaches here as HTML_MEDIA_TYPE and is refused
        # with the rest: 406 naming what is on offer, rather than a redirect to
        # a page that answers a different question.
        return Response(
            content=(
                "this resource is RDF only; it offers "
                + ", ".join(RDF_MEDIA_TYPES)
                + "\n"
            ),
            status_code=406,
            media_type="text/plain; charset=utf-8",
        )
    document = str(request.url)
    triples = void_triples(store, url, document)
    if not triples:
        return Response(
            content=(
                "no content profile for that endpoint in this store, so there "
                "is nothing to describe\n"
            ),
            status_code=404,
            media_type="text/plain; charset=utf-8",
        )
    return Response(
        content=serialize(triples, format=RdfFormat.from_media_type(media_type)),
        media_type=media_type,
    )


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
