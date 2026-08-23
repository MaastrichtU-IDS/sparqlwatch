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

from html.parser import HTMLParser

import pytest
from pyoxigraph import Literal, NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

from app import ENDPOINT_PATH, app, get_store

KADASTER = "https://data.kkg.kadaster.nl/query"
TRUNCATED = "https://truncated.example/sparql"
UNKNOWN = "https://nobody-has-ever-probed-this.example/sparql"

DQV = "http://www.w3.org/ns/dqv#"
SW = "urn:sparqlwatch:"
M = SW + "metric:"

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

    run-two-sweeps.nq measures kadaster's sw:metric:classes as "verified" in
    the current (16:00) run and "indeterminate" in the stale (14:00) one.
    The HTML path picks its run through endpoint_measurements.rq's SELECT and
    the RDF path through endpoint_description.rq's CONSTRUCT, so the two can
    drift: this asserts they agree, and that they agree on the current run's
    value rather than merely on each other.
    """
    client = client_for(store_two_sweeps)

    shown = html_verdicts(get(client, KADASTER, accept="text/html"))
    assert shown[M + "classes"] == CURRENT_CLASSES_VERDICT

    graph = graph_of(get(client, KADASTER, accept="text/turtle"))
    measurement = subjects_with(
        graph, NamedNode(DQV + "isMeasurementOf"), NamedNode(M + "classes")
    )
    assert len(measurement) == 1, "two runs measured this metric; one is current"
    served = {
        quad.object.value
        for quad in graph.quads_for_pattern(
            measurement.copy().pop(), NamedNode(DQV + "value"), None
        )
    }
    assert served == {shown[M + "classes"]}
    assert served == {CURRENT_CLASSES_VERDICT}
    assert STALE_CLASSES_VERDICT not in served
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
