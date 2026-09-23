"""One endpoint resource, two representations, chosen by Accept.

The point of this file is not that FastAPI can return a string. It is that
the HTML and the RDF for the same URL say the same thing, and that the
server never guesses which one the client wanted. The two representations
come from two genuinely different derivations: the HTML from the SELECT
bindings of endpoint_measurements.rq and endpoint_content.rq, the RDF from
the CONSTRUCT of endpoint_description.rq. That is what makes
test_the_two_representations_agree worth writing, and it is why the RDF body
is parsed here rather than merely checked for length: a body that is not
valid RDF is worse than a 406, because a machine will act on it.

Every value asserted here is a real value of the committed fixtures under
web/tests/fixtures/; see each fixture's header comment for its provenance.
"""

import gc
from html.parser import HTMLParser
from pathlib import Path

import pytest
from pyoxigraph import Literal, NamedNode, RdfFormat, Store, Variable, parse
from starlette.testclient import TestClient

from app import STORE_PATH_VARIABLE, ENDPOINT_PATH, INDEX_PATH, app, get_store
from load_run import load_run, rebuild_current
from queries import read_query

KADASTER = "https://data.kkg.kadaster.nl/query"
TRUNCATED = "https://truncated.example/sparql"
UNKNOWN = "https://nobody-has-ever-probed-this.example/sparql"
PROPERTIES_ONLY = "https://properties-only.example/sparql"

DQV = "http://www.w3.org/ns/dqv#"
PROV = "http://www.w3.org/ns/prov#"
SW = "urn:sparqlwatch:"
M = SW + "metric:"

ACTIVITY = SW + "activity:"
SAMPLING_SWEEP = "2026-08-22T16:00:00Z"
DECLINING_SWEEP = "2026-08-22T18:00:00Z"

# Every metric run-two-sweeps.nq measures on kadaster in its current (16:00)
# run, with the verdict that run recorded. All eight, because "both
# renderings represent every verdict identically" is the claim being made.
CURRENT_VERDICTS = {
    M + "availability": "verified",
    M + "classes": "verified",
    M + "cors": "verified",
    M + "cors-preflight": "verified",
    M + "geo-data": "verified",
    M + "geo-functions": "undeclared-but-verified",
    M + "has-classes": "verified",
    M + "service-description": "verified",
}

# The url values that cannot be an absolute IRI, so cannot name an endpoint.
# An empty one is what a submitted-but-empty form field sends and a trailing
# space is what a copy-paste sends, so neither is exotic.
MALFORMED_URLS = (
    "not a url",
    "",
    "http://a b/",
    "<script>",
    "https://data.kkg.kadaster.nl/query ",
)

CURRENT_CLASSES_VERDICT = "verified"
STALE_CLASSES_VERDICT = "indeterminate"
STALE_RUN_STAMP = "2026-08-22T14:00:00Z"


@pytest.fixture
def client_for():
    """Builds a test client whose route reads the store handed to it.

    The store arrives through FastAPI's dependency mechanism rather than
    being opened when app.py is imported, so each test gets the fixture
    store it asked for and no test can see another's data.
    """
    clients = []

    def build(store):
        app.dependency_overrides[get_store] = lambda: store
        client = TestClient(app)
        clients.append(client)
        return client

    yield build
    app.dependency_overrides.clear()
    for client in clients:
        client.close()


def get(client, endpoint, accept="*/*"):
    """GET the endpoint resource. ``accept=None`` sends no Accept header at
    all, which httpx will not do by default (it sends ``*/*``, exactly as
    curl 8.7.1 does), so the header is removed from the built request."""
    request = client.build_request(
        "GET", ENDPOINT_PATH, params={"url": endpoint}
    )
    if accept is None:
        del request.headers["accept"]
    else:
        request.headers["accept"] = accept
    return client.send(request)


def graph_of(response):
    """Parse a response body as RDF and return it as a queryable store.

    Parsing is the assertion: pyoxigraph.parse raises SyntaxError on a body
    that is not the media type the response claimed.
    """
    store = Store()
    store.extend(
        quad
        for quad in parse(
            response.content,
            format=RdfFormat.from_media_type(
                response.headers["content-type"].split(";")[0].strip()
            ),
        )
    )
    return store


def has_triple(store, subject, predicate, obj):
    return any(store.quads_for_pattern(subject, predicate, obj))


def subjects_with(store, predicate, obj):
    return {
        quad.subject
        for quad in store.quads_for_pattern(None, predicate, obj)
    }


class _Attributes(HTMLParser):
    """Collects every element's attributes, so the tests can read the page's
    data- attributes without depending on tag names, nesting or order."""

    def __init__(self):
        super().__init__()
        self.elements = []

    def handle_starttag(self, tag, attrs):
        self.elements.append(dict(attrs))


def html_verdicts(response):
    """The metric -> verdict mapping the HTML page states.

    ``data-metric`` and ``data-verdict`` are the contract between the page
    and this test, and Task 3's template must keep them: without a
    machine-readable hook there is no way to compare what a person is shown
    against what a machine is served, and that comparison is the whole point
    of test_the_two_representations_agree.
    """
    parser = _Attributes()
    parser.feed(response.text)
    return {
        element["data-metric"]: element["data-verdict"]
        for element in parser.elements
        if "data-metric" in element and "data-verdict" in element
    }


def html_declined(response):
    parser = _Attributes()
    parser.feed(response.text)
    return {
        element["data-metric"]: element["data-declined"]
        for element in parser.elements
        if "data-metric" in element and "data-declined" in element
    }


def objects_of(store, subject, predicate):
    return {
        quad.object
        for quad in store.quads_for_pattern(subject, predicate, None)
    }


def rdf_verdicts(graph):
    """The metric -> verdict mapping the RDF document states.

    Read out of the served document rather than out of the store, so this is
    what a machine consuming this resource would conclude.
    """
    verdicts = {}
    for quad in graph.quads_for_pattern(
        None, NamedNode(DQV + "isMeasurementOf"), None
    ):
        values = objects_of(graph, quad.subject, NamedNode(DQV + "value"))
        assert len(values) == 1, f"{quad.subject} carries {len(values)} verdicts"
        verdicts[quad.object.value] = values.pop().value
    return verdicts


def test_accept_html_serves_html(client_for, store):
    response = get(client_for(store), KADASTER, accept="text/html")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")
    assert KADASTER in response.text


def test_a_star_accept_serves_html(client_for, store):
    """``*/*`` means "anything", so which representation to serve is a
    choice, not a deduction. It is HTML because a person exploring with curl
    (which sends exactly this header: curl 8.7.1 on this machine) is better
    served by something legible than by Turtle."""
    response = get(client_for(store), KADASTER, accept="*/*")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")


def test_a_missing_accept_serves_html(client_for, store):
    response = get(client_for(store), KADASTER, accept=None)
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")


def test_accept_turtle_serves_turtle_that_parses_as_rdf(client_for, store):
    response = get(client_for(store), KADASTER, accept="text/turtle")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/turtle")
    graph = graph_of(response)
    assert len(graph) > 0, "parsed, but empty: nothing was actually served"


def test_accept_n_triples_serves_n_triples_that_parses_as_rdf(client_for, store):
    response = get(client_for(store), KADASTER, accept="application/n-triples")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("application/n-triples")
    assert len(graph_of(response)) > 0


def test_the_rdf_carries_this_endpoints_measurement_verbatim(client_for, store):
    """A specific triple, not a count. run-with-samples.nq measures
    kadaster's sw:metric:classes as "verified" in the 16:00 run."""
    graph = graph_of(get(client_for(store), KADASTER, accept="text/turtle"))

    measured_classes = subjects_with(
        graph, NamedNode(DQV + "isMeasurementOf"), NamedNode(M + "classes")
    )
    assert len(measured_classes) == 1, "one measurement of classes, one endpoint"
    measurement = measured_classes.pop()

    assert has_triple(
        graph,
        measurement,
        NamedNode(DQV + "computedOn"),
        NamedNode(KADASTER),
    )
    assert has_triple(
        graph,
        measurement,
        NamedNode(DQV + "value"),
        Literal(CURRENT_CLASSES_VERDICT),
    )


def test_the_rdf_carries_another_endpoints_nothing(client_for, store):
    """Three endpoints share the fixture's single run graph. A CONSTRUCT
    that ignored its ?endpoint substitution would serve all three."""
    graph = graph_of(get(client_for(store), KADASTER, accept="text/turtle"))
    computed_on = {
        quad.object.value
        for quad in graph.quads_for_pattern(
            None, NamedNode(DQV + "computedOn"), None
        )
    }
    assert computed_on == {KADASTER}


def test_the_rdf_carries_the_declined_fact(client_for, store_declined):
    """run-declined.nq declines sw:metric:classes for kadaster with reason
    "cost-ceiling". A decline is a different fact from a verdict, and the
    RDF must carry it as one rather than dropping it."""
    graph = graph_of(get(client_for(store_declined), KADASTER, accept="text/turtle"))

    declines = subjects_with(
        graph,
        NamedNode(SW + "notMeasuredMetric"),
        NamedNode(M + "classes"),
    )
    assert len(declines) == 1
    decline = declines.pop()

    assert has_triple(
        graph, decline, NamedNode(SW + "notMeasuredOn"), NamedNode(KADASTER)
    )
    assert has_triple(
        graph,
        decline,
        NamedNode(SW + "notMeasuredReason"),
        Literal("cost-ceiling"),
    )
    assert not has_triple(
        graph,
        decline,
        NamedNode(DQV + "value"),
        None,
    ), "a decline is not a measurement and must carry no dqv:value"


def test_an_unservable_accept_is_a_406(client_for, store):
    """Refusing is better than guessing: a client that asked only for JSON
    and got HTML has been handed something it cannot read while being told
    it succeeded."""
    response = get(client_for(store), KADASTER, accept="application/json")
    assert response.status_code == 406


def test_a_q_value_list_prefers_the_higher_q(client_for, store):
    """A negotiator that ignores q values while claiming to negotiate is a
    confident wrong answer about what the client asked for."""
    client = client_for(store)

    turtle_wins = get(client, KADASTER, accept="text/html;q=0.9, text/turtle")
    assert turtle_wins.status_code == 200
    assert turtle_wins.headers["content-type"].startswith("text/turtle")

    html_wins = get(client, KADASTER, accept="text/html, text/turtle;q=0.5")
    assert html_wins.status_code == 200
    assert html_wins.headers["content-type"].startswith("text/html")


def test_q_zero_rejects_a_representation(client_for, store):
    """q=0 means "not acceptable", so html;q=0 must not be served even
    though a bare */* would be."""
    client = client_for(store)

    response = get(client, KADASTER, accept="text/html;q=0, text/turtle")
    assert response.headers["content-type"].startswith("text/turtle")

    only_html_refused = get(client, KADASTER, accept="text/html;q=0")
    assert only_html_refused.status_code == 406


def test_a_specific_refusal_beats_a_wildcard_offer(client_for, store):
    """`*/*;q=1, text/html;q=0` means "anything except HTML".

    Reading it as "anything" requires taking the highest matching q rather
    than the q of the most specific matching range, which turns an explicit
    refusal into an offer. This test exists because the mutation that does
    exactly that passed every other test in this file.
    """
    response = get(client_for(store), KADASTER, accept="*/*;q=1, text/html;q=0")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/turtle")


def test_a_wildcard_subtype_is_matched(client_for, store):
    """text/* covers both representations; the server's own preference
    settles it, the same way */* does."""
    response = get(client_for(store), KADASTER, accept="text/*")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")


def test_a_malformed_q_value_is_ignored_rather_than_clamped(client_for, store):
    """Two equally malformed q values must not get opposite outcomes.

    RFC 9110's qvalue grammar is ( "0" [ "." 0*3DIGIT ] ) / ( "1" [ "."
    0*3("0") ] ), so none of the values below is a q. float() accepts some of
    them, though, and clamping what it accepted into the 0..1 range made
    "q=-1" mean "q=0": an explicit refusal read out of a malformed value,
    while "q=abc" and "q=2" defaulted to 1.0 and were served. A value that is
    not a q expresses no preference, so the parameter is ignored and the
    range keeps the q=1.0 it would have had with no parameter at all.
    """
    client = client_for(store)
    for value in ("abc", "", "nan", "inf", "2", "-1", "1.5", "0.1234", "+1"):
        response = get(client, KADASTER, accept=f"text/html;q={value}")
        assert response.status_code == 200, f"q={value!r}"
        assert response.headers["content-type"].startswith("text/html")

    # The grammar's own values still mean what they say, at both ends and
    # with every permitted number of decimals.
    for value in ("0", "0.0", "0.000"):
        assert get(client, KADASTER, accept=f"text/html;q={value}").status_code == 406
    for value in ("1", "1.0", "1.000", "0.5", "0.333"):
        served = get(client, KADASTER, accept=f"text/html;q={value}")
        assert served.status_code == 200, f"q={value!r}"
    ranked = get(client, KADASTER, accept="text/html;q=0.5, text/turtle;q=0.6")
    assert ranked.headers["content-type"].startswith("text/turtle")


def test_two_equally_specific_ranges_agree_whichever_order(client_for, store):
    """One header must not mean two things depending on the order it is in.

    RFC 9110 does not define which of two equally specific ranges wins, and
    breaking the tie on "first one seen" made "text/html;q=1, text/html;q=0"
    a page and "text/html;q=0, text/html;q=1" a 406. The lower q wins, so an
    explicit refusal anywhere in the header is honoured, which is the same
    reading as the most-specific-wins rule.
    """
    client = client_for(store)
    for accept in (
        "text/html;q=1, text/html;q=0",
        "text/html;q=0, text/html;q=1",
    ):
        assert get(client, KADASTER, accept=accept).status_code == 406, accept

    # The refusal is of HTML, not of the resource: a representation the
    # header does not refuse is still served.
    both = get(
        client, KADASTER, accept="text/html;q=0, text/html;q=1, text/turtle"
    )
    assert both.status_code == 200
    assert both.headers["content-type"].startswith("text/turtle")


def test_a_header_with_no_readable_range_falls_back_to_html(client_for, store):
    """No range at all is no preference, which is not the same as a refusal.

    "Accept: *" is invalid per RFC 9110 and some clients send it anyway;
    "garbage" and ",,," are the same shape. None of them contains a media
    range, so none of them says what the client wants, and a header that says
    nothing is what a missing header is. A 406 there refuses a request that
    asked for nothing in particular.

    A header whose ranges DO parse and which we cannot serve stays a 406.
    That distinction is the whole point: it is a client saying what it wants.
    """
    client = client_for(store)
    for accept in ("*", "garbage", ",,,", ";q=1"):
        response = get(client, KADASTER, accept=accept)
        assert response.status_code == 200, accept
        assert response.headers["content-type"].startswith("text/html")

    for accept in ("application/json", "application/json, application/xml"):
        assert get(client, KADASTER, accept=accept).status_code == 406, accept


def test_an_unknown_endpoint_is_404_as_html(client_for, store):
    response = get(client_for(store), UNKNOWN, accept="text/html")
    assert response.status_code == 404


def test_an_unknown_endpoint_is_404_as_rdf(client_for, store):
    """404 in both representations. An empty 200 would tell a machine that
    this service has looked at the endpoint and found nothing to say, which
    is a different claim from never having heard of it."""
    response = get(client_for(store), UNKNOWN, accept="text/turtle")
    assert response.status_code == 404


def test_an_endpoint_known_only_by_a_content_sample_is_served(
    client_for, store_truncated
):
    """run-truncated.nq holds a class sample for this endpoint and no
    measurement at all. "Known" therefore cannot mean "measured": defining
    it that way 404s a resource this service demonstrably has facts about,
    and Task 3's truncation test needs this page."""
    response = get(client_for(store_truncated), TRUNCATED, accept="text/turtle")
    assert response.status_code == 200
    graph = graph_of(response)
    assert has_triple(
        graph,
        None,
        NamedNode(SW + "sampleTruncated"),
        Literal("true", datatype=NamedNode("http://www.w3.org/2001/XMLSchema#boolean")),
    )


def test_the_two_representations_agree(client_for, store_two_sweeps):
    """The defect this file exists to prevent.

    run-two-sweeps.nq measures eight metrics on kadaster, and its
    sw:metric:classes reads "verified" in the current (16:00) run and
    "indeterminate" in the stale (14:00) one. The HTML path picks its run
    through endpoint_measurements.rq's SELECT and the RDF path through
    endpoint_description.rq's CONSTRUCT, so the two can drift: this asserts
    that all eight agree, and that they agree on the current run's values
    rather than merely on each other.
    """
    client = client_for(store_two_sweeps)

    shown = html_verdicts(get(client, KADASTER, accept="text/html"))
    graph = graph_of(get(client, KADASTER, accept="text/turtle"))
    served = rdf_verdicts(graph)

    # Every metric the current run measured, in both renderings, with the
    # value that run recorded. One metric would leave seven unchecked, and
    # the drift this guards against does not have to touch every metric.
    assert shown == CURRENT_VERDICTS
    assert served == CURRENT_VERDICTS
    assert served == shown
    assert shown[M + "classes"] == CURRENT_CLASSES_VERDICT
    assert STALE_CLASSES_VERDICT not in served.values()
    assert STALE_RUN_STAMP not in response_text_of(graph), (
        "the stale run's measurement IRIs must not appear at all"
    )


def response_text_of(graph):
    """Every IRI and literal in a parsed graph, as one string, so a test can
    assert that a whole run's worth of subjects is absent."""
    parts = []
    for quad in graph:
        for term in (quad.subject, quad.predicate, quad.object):
            parts.append(term.value)
    return "\n".join(parts)


def test_the_html_reports_a_decline_as_a_decline(client_for, store_declined):
    """Task 3 owns how a decline looks. What this pins is that the HTML path
    does not render one as a verdict, which would state a value for a metric
    nobody measured."""
    response = get(client_for(store_declined), KADASTER, accept="text/html")
    assert response.status_code == 200
    assert html_declined(response)[M + "classes"] == "cost-ceiling"
    assert M + "classes" not in html_verdicts(response)


def test_the_rdf_dates_the_sample_by_the_sweep_that_took_it(
    client_for, store_stale_sample
):
    """A consumer must be able to tell which sweep saw what.

    In this store the newest run that measured kadaster is the 18:00 sweep,
    which declined sw:metric:classes, and the newest run that sampled it is
    the 16:00 one. The document therefore carries two activities, and every
    fact in it has to hang off the right one: a document holding one activity
    and an unlinked sample invites the consumer to date the sample to the
    sweep that declined to look.
    """
    graph = graph_of(
        get(client_for(store_stale_sample), KADASTER, accept="text/turtle")
    )

    samples = subjects_with(
        graph, NamedNode(SW + "sampledBy"), NamedNode(M + "classes")
    )
    assert len(samples) == 1
    sample = samples.pop()
    assert objects_of(graph, sample, NamedNode(PROV + "wasGeneratedBy")) == {
        NamedNode(ACTIVITY + SAMPLING_SWEEP)
    }
    assert objects_of(
        graph,
        NamedNode(ACTIVITY + SAMPLING_SWEEP),
        NamedNode(PROV + "generatedAtTime"),
    ) == {
        Literal(
            SAMPLING_SWEEP,
            datatype=NamedNode("http://www.w3.org/2001/XMLSchema#dateTime"),
        )
    }

    # A MEASUREMENT, NOT A DECLINE, since 2026-09-23. The 18:00 sweep declined
    # classes, and declining no longer erases the 16:00 sweep's reading of it,
    # so what the document carries for this metric is 16:00's verdict hanging
    # off 16:00's activity. That is the same pairing this test was written to
    # protect -- every fact on the activity that observed it -- arriving at the
    # verdict now that the verdict survives.
    assert (
        subjects_with(
            graph, NamedNode(SW + "notMeasuredMetric"), NamedNode(M + "classes")
        )
        == set()
    ), "a measurement stands for classes, so no decline is published for it"

    measurements = subjects_with(
        graph, NamedNode(DQV + "isMeasurementOf"), NamedNode(M + "classes")
    )
    assert len(measurements) == 1
    assert objects_of(
        graph, measurements.pop(), NamedNode(PROV + "wasGeneratedBy")
    ) == {NamedNode(ACTIVITY + SAMPLING_SWEEP)}, (
        "the classes verdict must hang off the sweep that took it, not off the "
        "sweep that declined to"
    )

    stamps = {
        quad.object.value
        for quad in graph.quads_for_pattern(
            None, NamedNode(PROV + "generatedAtTime"), None
        )
    }
    assert stamps == {SAMPLING_SWEEP, DECLINING_SWEEP}


def test_a_sample_with_no_provenance_is_still_served(
    client_for, store_truncated
):
    """run-truncated.nq's sample carries no prov:wasGeneratedBy, which the
    real prober always writes but this synthetic fixture does not. The
    provenance the document can carry is OPTIONAL for exactly that reason: a
    sample the HTML shows must never be one the RDF omits."""
    graph = graph_of(
        get(client_for(store_truncated), TRUNCATED, accept="text/turtle")
    )
    samples = subjects_with(
        graph, NamedNode(SW + "sampledFrom"), NamedNode(TRUNCATED)
    )
    assert len(samples) == 1
    assert objects_of(
        graph, samples.pop(), NamedNode(PROV + "wasGeneratedBy")
    ) == set()


def test_a_malformed_url_is_a_400(client_for, store):
    """A string that cannot be an IRI is a malformed request, not an
    identifier that missed.

    A 500 tells the client the service is broken when the request was, and
    these are the two ordinary ways a person sends one: an empty form field
    and a copy-paste with a trailing space. A well-formed IRI this store has
    never heard of stays a 404, which is a statement about the store.
    """
    client = client_for(store)
    for url in MALFORMED_URLS:
        for accept in ("text/html", "text/turtle"):
            response = get(client, url, accept=accept)
            assert response.status_code == 400, f"{url!r} as {accept}"
            assert "not a valid absolute IRI" in response.text

    for url in ("urn:x", "javascript:alert(1)"):
        assert get(client, url, accept="text/html").status_code == 404, url


def test_a_missing_url_is_a_400_like_a_malformed_one(client_for, store):
    """The one response on this resource that ignored Accept.

    FastAPI answers a missing required query parameter with a 422 and a JSON
    body, so a person following the README's curl example without the
    parameter got JSON however they negotiated. A missing url and a malformed
    one are the same class of client error, so they get the same status and
    the same kind of body, and negotiation still decides first: a client that
    can read none of our representations gets the 406 either way.
    """
    client = client_for(store)
    for accept in ("text/html", "text/turtle", None):
        request = client.build_request("GET", ENDPOINT_PATH)
        if accept is None:
            del request.headers["accept"]
        else:
            request.headers["accept"] = accept
        response = client.send(request)
        assert response.status_code == 400, accept
        assert response.headers["content-type"].startswith("text/plain")
        assert "url query parameter" in response.text

    unservable = client.build_request("GET", ENDPOINT_PATH)
    unservable.headers["accept"] = "application/json"
    assert client.send(unservable).status_code == 406


def test_a_malformed_url_is_not_repaired(client_for, store):
    """The trailing space is not trimmed away.

    The endpoint IRI round-trips byte for byte by design (see the URL shape
    comment in web/app.py), so normalising the value would make the page
    describe a different endpoint from the one asked for.
    """
    response = get(client_for(store), KADASTER + " ", accept="text/html")
    assert response.status_code == 400
    assert "classes sampled" not in response.text


def test_an_endpoint_sampled_only_by_another_metric_is_404_that_says_what_was_checked(
    client_for, store_properties_sample
):
    """The 404 body must not claim ignorance the store contradicts.

    run-properties-sample.nq holds six quads about this endpoint, including a
    dcat:DataService declaration and a sample of its properties. What is
    missing is a measurement, a decline, and a class sample, which is what
    the route actually looks for, so that is what the body says.
    """
    for accept in ("text/html", "text/turtle"):
        response = get(client_for(store_properties_sample), PROPERTIES_ONLY, accept=accept)
        assert response.status_code == 404
        body = response.text.lower()
        assert "no measurement, no decline and no class sample" in body
        assert "recorded anything about" not in body
        assert PROPERTIES_ONLY in response.text


# ---------------------------------------------------------------------------
# The store the server opens
# ---------------------------------------------------------------------------
# These are the only tests here that read SPARQLWATCH_STORE. Every other test
# in this file replaces get_store through app.dependency_overrides and never
# touches a path on disk.
FIXTURE = Path(__file__).parent / "fixtures" / "run-with-samples.nq"
DECLINED_FIXTURE = Path(__file__).parent / "fixtures" / "run-declined.nq"
NEW_SUBJECTS_FIXTURE = Path(__file__).parent / "fixtures" / "run-new-subjects.nq"

# The three runs the two tests below build one store out of, oldest first, and
# the instant of the one they drop. Three and not two, so that dropping the
# newest leaves TWO run graphs behind and a rebuild has to choose between
# them: the 18:00 sweep declined sw:metric:classes on its cost ceiling, so its
# seven verdicts and one decline are a different answer from the 16:00 sweep's
# eight verdicts, and a rebuild that reached for the oldest run rather than the
# newest surviving one would be visible.
DROPPED_SWEEP = "2026-08-22T20:00:00Z"


def test_a_store_path_with_nothing_at_it_is_refused(tmp_path, monkeypatch):
    """A typo in SPARQLWATCH_STORE must not become an empty store.

    Store() creates the RocksDB directory when it is missing, so a mistyped
    path used to produce a server that answered every request with "no
    measurement, no decline and no class sample in this store mentions ...".
    That is true of the empty store it had just created, and it is
    indistinguishable from a registry nobody has swept, so the operator's
    mistake was reported as a fact about the endpoints.
    """
    missing = tmp_path / "typo"
    monkeypatch.setenv(STORE_PATH_VARIABLE, str(missing))
    with pytest.raises(RuntimeError) as raised:
        get_store()
    assert STORE_PATH_VARIABLE in str(raised.value)
    assert str(missing) in str(raised.value)
    assert not missing.exists(), "the mistake must not create a store"

    empty = tmp_path / "empty"
    empty.mkdir()
    monkeypatch.setenv(STORE_PATH_VARIABLE, str(empty))
    with pytest.raises(RuntimeError):
        get_store()


def test_a_directory_of_run_files_is_not_mistaken_for_a_store(tmp_path, monkeypatch):
    """The mistake a directory-entries check cannot see: SPARQLWATCH_STORE
    pointed at the directory holding the run files, rather than at the
    store. The documented load_run.py invocation takes the store path and
    the run path in a row, which makes the two easy to swap.

    The directory is not empty, so a check that only asked "is anything in
    here" would pass it, and Store() would then create a fresh RocksDB
    database inside the operator's data directory: exactly the empty store
    this check exists to refuse.
    """
    run_dir = tmp_path / "runs"
    run_dir.mkdir()
    (run_dir / "run-with-samples.nq").write_bytes(FIXTURE.read_bytes())
    monkeypatch.setenv(STORE_PATH_VARIABLE, str(run_dir))
    with pytest.raises(RuntimeError) as raised:
        get_store()
    assert STORE_PATH_VARIABLE in str(raised.value)


def test_an_empty_database_is_still_refused(tmp_path, monkeypatch):
    """The empty-store refusal, kept as its own test.

    A store directory that exists and opens as zero quads is either a store
    nothing has been loaded into yet or a path that is not the store at all,
    and answering "no measurement, no decline and no class sample in this store
    mentions ..." out of it reports the operator's mistake as a fact about the
    endpoints. The refusal has a paragraph of reasoning in _opened_store and
    the first draft of stage 3-2's plan dropped it while adding the current
    graph check beside it, so it gets a test of its own that names the
    condition.
    """
    path = tmp_path / "empty.db"
    Store(str(path))
    gc.collect()
    monkeypatch.setenv(STORE_PATH_VARIABLE, str(path))
    with pytest.raises(RuntimeError, match="holding no quads"):
        get_store()


def test_a_store_with_run_graphs_and_no_current_is_refused_at_open(
    tmp_path, monkeypatch
):
    """The migration guard, and the same reasoning as the two refusals above.

    Every store built before stage 3-2 holds run graphs and no
    urn:sparqlwatch:current graph, and that includes the only real store this
    project has. The three read queries read current, so such a store answers
    "we know nothing about this endpoint" for every endpoint it fully
    describes. That is the failure _opened_store already exists to prevent,
    reached by a third route, so it is refused the same way and the message
    names the command that fixes it.
    """
    path = tmp_path / "pre-current.db"
    built = Store(str(path))
    load_run(built, FIXTURE.read_bytes())
    built.remove_graph(NamedNode("urn:sparqlwatch:current"))
    assert len(built) > 0, "the run graph is still there"
    del built
    gc.collect()

    monkeypatch.setenv(STORE_PATH_VARIABLE, str(path))
    with pytest.raises(RuntimeError) as raised:
        get_store()
    message = str(raised.value)
    assert "urn:sparqlwatch:current" in message, "name the graph that is missing"
    assert "--rebuild" in message, "and name the command that builds it"
    assert str(path) in message


def test_a_store_whose_pointer_names_a_dropped_run_is_refused_at_open(
    tmp_path, monkeypatch
):
    """Dropping a bad run graph, which the design advertises, one step along.

    One graph per run exists so a bad run can be removed wholesale, and
    remove_graph is used in this suite today. Since stage 3-2 decided recency
    once, in urn:sparqlwatch:current, the endpoints of a dropped run keep a
    sw:currentRun naming a graph that is gone, and both read queries drop those
    solutions on FILTER (BOUND(?generatedAt)). The index then says "no run in
    this store has recorded a measurement or a decline for any endpoint" and
    the endpoint page says "no run in this store has measured this endpoint",
    out of a store whose older run graph still holds eight verdicts for each of
    them.

    Answering "nothing measured" out of a store that holds the measurements is
    the failure the two refusals above exist to prevent, so this is refused the
    same way, at the same place, and the message names the rebuild.

    The single derivation is deliberate and stays: two derivations that can
    disagree is what moving recency into current removed. So the store fails
    loudly rather than falling back.
    """
    path = tmp_path / "dropped-run.db"
    built = Store(str(path))
    for fixture in (FIXTURE, DECLINED_FIXTURE, NEW_SUBJECTS_FIXTURE):
        load_run(built, fixture.read_bytes())
    built.remove_graph(NamedNode(SW + "run:" + DROPPED_SWEEP))
    assert len(built) > 0, "the two older run graphs are still there"
    del built
    gc.collect()

    monkeypatch.setenv(STORE_PATH_VARIABLE, str(path))
    with pytest.raises(RuntimeError) as raised:
        get_store()
    message = str(raised.value)
    assert SW + "run:" + DROPPED_SWEEP in message, "name the graph that is gone"
    assert KADASTER in message, "name an endpoint it leaves lying"
    assert "3" in message, "and say how many endpoints are affected"
    assert "--rebuild" in message, "and name the command that repairs it"
    assert str(path) in message


def test_a_rebuild_returns_a_store_whose_run_was_dropped_to_service(
    tmp_path, monkeypatch
):
    """The other half: the repair works, and it brings back the older run.

    Without this the refusal above would be a dead end, and the operator who
    dropped a bad run would have a store nothing can serve. current is derived
    from the run graphs alone, so a rebuild after a drop points every endpoint
    at the newest run that still measures it, which here is the 16:00 sweep.
    """
    path = tmp_path / "rebuilt.db"
    built = Store(str(path))
    for fixture in (FIXTURE, DECLINED_FIXTURE, NEW_SUBJECTS_FIXTURE):
        load_run(built, fixture.read_bytes())
    built.remove_graph(NamedNode(SW + "run:" + DROPPED_SWEEP))
    rebuild_current(built)
    del built
    gc.collect()

    monkeypatch.setenv(STORE_PATH_VARIABLE, str(path))
    client = TestClient(app)
    try:
        response = get(client, KADASTER, accept="text/html")
        assert response.status_code == 200, response.text
        # Read back off the page's own attributes rather than out of the store,
        # so this is a claim about what a reader is served. The 18:00 sweep is
        # the newest that survives the drop and it is the run the page names.
        assert DECLINING_SWEEP in response.text, (
            "the newest surviving run is the one shown"
        )
        # EIGHT VERDICTS AND NO DECLINE since 2026-09-23, where it was seven and
        # one. The 18:00 sweep declined sw:metric:classes, and a decline no
        # longer erases the 16:00 sweep's reading of it, so the page carries
        # that reading -- dated to 16:00, beside seven rows dated 18:00. The old
        # numbers recorded the loss this change exists to end.
        assert len(html_verdicts(response)) == 8, html_verdicts(response)
        assert list(html_declined(response)) == [], html_declined(response)
    finally:
        client.close()


def test_a_store_path_holding_a_store_is_opened(tmp_path, monkeypatch):
    """The other half of the check: a real store still opens.

    Without this, the refusal above would pass just as well if it refused
    everything, and the server would never start at all.
    """
    path = tmp_path / "sparqlwatch.db"
    built = Store(str(path))
    load_run(built, FIXTURE.read_bytes())
    # An on-disk Oxigraph store cannot be opened twice at once, so this test
    # has to let go of its own handle before asking the app to open the path.
    del built
    gc.collect()

    monkeypatch.setenv(STORE_PATH_VARIABLE, str(path))
    assert len(get_store()) > 0


# ---------------------------------------------------------------------------
# The unfinished-run facts, in both representations
# ---------------------------------------------------------------------------
QLEVER = "https://qlever.dev/api/osm-planet"
CRASHED_SWEEP = "2026-08-23T04:00:00Z"
FINISHED_SWEEP = "2026-08-23T02:00:00Z"

EMISSION = NamedNode(SW + "emission")
FINALISED = NamedNode(SW + "finalised")
COMPLETED = NamedNode(SW + "completedEndpoint")
GENERATED_AT = NamedNode(PROV + "generatedAtTime")

TRUE = Literal("true", datatype=NamedNode("http://www.w3.org/2001/XMLSchema#boolean"))


def activity_at(stamp):
    return NamedNode(ACTIVITY + stamp)


def test_the_rdf_carries_the_inputs_the_unfinished_sentence_is_derived_from(
    client_for, store_crashed_partway
):
    """The HTML says the sweep it is showing did not finish. The RDF must let
    a machine conclude the same thing, from facts rather than from a flag.

    So the CONSTRUCT emits the inputs: this run's sw:emission, its
    sw:completedEndpoint for this endpoint, and no sw:finalised, because the
    run has none. A derived "unfinished" quad would be the read tier
    asserting something no run graph holds, and endpoint_description.rq's
    header is explicit that every triple it emits appears verbatim in the
    store.
    """
    client = client_for(store_crashed_partway)
    shown = texts_with(get(client, KADASTER, accept="text/html"), "data-run-unfinished")
    assert len(shown) == 1, "the HTML must be making the claim being compared"
    assert CRASHED_SWEEP in shown[0]

    graph = graph_of(get(client, KADASTER, accept="text/turtle"))
    activity = activity_at(CRASHED_SWEEP)
    assert has_triple(graph, activity, EMISSION, Literal("incremental"))
    assert has_triple(graph, activity, COMPLETED, NamedNode(KADASTER))
    assert not has_triple(graph, activity, FINALISED, None), (
        "this run recorded no sw:finalised, so the document must not either"
    )
    assert not has_triple(graph, None, NamedNode(SW + "unfinished"), None), (
        "the derivation's result is not a fact any run graph holds"
    )


def test_a_finished_runs_rdf_carries_finalised(client_for, store_prober_failed):
    """The converse, and without it the test above proves nothing.

    An absent quad is unreadable on its own: a consumer that sees no
    sw:finalised cannot tell "this run did not finish" from "this
    representation does not carry that predicate at all". So a run that DID
    finish has to serve the quad, and this is the test that says the
    predicate is served when the store holds it.
    """
    graph = graph_of(get(client_for(store_prober_failed), KADASTER, accept="text/turtle"))
    activity = activity_at(FINISHED_SWEEP)
    assert has_triple(graph, activity, EMISSION, Literal("incremental"))
    assert has_triple(graph, activity, FINALISED, TRUE)
    assert has_triple(graph, activity, COMPLETED, NamedNode(KADASTER))


def test_the_rdf_carries_the_newest_run_an_endpoint_was_never_reached_by(
    client_for, store_crashed_partway
):
    """Condition (b)'s inputs, which are the ones a CONSTRUCT cannot state as
    a conclusion.

    Two of the three are absences: the newest run has no sw:finalised and no
    sw:completedEndpoint naming qlever. What the document can carry is the
    newest activity itself, its prov:generatedAtTime and its sw:emission, and
    that is enough: an activity later than the one the measurements hang off,
    saying it was written incrementally, with neither terminator that would
    account for this endpoint. A consumer draws the same conclusion the page
    draws, from the same facts.
    """
    client = client_for(store_crashed_partway)
    shown = texts_with(
        get(client, QLEVER, accept="text/html"), "data-newer-run-unfinished"
    )
    assert len(shown) == 1, "the HTML must be making the claim being compared"
    # The HTML names the readings' instant, not the crashed sweep's: the
    # sentence was cut back on 2026-09-18 and the crashed sweep's own timestamp
    # went with it. The RDF below still carries that activity and its
    # prov:generatedAtTime, which is where a consumer gets it and is exactly
    # the asymmetry this file exists to check.
    assert SAMPLING_SWEEP in shown[0]

    graph = graph_of(get(client, QLEVER, accept="text/turtle"))
    newest = activity_at(CRASHED_SWEEP)
    shown_activity = activity_at(SAMPLING_SWEEP)

    assert has_triple(graph, newest, NamedNode(PROV + "type"), None) is False
    assert has_triple(
        graph,
        newest,
        GENERATED_AT,
        Literal(
            CRASHED_SWEEP,
            datatype=NamedNode("http://www.w3.org/2001/XMLSchema#dateTime"),
        ),
    ), "the newest activity must be dated, or nothing says it is the newer one"
    assert has_triple(graph, newest, EMISSION, Literal("incremental"))
    assert not has_triple(graph, newest, FINALISED, None)
    assert not has_triple(graph, newest, COMPLETED, None), (
        "this run never reached qlever, and it may not carry another "
        "endpoint's completion marker into qlever's document"
    )

    # And the run the facts DO come from is a pre-1c-b4 sweep, so it carries
    # none of the three. The two activities have to be distinguishable, or a
    # consumer cannot tell which one the measurements belong to.
    assert has_triple(graph, shown_activity, EMISSION, None) is False
    assert has_triple(graph, shown_activity, FINALISED, None) is False
    assert has_triple(graph, shown_activity, COMPLETED, None) is False
    measured_by = {
        quad.object
        for quad in graph.quads_for_pattern(
            None, NamedNode(PROV + "wasGeneratedBy"), None
        )
    }
    assert measured_by == {shown_activity}


def texts_with(response, attribute):
    """The text of each element carrying ``attribute``, as the reader sees it.

    A second reader of the page in this file, deliberately narrow: the tests
    above compare one sentence a person is shown against the quads a machine
    is served, so they need the sentence and not the whole document. Asserting
    the sentence is somewhere in the body would pass while it sat in a
    comment.
    """
    parser = _SentenceTexts(attribute)
    parser.feed(response.text)
    return parser.found


class _SentenceTexts(HTMLParser):
    def __init__(self, attribute):
        super().__init__()
        self.attribute = attribute
        self.found = []
        self._open = False
        self._buffer = []

    def handle_starttag(self, tag, attrs):
        if self.attribute in dict(attrs):
            self._open = True
            self._buffer = []

    def handle_endtag(self, tag):
        if self._open:
            self.found.append(" ".join("".join(self._buffer).split()))
            self._open = False

    def handle_data(self, data):
        if self._open:
            self._buffer.append(data)


# ---------------------------------------------------------------------------
# The dormancy facts, in both machine-readable representations
# ---------------------------------------------------------------------------
DORMANT_ENDPOINT = NamedNode(SW + "dormantEndpoint")
DORMANCY_REASON = NamedNode(SW + "dormancyReason")
DORMANT_SINCE = NamedNode(SW + "dormantSince")
DORMANT_SWEEP = "2026-08-27T10:00:00Z"
DORMANT_SINCE_INSTANT = "2026-08-27T09:30:00Z"
DATE_TIME = NamedNode("http://www.w3.org/2001/XMLSchema#dateTime")


def test_the_turtle_for_a_dormant_endpoint_carries_a_dateable_dormancy(
    client_for, store_dormant_newest
):
    """The HTML says the newest sweep did not ask this endpoint and why. The
    RDF has to let a machine reach the same claim, and date it.

    Three facts, all of them verbatim from the newest run's graph: the
    activity's sw:dormantEndpoint naming this endpoint, the endpoint's
    sw:dormancyReason, and its sw:dormantSince. Dateable is the load-bearing
    word: the declaration hangs off an activity that carries its own
    prov:generatedAtTime, so the sweep that declined is identifiable and is
    distinguishable from the older activity the measurements hang off. Without
    that a consumer would date the dormancy to the only activity it could find.

    endpoint_description.rq's header makes the widening a rule: the two
    representations move together or the HTML draws a conclusion the RDF
    cannot.
    """
    client = client_for(store_dormant_newest)
    shown = texts_with(
        get(client, KADASTER, accept="text/html"), "data-newest-sweep-silent"
    )
    assert len(shown) == 1, "the HTML must be making the claim being compared"
    # Same cut as the crash sentence above: the HTML dates the readings and
    # names the reason, and leaves the declining sweep's instant to the RDF.
    assert SAMPLING_SWEEP in shown[0]
    assert "operator-hold" in shown[0]

    graph = graph_of(get(client, KADASTER, accept="text/turtle"))
    declining = activity_at(DORMANT_SWEEP)
    endpoint = NamedNode(KADASTER)

    assert has_triple(graph, declining, DORMANT_ENDPOINT, endpoint)
    assert has_triple(graph, endpoint, DORMANCY_REASON, Literal("operator-hold"))
    assert has_triple(
        graph,
        endpoint,
        DORMANT_SINCE,
        Literal(DORMANT_SINCE_INSTANT, datatype=DATE_TIME),
    )
    assert has_triple(
        graph,
        declining,
        GENERATED_AT,
        Literal(DORMANT_SWEEP, datatype=DATE_TIME),
    ), "the declining activity must be dated, or the dormancy is not dateable"
    assert has_triple(
        graph,
        activity_at(SAMPLING_SWEEP),
        GENERATED_AT,
        Literal(SAMPLING_SWEEP, datatype=DATE_TIME),
    ), "and the activity the verdicts come from must stay distinguishable"
    assert not has_triple(graph, None, NamedNode(SW + "dormant"), None), (
        "the derivation's result is not a fact any run graph holds"
    )


def test_the_index_turtle_carries_it_too(client_for, store_dormant_newest):
    """The same three facts in the index's representation.

    The index and the endpoint page are two views of one set of facts, so a
    dormancy the endpoint's own document carries and the index's does not would
    be a fact that exists only if you know which URL to ask. index.rq selects
    it for the HTML index in this same commit, and the two must widen together
    for the reason index_description.rq's header gives.

    The declaration names ONE endpoint here, not every endpoint the document
    describes: the other two of the trio were measured by that sweep, and a
    dormancy triple about them would be false.
    """
    client = client_for(store_dormant_newest)
    # `facet=all`: the index defaults to `available` since 2026-09-18, and the
    # endpoint this test is about is the dormant one. Both representations
    # honour that default together -- test_index.py holds them to it -- so this
    # asks the listing that holds every endpoint, which is what the document
    # this test is about describes.
    turtle = client.get(
        INDEX_PATH, params={"facet": "all"}, headers={"accept": "text/turtle"}
    )
    assert turtle.status_code == 200
    graph = graph_of(turtle)

    declining = activity_at(DORMANT_SWEEP)
    assert {
        quad.object.value
        for quad in graph.quads_for_pattern(declining, DORMANT_ENDPOINT, None)
    } == {KADASTER}
    assert has_triple(
        graph, NamedNode(KADASTER), DORMANCY_REASON, Literal("operator-hold")
    )
    assert has_triple(
        graph,
        NamedNode(KADASTER),
        DORMANT_SINCE,
        Literal(DORMANT_SINCE_INSTANT, datatype=DATE_TIME),
    )
    assert has_triple(
        graph, declining, GENERATED_AT, Literal(DORMANT_SWEEP, datatype=DATE_TIME)
    )


def test_neither_description_query_describes_an_endpoint_the_html_omits(
    client_for, store_dormancy_alone
):
    """A dormancy-only endpoint stays a 404, and the RDF stays in lockstep.

    An endpoint the newest run only declared dormant has no sw:currentRun, so
    both read queries return zero rows for it and app.py's knownness test 404s
    it. That test is deliberately NOT widened here: the comment above it
    (app.py, at the `not measurements.assessed and not content.sampled` line)
    argues this position already, and widening it would mean widening
    endpoint_content.rq's metric pin, which belongs to spec stage 2b.

    So the dormancy branch in both description queries is keyed on the endpoint
    having that pointer. Without the key the index document would carry a
    dormancy for a resource the index does not list and the endpoint document
    would describe one the server will not serve, against
    endpoint_description.rq's own rule that the two representations widen
    together.
    """
    client = client_for(store_dormancy_alone)
    assert get(client, KADASTER, accept="text/html").status_code == 404
    assert get(client, KADASTER, accept="text/turtle").status_code == 404

    index = graph_of(client.get(INDEX_PATH, headers={"accept": "text/turtle"}))
    assert not has_triple(index, None, DORMANT_ENDPOINT, NamedNode(KADASTER)), (
        "the index does not list this endpoint, so its document may not "
        "describe it"
    )
    assert not has_triple(index, NamedNode(KADASTER), DORMANCY_REASON, None)
    assert not has_triple(index, NamedNode(KADASTER), DORMANT_SINCE, None)
    # And the endpoints that sweep DID measure are still described, so this is
    # not passing because the document is empty.
    assert has_triple(
        index, None, NamedNode(DQV + "computedOn"), NamedNode(QLEVER)
    )

    # The endpoint's OWN description query, asked directly, because the route
    # 404s before it is reached and the guard there would otherwise be
    # untested: a CONSTRUCT that emitted a dormancy for this endpoint would be
    # the document for a resource this server refuses to serve.
    described = list(
        store_dormancy_alone.query(
            read_query("endpoint_description"),
            substitutions={Variable("endpoint"): NamedNode(KADASTER)},
        )
    )
    predicates = {triple.predicate for triple in described}
    assert DORMANT_ENDPOINT not in predicates
    assert DORMANCY_REASON not in predicates
    assert DORMANT_SINCE not in predicates
    # What the arm does still emit for an endpoint it knows nothing about is
    # the newest activity's own run-level facts, which it emitted before this
    # commit too: that arm is deliberately not scoped to ?endpoint, and the
    # route 404s before the document is ever built. The dormancy branch is the
    # part that had to be keyed, because a dormancy names an endpoint and those
    # facts name only the run.
    assert {triple.subject for triple in described} == {
        activity_at(DORMANT_SWEEP)
    }


def rows_of(text):
    """Every endpoint URL shown as a row on the index page.

    Read off ``data-endpoint`` via this file's own `_Attributes` parser, so
    this does not depend on tag names, nesting or order. A local copy of
    test_index.py's `endpoints_shown`, kept local because the test files in
    this repo each carry their own fixtures and helpers rather than
    importing across suites.
    """
    parser = _Attributes()
    parser.feed(text)
    return [
        element["data-endpoint"]
        for element in parser.elements
        if "data-endpoint" in element
    ]


def test_a_filtered_index_describes_exactly_the_endpoints_it_lists(
    client_for, store_registry_sample
):
    """The two representations of a filtered index must name the same set.

    This is the same guarantee as
    test_neither_description_query_describes_an_endpoint_the_html_omits, now
    under a query parameter. A filter applied to one representation and not the
    other is the most ordinary way for them to drift.
    """
    client = client_for(store_registry_sample)
    listed = set(rows_of(client.get("/?q=uniprot", headers={"accept": "text/html"}).text))
    graph = graph_of(
        client.get("/?q=uniprot", headers={"accept": "text/turtle"})
    )
    # [R3] Endpoints are the OBJECTS of dqv:computedOn / sw:notMeasuredOn.
    # None is ever a subject in THIS fixture -- measured: 0 of 439 constructed
    # triples have an endpoint IRI as subject. That is a fact of
    # store_registry_sample, which carries no dormancy; it is not general
    # (see test_a_filtered_index_names_no_non_matching_endpoint_anywhere
    # below, which uses a fixture where it fails). This assertion stays
    # because it is still true and useful for this fixture, not because it
    # is the whole invariant.
    described = {
        str(t.object.value)
        for t in graph
        if str(t.predicate.value)
        in (
            "http://www.w3.org/ns/dqv#computedOn",
            "urn:sparqlwatch:notMeasuredOn",
        )
    }
    assert listed, "the fixture must yield a non-empty subset"
    assert described == listed, (
        f"the page lists {sorted(listed)} and the RDF describes {sorted(described)}"
    )


ONTOP = "https://ontop.certain.ai.ustp.at/sparql"


def test_a_filtered_index_names_no_non_matching_endpoint_anywhere(
    client_for, store_dormant_newest
):
    """The closed form of the invariant above: not just the two predicates
    the HTML draws its rows from, but every position in the graph.

    store_dormant_newest is the fixture that breaks a filter keyed only on
    dqv:computedOn / sw:notMeasuredOn: it measured ontop and qlever, and its
    newest sweep declared kadaster dormant. That makes kadaster the SUBJECT
    of its own sw:dormancyReason / sw:dormantSince, and makes qlever the
    OBJECT of the declining activity's sw:completedEndpoint. A filter that
    only reads computedOn/notMeasuredOn objects cannot see either channel,
    so narrowing to "ontop" must still make both kadaster and qlever vanish
    from EVERY position, or the RDF still names an endpoint the HTML
    dropped.
    """
    client = client_for(store_dormant_newest)
    # `facet=all` on BOTH, and the same string on both, which is the point:
    # since 2026-09-18 the bare `/` carries a default facet, so a test that
    # asked the HTML for one URL and the Turtle for another would be comparing
    # two pages. What is being pinned is that one URL gives one endpoint set in
    # either representation, so the URL has to be identical.
    url = "/?q=ontop&facet=all"
    listed = set(rows_of(client.get(url, headers={"accept": "text/html"}).text))
    assert listed == {ONTOP}, "the fixture must narrow to exactly one endpoint"

    graph = graph_of(client.get(url, headers={"accept": "text/turtle"}))
    non_matching = {KADASTER, "https://qlever.dev/api/osm-planet"}
    mentioned = {
        str(node.value)
        for quad in graph
        for node in (quad.subject, quad.object)
        if isinstance(node, NamedNode) and str(node.value) in non_matching
    }
    assert not mentioned, (
        f"the filtered RDF still names {mentioned} in some position, "
        "although the HTML dropped it"
    )
    # And the sweep's own provenance survives losing those links: an empty
    # activity node would be describing less than the store knows, not
    # narrowing to a query. The declining activity's own dating fact -- not
    # one of the links just erased -- is what proves this.
    declining = activity_at(DORMANT_SWEEP)
    assert has_triple(
        graph, declining, GENERATED_AT, Literal(DORMANT_SWEEP, datatype=DATE_TIME)
    ), "the activity's own facts must survive filtering the endpoints it links to"


def test_a_query_matching_a_title_narrows_both_representations(
    client_for, store_registry_sample
):
    """The invariant, under the case this feature introduces.

    Before names, ?q= could only match a URL, and both representations
    filtered on the same string. A title lives in the registry files and not
    in the run graph, so the RDF branch cannot see it unless the predicate
    they share does -- and if only the HTML saw it, the two would describe
    different endpoint sets under the same URL.

    Reuses this file's own `rows_of` and `graph_of` rather than defining
    "endpoints_shown"/"parse_graph" of the same shape a second time: `rows_of`
    already reads every rendered row's `data-endpoint`, and `graph_of` already
    parses a response body as RDF.

    _NAMES holds ~552 registry entries, almost all of endpoints this
    fixture's store never heard of; picking the first titled entry in _NAMES
    without checking it is one of the nine this fixture actually stores would
    pick an endpoint no query -- correct or broken -- could ever surface here,
    making `listed` and `described` both trivially empty. So the candidate is
    restricted to a title carried by an endpoint this fixture stores.

    Equality of the two representations' sets is necessary but not
    sufficient: both branches share one `_matches_query`, so they cannot
    disagree with each other even with title matching entirely disabled --
    that failure mode is structurally already ruled out. What can fail is
    title matching not happening at all, which set equality alone does not
    catch (two empty sets are still equal). The membership assertion below is
    what catches that.
    """
    import app as app_module

    client = client_for(store_registry_sample)
    known = set(rows_of(client.get("/", headers={"accept": "text/html"}).text))
    titled = [u for u, n in app_module._NAMES.items() if n.title and u in known]
    if not titled:
        pytest.skip("no endpoint in this fixture carries a registry title")
    url = titled[0]
    name = app_module._NAMES[url]
    needle = name.title.split()[0].lower()
    assert needle not in url.lower() and needle not in name.host.lower(), (
        "the chosen needle must be unreachable except through the title, or "
        "this test cannot tell a title match from a URL match"
    )

    listed = set(
        rows_of(client.get(f"/?q={needle}", headers={"accept": "text/html"}).text)
    )
    graph = graph_of(
        client.get(f"/?q={needle}", headers={"accept": "text/turtle"})
    )
    described = {
        str(t.object.value)
        for t in graph
        if str(t.predicate.value)
        in (
            "http://www.w3.org/ns/dqv#computedOn",
            "urn:sparqlwatch:notMeasuredOn",
        )
    }
    assert listed, "the fixture must yield a title-only match, or this test proves nothing"
    assert url in listed
    assert described == listed


def test_a_domain_narrows_both_representations(client_for, store_registry_sample):
    """The invariant again, under `?domain=`, task 6's own new filter.

    A domain lives in the registry files, the same place a title does, and
    not in the run graph -- so this is exactly the hazard
    test_a_query_matching_a_title_narrows_both_representations guards
    against, one query parameter over. `?domain=` reaches the RDF branch
    through `_matches_domain`, and both branches now call that one function
    rather than each spelling its own comparison (fix-round-2 closed the gap
    where the HTML branch re-inlined it) -- but that structural guarantee
    alone would pass even if `_matches_domain` matched nothing at all: two
    empty sets are still equal. `sparql.uniprot.org` is one of this
    fixture's two endpoints of domain "life_sciences" (the other is
    www.foodie-cloud.org; registry_names.load_names merging each of title,
    domain and datasets independently, fix-round-1, is why both carry it).
    "government" -- the domain named in the original brief -- now matches 2
    of this fixture's nine too, once that same fix stopped a bare, title-less
    listing in one registry file from silently erasing a domain a richer
    entry gave the same URL in another; life_sciences is kept here because
    it was never affected by that bug, not because government still fails.
    The membership assertion below is what actually catches a `?domain=`
    that reached only one representation.
    """
    client = client_for(store_registry_sample)
    listed = set(
        rows_of(client.get("/?domain=life_sciences", headers={"accept": "text/html"}).text)
    )
    graph = graph_of(
        client.get("/?domain=life_sciences", headers={"accept": "text/turtle"})
    )
    described = {
        str(t.object.value)
        for t in graph
        if str(t.predicate.value)
        in (
            "http://www.w3.org/ns/dqv#computedOn",
            "urn:sparqlwatch:notMeasuredOn",
        )
    }
    assert listed, "the fixture must yield a domain match, or this test proves nothing"
    assert "https://sparql.uniprot.org/sparql" in listed
    assert described == listed


def test_the_default_facet_narrows_both_representations_of_the_bare_url(
    client_for, store_dormant_newest
):
    """The single-predicate invariant, at the URL most likely to break it.

    `/` carries a default facet since 2026-09-18 -- `available` -- and the
    filter in `_index_description` was guarded by `if q or domain or facet:`,
    which is False for a request that names nothing. Left alone, the bare `/`
    would have served every endpoint as Turtle while its own HTML showed only
    those that answer: one URL, two endpoint sets, which is exactly what
    index.rq's header forbids and what sharing `_matches_facet` between the
    branches is for.

    store_dormant_newest is the fixture that can show it: three endpoints, one
    of them dormant, so `available` and `all` are genuinely different sets and
    a Turtle document that ignored the default would name an endpoint the page
    does not list.
    """
    client = client_for(store_dormant_newest)

    default_html = set(rows_of(client.get("/", headers={"accept": "text/html"}).text))
    everything = set(
        rows_of(client.get("/?facet=all", headers={"accept": "text/html"}).text)
    )
    assert default_html < everything, (
        "the default has to narrow something in this fixture, or this test "
        "passes on a service that ignores it"
    )

    graph = graph_of(client.get("/", headers={"accept": "text/turtle"}))
    named = {
        str(node.value)
        for quad in graph
        for node in (quad.subject, quad.object)
        if isinstance(node, NamedNode)
    }
    for dropped in everything - default_html:
        assert dropped not in named, (
            f"the bare URL's Turtle names {dropped}, which its own HTML does "
            f"not list"
        )
    for listed in default_html:
        assert listed in named, f"the Turtle drops {listed}, which the HTML lists"
