"""The read-only SPARQL endpoint, and the guards in front of it.

Every guard here was written against a behaviour verified on this build. The
federation one especially: pyoxigraph 0.5.9 does not refuse
`SERVICE <http://...>`, it opens the socket. Measured 2026-09-25 against the
cloud metadata address, the result was a real connection timeout, not a parse
error. These tests exist so that stays refused.
"""

import json

import pytest
from pyoxigraph import RdfFormat, parse

import sparql_service
from app import SPARQL_PATH, app, get_store
from starlette.testclient import TestClient


@pytest.fixture
def client_for():
    """A test client whose routes read the store handed to it.

    The same shape as test_page.py's, kept here rather than moved to conftest:
    this file is about one route and should be readable without reading that
    one. See test_page.py::client_for if the two ever need to agree.
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


def _q(client, query, **kw):
    return client.get(SPARQL_PATH, params={"query": query}, **kw)


# ---------------------------------------------------------------------------
# The guards
# ---------------------------------------------------------------------------
@pytest.mark.parametrize(
    "query",
    [
        "SELECT * WHERE { SERVICE <http://169.254.169.254/> { ?s ?p ?o } }",
        "select * where { service <http://10.0.0.1/> { ?s ?p ?o } }",
        "SELECT * WHERE { ?s ?p ?o . SERVICE <http://x/> { ?a ?b ?c } }",
        "SELECT * WHERE { ?s ?p ?o SERVICE",
        # FAIL CLOSED. The scan blanks literals so that a SERVICE mentioned
        # inside one is not mistaken for federation -- which means an
        # UNTERMINATED literal could otherwise swallow a real SERVICE clause
        # and carry it past the check. Text that cannot be scanned is refused,
        # not waved through: a parse oddity must not become a way in.
        'SELECT * WHERE { ?s ?p "unclosed . SERVICE <http://169.254.169.254/> {?a ?b ?c} }',
    ],
)
def test_federation_is_refused(client_for, store, query):
    """SSRF. Unmitigated this lets a stranger make the pod fetch a URL of
    their choosing -- in-cluster Services, the Kubernetes API, the cloud
    metadata endpoint."""
    r = _q(client_for(store), query)
    assert r.status_code == 400, r.text
    assert "SERVICE is not available" in r.text


@pytest.mark.parametrize(
    "query",
    [
        'SELECT * WHERE { ?s ?p "a SERVICE outage" }',
        "SELECT * WHERE { ?s <http://example.test/ns#SERVICE> ?o }",
        "# SERVICE in a comment\nSELECT * WHERE { ?s ?p ?o }",
        "SELECT * WHERE { ?s ?p ?o FILTER(?a < ?b) }",
    ],
)
def test_a_query_that_merely_says_service_is_allowed(client_for, store, query):
    """An endpoint that refuses legitimate queries with a security message
    teaches people to distrust the message."""
    assert _q(client_for(store), query).status_code == 200


def test_writes_are_refused(client_for, store):
    """Twice over: the store is opened read_only, and query() rejects update
    syntax outright. Neither is relied on alone."""
    for query in (
        "INSERT DATA { <urn:a> <urn:b> <urn:c> }",
        "DELETE WHERE { ?s ?p ?o }",
        "DROP GRAPH <urn:sparqlwatch:current>",
        "LOAD <http://example.test/evil.ttl>",
    ):
        assert _q(client_for(store), query).status_code == 400, query


def test_an_overlong_query_is_refused_before_the_engine_sees_it(client_for, store):
    r = _q(client_for(store), "SELECT * WHERE {?s ?p ?o} #" + "x" * 20000)
    assert r.status_code == 413


def test_a_truncated_result_says_so(store_two_sweeps, client_for, monkeypatch):
    """A truncated answer that does not announce itself is a wrong answer."""
    monkeypatch.setattr(sparql_service, "MAX_ROWS", 1)
    r = _q(client_for(store_two_sweeps), "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }")
    assert r.status_code == 200
    assert r.headers.get("X-SPARQLWatch-Incomplete") == "true"
    body = json.loads(r.text)
    assert body["head"].get("link"), "nothing in the document itself said so"
    assert len(body["results"]["bindings"]) == 1


# ---------------------------------------------------------------------------
# The protocol
# ---------------------------------------------------------------------------
def test_all_three_ways_in(client_for, store):
    """A client following the spec should not have to guess which POST body
    we happened to implement."""
    client = client_for(store)
    ask = "ASK { ?s ?p ?o }"
    got = [
        client.get(SPARQL_PATH, params={"query": ask}),
        client.post(
            SPARQL_PATH,
            content=b"query=ASK+%7B+%3Fs+%3Fp+%3Fo+%7D",
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        ),
        client.post(
            SPARQL_PATH, content=ask, headers={"Content-Type": "application/sparql-query"}
        ),
    ]
    assert [r.status_code for r in got] == [200, 200, 200]
    assert len({r.text for r in got}) == 1, "the three routes disagreed"


def test_cors_is_open_and_preflight_answers(client_for, store):
    """This service reports an endpoint that answers no OPTIONS as failing
    cors-preflight. Publishing one that would fail our own check is not an
    option."""
    client = client_for(store)
    assert _q(client, "ASK {?s ?p ?o}").headers["access-control-allow-origin"] == "*"
    pre = client.options(SPARQL_PATH)
    assert pre.status_code == 204
    assert "POST" in pre.headers["access-control-allow-methods"]


def test_no_query_returns_a_service_description_that_parses(client_for, store):
    """We measure service-description on other endpoints."""
    r = client_for(store).get(SPARQL_PATH)
    assert r.status_code == 200
    assert "turtle" in r.headers["content-type"]
    quads = list(parse(r.content, format=RdfFormat.TURTLE, base_iri="http://x/"))
    sd = "http://www.w3.org/ns/sparql-service-description#"
    preds = {q.predicate.value for q in quads}
    assert f"{sd}endpoint" in preds
    assert f"{sd}supportedLanguage" in preds
    objects = {q.object.value for q in quads}
    assert f"{sd}SPARQL11Query" in objects


def test_the_description_does_not_advertise_federation_it_refuses(client_for, store):
    """Claiming BasicFederatedQuery while rejecting SERVICE would make the
    description a lie a client could act on."""
    body = client_for(store).get(SPARQL_PATH).text
    assert "BasicFederatedQuery" not in body
    assert "federationAvailable false" in body


def test_ask_select_and_construct_each_come_back_in_their_own_form(client_for, store):
    client = client_for(store)
    assert json.loads(_q(client, "ASK {?s ?p ?o}").text)["boolean"] in (True, False)
    body = json.loads(_q(client, "SELECT * WHERE {?s ?p ?o} LIMIT 1").text)
    assert "vars" in body["head"]
    con = _q(client, "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o } LIMIT 1")
    assert "triples" in con.headers["content-type"] or "turtle" in con.headers["content-type"]


# ---------------------------------------------------------------------------
# The dataset this endpoint exposes
# ---------------------------------------------------------------------------
def test_the_default_graph_is_current_and_history_is_still_reachable(
    client_for, store_two_sweeps
):
    """The choice the SPARQL spec leaves open, and this service's answer.

    Every fact here lives in a named graph, one per sweep, so a spec-default
    empty default graph makes `?s ?p ?o` return nothing -- correct and useless.
    The union of every run is 1.4M triples of history. `current` is the view
    the pages render: bounded, fast, and what somebody asking an unqualified
    question means. History is exactly where it was.
    """
    client = client_for(store_two_sweeps)
    plain = json.loads(_q(client, "SELECT * WHERE { ?s ?p ?o }").text)
    assert plain["results"]["bindings"], "the default graph answered nothing"

    graphs = json.loads(
        _q(client, "SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }").text
    )
    assert int(graphs["results"]["bindings"][0]["n"]["value"]) > 1, "history is gone"


def test_a_literal_keeps_its_datatype_and_language(client_for, store):
    """xsd:string is omitted by the spec, so emitting it makes identical
    results compare unequal."""
    r = _q(
        client_for(store),
        'SELECT * WHERE { VALUES (?a ?b ?c) { ("plain" "en"@en 1) } }',
    )
    row = json.loads(r.text)["results"]["bindings"][0]
    assert "datatype" not in row["a"], row["a"]
    assert row["b"]["xml:lang"] == "en"
    assert row["c"]["datatype"].endswith("integer")
