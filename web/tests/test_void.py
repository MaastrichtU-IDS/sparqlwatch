"""The derived VoID description.

The half of this project's thesis that measurement alone does not reach: not
"your description is missing" but "here is one". Nine of the nine endpoints in
prober/registry/kg-catalog.toml publish nothing a tool can read, so this is the
only description of them there is.
"""

from __future__ import annotations

import urllib.parse

import pytest
from pyoxigraph import BlankNode, NamedNode, RdfFormat, Store, parse
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


def test_a_browser_gets_the_turtle_as_text_rather_than_a_refusal(
    client_for, store_content_profiles
):
    """Still no HTML rendering. The bytes are the Turtle a machine gets; only
    the label changes, so a browser paints them instead of downloading a file.

    This asserted a 406 until 2026-09-17. The 406 was correct about the
    resource -- there is no HTML here, and a third rendering of these facts
    would be a third thing to keep true -- and wrong about the reader: the
    endpoint page prints this url as a link, and every person who clicked it
    reached a refusal from a page that had just offered it.

    What must not come back is a second rendering. The last assertion is what
    holds that: the text/plain body has to PARSE as Turtle and describe the same
    thing the text/turtle body does. Compared as triple counts and IRI triples
    rather than as bytes, because blank node labels are minted per serialisation
    and two calls never agree on them -- which is not a difference in what is
    said.
    """
    client = client_for(store_content_profiles)
    response = fetch(client, PROFILED, accept="text/html")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/plain")
    assert "<html" not in response.text.lower()
    turtle = fetch(client, PROFILED, accept="text/turtle")

    def shape(body):
        triples = list(parse(body.encode(), format=RdfFormat.TURTLE))
        named = {
            (str(t.subject), str(t.predicate), str(t.object))
            for t in triples
            if isinstance(t.subject, NamedNode) and not isinstance(t.object, BlankNode)
        }
        return len(triples), named

    assert shape(response.text) == shape(turtle.text)
    assert shape(response.text)[0] > 0


def test_the_turtle_declares_prefixes_and_still_says_the_same_thing(
    client_for, store_content_profiles
):
    """A document a person can read, without becoming a different document.

    Served with no prefixes until 2026-09-17, so every term was a full IRI in
    angle brackets and semopenalex's description ran to 47.9 KB -- the owner
    asked whether it could be rendered nicely and this is the half that costs
    nothing. Both halves are asserted, because only the pair is the claim:
    prefixes ARE declared and used, and the triples are unchanged. Prefixed
    Turtle that dropped or altered a statement would be a smaller document
    saying something else.
    """
    client = client_for(store_content_profiles)
    body = fetch(client, PROFILED, accept="text/turtle").text
    assert body.startswith("@prefix ") or "\n@prefix " in body
    assert "@prefix void: <http://rdfs.org/ns/void#> ." in body
    assert "@prefix sw: <urn:sparqlwatch:> ." in body
    # Declared AND used: a document full of @prefix lines that still writes
    # every term out in full is the same document with a longer header.
    assert "a void:Dataset" in body, body[:400]
    assert "<http://rdfs.org/ns/void#Dataset>" not in body

    # The same triples. Parsed rather than compared as text, and blank nodes
    # excluded, because their labels are minted per serialisation.
    ntriples = fetch(client, PROFILED, accept="application/n-triples").text
    def shape(text, fmt):
        triples = list(parse(text.encode(), format=fmt))
        return len(triples), {
            (str(t.subject), str(t.predicate), str(t.object))
            for t in triples
            if isinstance(t.subject, NamedNode) and not isinstance(t.object, BlankNode)
        }
    assert shape(body, RdfFormat.TURTLE) == shape(ntriples, RdfFormat.N_TRIPLES)


def test_the_document_and_the_explorer_abbreviate_a_term_the_same_way(
    client_for, store_content_profiles
):
    """One table, not two. A class the explorer labels `skos:` must not be
    `core:` in the document beside it, which is what a second hand-written
    prefix list here would eventually produce.

    VOID_PREFIXES inverts explore_payload's own table rather than restating it,
    so this is a guard on that sharing surviving, not on the values.
    """
    from explore_payload import WELL_KNOWN_PREFIXES
    from void_document import VOID_PREFIXES

    for namespace, prefix in WELL_KNOWN_PREFIXES.items():
        assert prefix in VOID_PREFIXES, f"{prefix} is on the explorer and not here"
    # Not injective: http://schema.org/ and https://schema.org/ share `schema`,
    # and Turtle may declare a prefix once. First-wins is the rule, so the
    # document keeps the http form and the https one writes out in full.
    assert VOID_PREFIXES["schema"] == "http://schema.org/"
    # The document's own five are never overwritten by that merge.
    assert VOID_PREFIXES["void"] == "http://rdfs.org/ns/void#"
    assert VOID_PREFIXES["sw"] == "urn:sparqlwatch:"


def test_an_accept_naming_only_unservable_types_is_still_a_406(
    client_for, store_content_profiles
):
    """The browser case above is `text/html` and `*/*` -- a reader, or a client
    saying "whatever you have". A client that names something specific and
    unservable is a different request, and handing it Turtle while reporting
    success is the confident wrong answer 406 exists to avoid.
    """
    response = fetch(client_for(store_content_profiles), PROFILED, accept="application/json")
    assert response.status_code == 406
    assert "text/turtle" in response.text


# ---------------------------------------------------------------------------
# The document, as a table on the endpoint page
# ---------------------------------------------------------------------------


def test_the_table_shows_exactly_what_the_document_holds(store_content_profiles):
    """The table is a RENDERING of the document, not a second reading of the
    store, and this is what makes that true rather than intended.

    `void_partitions` walks the triples `void_triples` emits, for the reason
    `void_summary` gives: a second reader working these out by its own
    arithmetic would eventually disagree with the document it claims to render,
    in front of somebody deciding whether to depend on it. So every class and
    every property in the table is compared against the triples themselves.
    """
    from void_document import VOID, void_partitions, void_triples

    store = store_content_profiles
    rows = void_partitions(store, PROFILED)
    assert rows, "this fixture describes no classes"

    triples = void_triples(store, PROFILED, "urn:test")
    in_doc_classes = {
        str(t.object.value) for t in triples if t.predicate.value == VOID + "class"
    }
    in_doc_props = {
        str(t.object.value) for t in triples if t.predicate.value == VOID + "property"
    }
    assert {r["class"] for r in rows} == in_doc_classes
    assert {p["property"] for r in rows for p in r["properties"]} == in_doc_props


def test_every_count_says_whether_it_was_scanned_or_sampled(
    store_content_profiles, store_sampled_profile
):
    """The distinction the document spends an rdfs:comment on.

    A count is `void:entities`, scanned exactly, or `sw:sampledEntities`, drawn
    from a sample of the class and NOT generalising to it. `void_triples` picks
    between those two predicates from the sampling value and nothing else, so
    the table's sampling column is not a hint about which it is -- it is that
    same decision, shown. This asserts the two agree for every row, which is
    what lets the table carry one column instead of marking each number.

    BOTH fixtures, and that is the assertion. store_sampled_profile is
    byte-identical to store_content_profiles but for the sampling, so a run of
    this against the exact one alone proves nothing: every `sampled` there is
    False and a function hard-coded to return False passes it. Verified by
    making exactly that change.
    """
    from void_document import void_partitions

    seen = set()
    for store in (store_content_profiles, store_sampled_profile):
        rows = void_partitions(store, PROFILED)
        assert rows, "a fixture describes no classes"
        for row in rows:
            exact = row["sampling"] == "exact"
            seen.add(row["sampling"])
            assert row["sampled"] is not exact, row
            for prop in row["properties"]:
                assert prop["sampled"] is not exact, (row["class"], prop)
    assert len(seen) > 1, f"both fixtures sampled the same way: {seen}"


def test_one_namespace_gets_one_prefix_and_two_never_share_one():
    """The same bug this repository shipped on the vocabulary list earlier on
    2026-09-17, guarded here before it could be shipped twice.

    `_prefix_for` resolves a collision by counting up. Asked once per TERM with
    every previous answer marked taken, it splits one vocabulary across `ns`,
    `ns2`, `ns3` -- exactly what its own docstring says a suffix must never
    mean. It is asked once per NAMESPACE here.

    ASKED OF THE FUNCTION, not of a fixture, and that is the point. Every
    committed profile fixture holds ONE namespace the document does not already
    declare, so against a store there is nothing for a repeated namespace to
    collide with and the per-term bug passes unnoticed -- verified by
    reintroducing it. The input below is what a real endpoint looks like:
    several terms per namespace, several namespaces, and two whose last path
    segment is the same word.
    """
    from void_document import _generated_prefixes

    labels = _generated_prefixes([
        "https://example.org/onto/Author",
        "https://example.org/onto/Work",
        "https://example.org/onto/cites",
        "https://other.example/onto/Author",
        "http://purl.org/spar/fabio/Paper",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
    ])
    namespaces = list(labels.values())
    assert len(namespaces) == len(set(namespaces)), f"a namespace twice: {labels}"
    assert len(labels) == len(set(labels)), f"a label twice: {labels}"
    # rdf: is already declared by the document, so it is not generated again.
    assert "http://www.w3.org/1999/02/22-rdf-syntax-ns#" not in namespaces
    # The three example.org/onto terms share one label, and other.example's
    # identically-named namespace gets its own rather than merging into it.
    assert labels["onto"] in ("https://example.org/onto/", "https://other.example/onto/")
    assert len(namespaces) == 3, labels


def test_the_endpoint_page_draws_the_table_closed(client_for, store_content_profiles):
    """An accordion, shut, for the reason the index's matrix is one: a
    mid-sized endpoint has 69 classes with their properties, which is a page of
    its own, and a reader who came for the verdicts should not scroll past it.
    """
    from urllib.parse import quote

    body = client_for(store_content_profiles).get(
        "/endpoint?url=" + quote(PROFILED, safe=""), headers={"accept": "text/html"}
    ).text
    assert "data-void-table" in body, "the endpoint page draws no VoID table"
    opening = body[body.index("<details") : body.index("data-void-table") + 40]
    assert " open" not in opening, f"the table starts open: {opening!r}"
    classes = body.count("data-void-class=")
    assert classes == len(_partitions(store_content_profiles)), classes


def _partitions(store):
    from void_document import void_partitions

    return void_partitions(store, PROFILED)


def test_naming_no_endpoint_is_a_400(client_for, store_content_profiles):
    response = client_for(store_content_profiles).get(
        VOID_PATH, headers={"accept": "text/turtle"}
    )
    assert response.status_code == 400


# ---------------------------------------------------------------------------
# Provably complete, or sampled only
# ---------------------------------------------------------------------------
# The one thing a consumer has to read before using this document for anything.


def flag(ts, predicate):
    got = objects(ts, predicate)
    return got[0].value if got else None


def test_a_document_says_whether_it_can_be_trusted_as_the_whole_endpoint(
    client_for, store_content_profiles
):
    """The flag is always present, whichever way it falls.

    A consumer must never have to infer completeness by counting partitions and
    comparing samplings itself: two readers doing that arithmetic would be two
    chances to get it wrong, and the one with the evidence is this one.
    """
    ts = triples(fetch(client_for(store_content_profiles), PROFILED))
    assert flag(ts, SW + "provablyComplete") in {"true", "false"}
    assert flag(ts, SW + "classesDescribed") is not None


def test_an_exactly_scanned_endpoint_that_states_no_class_count_is_not_complete(
    client_for, store_content_profiles
):
    """Exact sampling alone does not prove the CLASS LIST is whole.

    This fixture scanned every instance of every class it found, and still
    cannot be called complete: the endpoint never said how many classes it has,
    so a truncated enumeration would look exactly like this one. Unproven and
    incomplete have to read the same way, because acting on either is the same
    mistake.
    """
    ts = triples(fetch(client_for(store_content_profiles), PROFILED))
    assert {o.value for o in objects(ts, SW + "sampling")} == {"exact"}
    assert flag(ts, SW + "classesReported") is None, "this fixture has no class count"
    assert flag(ts, SW + "provablyComplete") == "false"


def test_a_sampled_endpoint_is_never_complete(client_for, store_sampled_profile):
    """Whatever else holds, a sampled class means the counts are not the
    endpoint's, so the document cannot stand for the whole of it."""
    ts = triples(fetch(client_for(store_sampled_profile), PROFILED))
    assert flag(ts, SW + "provablyComplete") == "false"


def test_completeness_needs_all_four_conditions():
    """The rule itself, at its boundaries.

    Asserted on the function because reaching each corner through a store means
    a fixture per corner, and the corners are what the claim rests on.
    """
    from void_document import _provably_complete

    assert _provably_complete(5, 5, {"exact"}), "every condition met"
    assert not _provably_complete(5, 5, {"exact", "first-n"}), "one class sampled"
    assert not _provably_complete(5, 5, {"first-n"}), "all classes sampled"
    assert not _provably_complete(5, 6, {"exact"}), "a class was never described"
    assert not _provably_complete(5, None, {"exact"}), "no total to check against"
    assert not _provably_complete(0, 0, set()), "a document about nothing is not complete"


# ---------------------------------------------------------------------------
# What the endpoint page claims about the document
# ---------------------------------------------------------------------------


def page(client, endpoint):
    return client.get(
        "/endpoint", params={"url": endpoint}, headers={"accept": "text/html"}
    ).text


def test_the_page_offers_the_document_and_says_which_kind_it_is(
    client_for, store_content_profiles
):
    """The offer and its qualifier are one block, never one without the other.

    Every other section of that page grades the endpoint. This is the only one
    that hands the reader something, and something a consumer might depend on,
    so the sentence that says how far to trust it cannot be somewhere else on
    the page or only inside the RDF.
    """
    body = page(client_for(store_content_profiles), PROFILED)
    assert 'data-section="void"' in body
    assert "/void?url=" in body, "the document is not linked"
    assert ("Provably complete" in body) or ("A sample only" in body)


def test_a_sampled_document_is_never_offered_as_complete(
    client_for, store_sampled_profile
):
    """The claim follows the evidence, on the page as in the RDF."""
    body = page(client_for(store_sampled_profile), PROFILED)
    assert "A sample only" in body
    assert "Provably complete" not in body


def test_an_endpoint_with_no_profile_offers_nothing(client_for, store):
    """No block at all, rather than a link to a 404.

    An offer that leads nowhere is worse than no offer: it tells a reader we
    have a description of this endpoint, which is the thing that is not true.
    """
    body = page(client_for(store), "https://data.kkg.kadaster.nl/query")
    assert 'data-section="void"' not in body


# ---------------------------------------------------------------------------
# The vocabulary the document publishes
# ---------------------------------------------------------------------------


def test_every_published_term_is_documented():
    """A term the document emits and the docs do not explain is unreachable.

    These go into RDF under `urn:sparqlwatch:` IRIs, which resolve to nothing,
    so the docs page is the only place a consumer meeting one can learn what it
    licenses. Compared against the source rather than a list kept by hand,
    because a list kept by hand is what drifts.
    """
    import re
    from pathlib import Path as P

    import void_document

    source = P(void_document.__file__).read_text()
    # Terms the builder actually emits, as `SW + "name"`. The docs table itself
    # is keyed by bare name, so it cannot be mistaken for an emission.
    emitted = set(re.findall(r'SW \+ "([a-zA-Z]+)"', source))
    assert emitted, "no terms found: the pattern stopped matching the source"
    assert emitted <= set(void_document.TERM_DOCS), (
        "emitted but undocumented: "
        f"{sorted(emitted - set(void_document.TERM_DOCS))}"
    )
    assert set(void_document.TERM_DOCS) <= emitted, (
        "documented but never emitted: "
        f"{sorted(set(void_document.TERM_DOCS) - emitted)}"
    )


def test_the_docs_page_lists_every_term(client_for, store_content_profiles):
    """And the page renders them, rather than merely having them in a dict."""
    import void_document

    body = client_for(store_content_profiles).get("/docs/void").text
    for name in void_document.TERM_DOCS:
        assert f'data-term="{name}"' in body, f"{name} is not on the page"
    assert "provablyComplete" in body


def test_the_docs_index_links_the_new_page(client_for, store_content_profiles):
    """A page nothing links to is a page nobody finds."""
    body = client_for(store_content_profiles).get("/docs").text
    assert "/docs/void" in body
