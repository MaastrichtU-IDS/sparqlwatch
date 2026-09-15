"""The derived VoID description.

The half of this project's thesis that measurement alone does not reach: not
"your description is missing" but "here is one". Nine of the nine endpoints in
prober/registry/kg-catalog.toml publish nothing a tool can read, so this is the
only description of them there is.
"""

from __future__ import annotations

import urllib.parse

import pytest
from pyoxigraph import NamedNode, RdfFormat, Store, parse
from starlette.testclient import TestClient

from app import VOID_PATH, app, get_store

# The endpoint both profile fixtures describe.
PROFILED = "http://127.0.0.1:9200/sparql"

VOID = "http://rdfs.org/ns/void#"
SW = "urn:sparqlwatch:"


@pytest.fixture
def client_for():
    def build(store: Store) -> TestClient:
        app.dependency_overrides[get_store] = lambda: store
        return TestClient(app)

    yield build
    app.dependency_overrides.clear()


def fetch(client, endpoint, accept="text/turtle"):
    return client.get(
        VOID_PATH,
        params={"url": endpoint},
        headers={"accept": accept},
    )


def triples(response):
    return list(parse(response.content, format=RdfFormat.TURTLE))


def objects(ts, predicate):
    node = NamedNode(predicate)
    return [t.object for t in ts if t.predicate == node]


def test_the_document_describes_our_observation_and_not_their_dataset(
    client_for, store_content_profiles
):
    """The subject is ours, and it says so in its own triples.

    A derived description that presented itself as the endpoint's own would be
    putting words in a publisher's mouth, which is the one thing a service
    built on "measured rather than asserted" may not do. So the subject is the
    document's own url, the endpoint is the OBJECT of void:sparqlEndpoint and
    prov:wasDerivedFrom, and a comment says which is which for a reader who
    dereferences this and nothing else.
    """
    ts = triples(fetch(client_for(store_content_profiles), PROFILED))
    target = NamedNode(PROFILED)
    assert target in objects(ts, VOID + "sparqlEndpoint")
    assert target in objects(ts, "http://www.w3.org/ns/prov#wasDerivedFrom")
    # The endpoint is never the SUBJECT: nothing here speaks for it.
    assert not [t for t in ts if t.subject == target], "the document speaks for the endpoint"
    comment = objects(ts, "http://www.w3.org/2000/01/rdf-schema#comment")
    assert comment and "Not the endpoint's own description" in comment[0].value


def test_a_sampled_count_never_wears_a_population_predicate(
    client_for, store_sampled_profile
):
    """The rule the whole module is arranged around.

    `void:entities` is a statement about every instance of a class. A profile
    drawn from a hash prefix or from the bounded rung is a statement about the
    sample, and `Sampling`'s header records a bounded sample being wrong by
    0.950. Publishing one under VoID's own term would be a confident wrong
    claim about somebody else's data.
    """
    ts = triples(fetch(client_for(store_sampled_profile), PROFILED))
    samplings = {o.value for o in objects(ts, SW + "sampling")}
    assert samplings and samplings != {"exact"}, "this fixture must contain a sampled profile"
    assert not objects(ts, VOID + "entities"), (
        "a sampled profile stated a population count"
    )
    assert objects(ts, SW + "sampledEntities"), "the observation must still be published"


def test_an_exact_profile_does_state_the_population_count(
    client_for, store_content_profiles
):
    """The other half: the rule must not make the document useless.

    Where the pass scanned every instance, the count IS the population's, and
    refusing to say so in VoID's own vocabulary would make the document
    unreadable by the tools it exists for.
    """
    ts = triples(fetch(client_for(store_content_profiles), PROFILED))
    assert {o.value for o in objects(ts, SW + "sampling")} == {"exact"}
    assert objects(ts, VOID + "entities"), "an exact scan must state void:entities"


def test_the_partitions_hang_off_their_class_and_not_off_the_dataset(
    client_for, store_content_profiles
):
    """A partition is what makes this a description rather than a bag of terms.

    A property observed on instances of one class says nothing about another,
    and a document that listed every property against the dataset would be
    claiming it did.
    """
    ts = triples(fetch(client_for(store_content_profiles), PROFILED))
    class_nodes = {t.object for t in ts if t.predicate == NamedNode(VOID + "classPartition")}
    assert class_nodes, "no class partitions at all"
    for t in ts:
        if t.predicate == NamedNode(VOID + "propertyPartition"):
            assert t.subject in class_nodes, "a property partition hangs off the dataset"


def test_an_endpoint_with_no_profile_is_a_404_and_not_an_empty_description(
    client_for, store_content_profiles
):
    """An empty document would say the endpoint holds nothing.

    That is a claim about the endpoint. The 404 says the true thing, which is
    about us: we have not profiled this.
    """
    response = fetch(client_for(store_content_profiles), "https://nobody.example/sparql")
    assert response.status_code == 404
    assert "nothing to describe" in response.text


def test_the_resource_is_rdf_only(client_for, store_content_profiles):
    """No HTML rendering, and a 406 that names what is on offer.

    A person reading about an endpoint has the endpoint page and the explorer.
    A third rendering of the same facts is a third thing to keep true.
    """
    response = fetch(client_for(store_content_profiles), PROFILED, accept="text/html")
    assert response.status_code == 406
    assert "text/turtle" in response.text


def test_naming_no_endpoint_is_a_400(client_for, store_content_profiles):
    response = client_for(store_content_profiles).get(
        VOID_PATH, headers={"accept": "text/turtle"}
    )
    assert response.status_code == 400
