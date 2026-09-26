"""The service's own VoID description at /.well-known/void.

Not to be confused with void_document.py's, which describes an endpoint we
MEASURED. This one describes the dataset we publish, and the distinction is
the thing most worth protecting: a reader must not come away believing this
dataset contains the endpoints' data, or that its triple count says anything
about anyone's store but ours.
"""

import pytest
from pyoxigraph import RdfFormat, Store, parse
from starlette.testclient import TestClient

import sparql_pool
import void_self
from app import WELL_KNOWN_VOID_PATH, app, get_sparql_pool, get_store

VOID = "http://rdfs.org/ns/void#"
DCTERMS = "http://purl.org/dc/terms/"


@pytest.fixture
def client_for():
    clients = []

    def build(store):
        app.dependency_overrides[get_store] = lambda: store
        # /sparql needs the pool too: one of these tests reads the service
        # description to check both documents agree about the location.
        app.dependency_overrides[get_sparql_pool] = lambda: sparql_pool.DirectPool(store)
        client = TestClient(app)
        clients.append(client)
        return client

    yield build
    app.dependency_overrides.clear()
    for client in clients:
        client.close()


def _graph(client):
    response = client.get(WELL_KNOWN_VOID_PATH)
    assert response.status_code == 200, response.text
    assert "turtle" in response.headers["content-type"]
    return response, list(parse(response.content, format=RdfFormat.TURTLE,
                                base_iri="http://testserver/"))


def test_it_is_parseable_rdf_and_names_the_endpoint(client_for, store):
    """A description nothing can parse is not a description."""
    response, quads = _graph(client_for(store))
    preds = {q.predicate.value for q in quads}
    assert f"{VOID}sparqlEndpoint" in preds
    assert f"{VOID}triples" in preds
    endpoint = next(q.object.value for q in quads if q.predicate.value == f"{VOID}sparqlEndpoint")
    assert endpoint.endswith("/sparql")


def test_the_document_points_at_the_dataset_it_describes(client_for, store):
    """`.well-known/void` is a DatasetDescription, not the dataset. A consumer
    follows foaf:primaryTopic to the thing with the counts on it."""
    _, quads = _graph(client_for(store))
    kinds = {(q.subject.value, q.object.value) for q in quads
             if q.predicate.value.endswith("22-rdf-syntax-ns#type")}
    assert any(s.endswith("/.well-known/void") and o == f"{VOID}DatasetDescription"
               for s, o in kinds), kinds
    topic = next(q.object.value for q in quads if q.predicate.value.endswith("primaryTopic"))
    assert any(s == topic and o == f"{VOID}Dataset" for s, o in kinds)


def test_the_counts_are_the_store_s_own_and_not_placeholders(client_for, store):
    """Measured, not declared-and-hoped. If these ever stop tracking the store,
    the description becomes the kind of claim this project exists to catch."""
    client = client_for(store)
    _, quads = _graph(client)
    # BY SUBJECT as well as predicate: the document carries two datasets, and
    # both state void:triples. Keying on the predicate alone let the subset's
    # count silently stand in for the whole dataset's.
    topic = next(q.object.value for q in quads if q.predicate.value.endswith("primaryTopic"))
    stated = {
        q.predicate.value.rsplit("#", 1)[-1]: int(q.object.value)
        for q in quads
        if q.subject.value == topic
        and q.predicate.value.startswith(VOID)
        and q.object.value.isdigit()
    }
    real = list(store.query("SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"))
    assert stated["triples"] == int(real[0]["n"].value)
    assert stated["triples"] > 0, "a fixture with no triples proves nothing here"

    # And the subset really is the smaller one, which is what makes the two
    # numbers worth telling apart.
    subset = next(q.object.value for q in quads if q.predicate.value == f"{VOID}subset")
    subset_triples = next(int(q.object.value) for q in quads
                          if q.subject.value == subset and q.predicate.value == f"{VOID}triples")
    assert subset_triples < stated["triples"]


def test_it_says_whose_data_this_is(client_for, store):
    """THE property worth protecting. The subject is our observations, never
    the endpoints observed, and the description has to say so itself -- the
    same rule void_document.py follows when describing somebody else."""
    _, quads = _graph(client_for(store))
    description = next(q.object.value for q in quads
                       if q.predicate.value == f"{DCTERMS}description")
    assert "OBSERVED" in description
    assert "does not contain their data" in description


def test_the_default_graph_is_declared_as_a_subset(client_for, store):
    """A consumer who writes `?s ?p ?o` against our endpoint gets `current`,
    not the whole history. Nothing else on the open web would tell them why."""
    _, quads = _graph(client_for(store))
    subsets = [q.object.value for q in quads if q.predicate.value == f"{VOID}subset"]
    assert subsets, "the default graph is undeclared"
    named = {q.object.value for q in quads if q.predicate.value.endswith("#name")}
    assert "urn:sparqlwatch:current" in named


def test_it_is_readable_from_a_browser(client_for, store):
    """This service marks endpoints down for missing CORS; a description a
    browser cannot fetch would be the same failure in our own house."""
    response = client_for(store).get(WELL_KNOWN_VOID_PATH)
    assert response.headers["access-control-allow-origin"] == "*"


def test_it_names_the_host_the_reader_actually_reached(client_for, store):
    """Baked-in hostnames are how a description ends up describing staging."""
    _, quads = _graph(client_for(store))
    assert any(q.subject.value.startswith("http://testserver/") for q in quads)


def test_the_data_licence_is_cc_by_and_not_the_code_licence(client_for, store):
    """The split, and why it has to be asserted rather than assumed.

    The software is Apache-2.0 and the measurements are CC BY 4.0. Those files
    now deliberately DISAGREE, so a test pinning the document against LICENSE
    -- which is what the previous one did -- would now enforce exactly the
    confusion this split exists to remove. Each is pinned to its own file.

    Apache-2.0 is a software licence: its terms speak of source and object
    form, of contributions and of patent grants, none of which map onto a set
    of observations. A catalogue that lists datasets looks for a data licence.
    """
    from pathlib import Path

    _, quads = _graph(client_for(store))
    stated = [q.object.value for q in quads if q.predicate.value == f"{DCTERMS}license"]
    assert stated == ["https://creativecommons.org/licenses/by/4.0/"], stated
    assert not any("apache.org" in url for url in stated), "the code licence leaked into the data"

    root = Path(__file__).resolve().parents[2]
    code = (root / "LICENSE").read_text()
    assert "Apache License" in code and "Version 2.0" in code
    data = (root / "LICENSE-DATA").read_text()
    assert "CC BY 4.0" in data
    assert "creativecommons.org/licenses/by/4.0" in data


def test_it_says_who_to_attribute_by_identifier_and_by_name(client_for, store):
    """CC BY REQUIRES attribution, so a licence URI alone is not enough.

    A consumer told they must attribute and not told to whom cannot comply.
    And the two halves are both needed: the ORCID makes the creator a resource
    a catalogue can resolve and reconcile rather than match by spelling, while
    the name is what actually goes in an attribution line -- an identifier
    cannot be written into one. So the document carries the ORCID on the
    dataset and the name on the ORCID, one hop away, in the same file.
    """
    from pathlib import Path

    _, quads = _graph(client_for(store))
    orcid = "https://orcid.org/0000-0003-4727-9435"
    attributed = {q.object.value for q in quads
                  if q.predicate.value in (f"{DCTERMS}creator", f"{DCTERMS}rightsHolder")}
    assert attributed == {orcid}, attributed

    named = {q.object.value for q in quads
             if q.subject.value == orcid and q.predicate.value.endswith("foaf/0.1/name")}
    assert named == {"Michel Dumontier"}, (
        "the identifier resolves to nobody inside this document, so a consumer "
        f"cannot write the attribution line: {named}"
    )

    # The repository's own attribution line has to agree with both halves.
    data = (Path(__file__).resolve().parents[2] / "LICENSE-DATA").read_text()
    assert "Michel Dumontier" in data
    assert orcid in data


def test_the_orcid_checksum_is_valid(client_for, store):
    """An ORCID carries an ISO 7064 MOD 11-2 check digit, and one that fails
    it points at nobody. Published RDF asserting a broken identifier is worse
    than publishing none: it is a claim about a person who does not exist."""
    _, quads = _graph(client_for(store))
    orcid = next(q.object.value for q in quads
                 if q.predicate.value == f"{DCTERMS}creator")
    digits = orcid.rsplit("/", 1)[-1].replace("-", "")
    total = 0
    for digit in digits[:-1]:
        total = (total + int(digit)) * 2
    expected = (12 - total % 11) % 11
    assert digits[-1] == ("X" if expected == 10 else str(expected)), orcid


def test_the_copyright_holder_is_stated_in_both_licence_files(client_for, store):
    """A licence with an unfilled holder grants nothing clearly."""
    from pathlib import Path

    root = Path(__file__).resolve().parents[2]
    for name in ("LICENSE", "LICENSE-DATA"):
        text = (root / name).read_text()
        assert "Michel Dumontier" in text, name
        assert "[name of copyright owner]" not in text, name


def test_the_count_cache_is_keyed_on_the_store(store, store_two_sweeps):
    """A cache keyed on nothing is right in production -- one store per
    process -- and wrong in every test. This codebase has already shipped that
    bug once, in the page cache."""
    first = void_self.counts(store)
    second = void_self.counts(store_two_sweeps)
    assert first is not second
    assert void_self.counts(store) == first


# ---------------------------------------------------------------------------
# The scheme
#
# Traefik terminates TLS and forwards plain HTTP, so request.url says `http`
# for a site every reader reaches over `https`. Harmless in a redirect; not
# harmless in RDF, where an IRI differing by scheme is a different IRI.
#
# It shipped wrong: the deployed VoID and service description both named
# `http://` on an HTTPS-only site, because the response cache keyed without
# the forwarded scheme and the readiness probe -- which reaches the pod
# directly, with no such header, every ten seconds -- always won the race to
# populate it.
# ---------------------------------------------------------------------------
def test_the_document_names_the_scheme_the_reader_used(client_for, store):
    client = client_for(store)
    secure = client.get(WELL_KNOWN_VOID_PATH, headers={"X-Forwarded-Proto": "https"}).text
    assert "<https://testserver/.well-known/void>" in secure, secure[:200]
    assert "void:sparqlEndpoint <https://" in secure


def test_the_cache_does_not_serve_one_scheme_to_the_other(client_for, store):
    """THE bug, and the reason it reached production.

    A probe with no forwarded header and a reader with one must not share a
    cache entry, or whichever arrives first decides what everyone is told.
    """
    client = client_for(store)
    probe = client.get(WELL_KNOWN_VOID_PATH).text
    reader = client.get(WELL_KNOWN_VOID_PATH, headers={"X-Forwarded-Proto": "https"}).text
    assert "<http://testserver/" in probe
    assert "<https://testserver/" in reader
    assert probe != reader

    # And the order must not matter: ask again the other way round.
    again_probe = client.get(WELL_KNOWN_VOID_PATH).text
    assert again_probe == probe


def test_an_unknown_forwarded_scheme_is_ignored(client_for, store):
    """The header is trusted for a scheme and nothing else. Anything that is
    not http or https is not a scheme this service will name itself with."""
    body = client_for(store).get(
        WELL_KNOWN_VOID_PATH, headers={"X-Forwarded-Proto": "gopher"}
    ).text
    assert "gopher://" not in body
    assert "<http://testserver/" in body


# ---------------------------------------------------------------------------
# Where the server is
#
# A property of the SERVICE, never of the data, and that distinction is the
# whole reason these tests exist rather than a single "location is present".
# ---------------------------------------------------------------------------
SCHEMA = "https://schema.org/"
WGS84 = "http://www.w3.org/2003/01/geo/wgs84_pos#"
MAASTRICHT = "https://sws.geonames.org/2751283/"


def test_the_location_hangs_off_the_endpoint_and_never_off_the_dataset(client_for, store):
    """THE modelling decision, and getting it wrong would publish a falsehood.

    `dcterms:spatial` on a `void:Dataset` means the spatial COVERAGE of the
    data. This dataset covers SPARQL endpoints in Japan, Brazil, Switzerland
    and wherever else the registry reaches, so saying it is "about Maastricht"
    would be false -- and a consumer filtering a catalogue by region would be
    told these are Dutch measurements.

    What is true is that the machine answering sits in Maastricht, which is a
    fact about latency and about who to ask when it stops. That hangs off the
    endpoint.
    """
    _, quads = _graph(client_for(store))
    topic = next(q.object.value for q in quads if q.predicate.value.endswith("primaryTopic"))

    located = [q for q in quads if q.predicate.value == f"{SCHEMA}location"]
    assert located, "the location is missing"
    assert all(q.subject.value.endswith("/sparql") for q in located), (
        f"the location is stated about something other than the endpoint: "
        f"{[q.subject.value for q in located]}"
    )
    assert not any(q.subject.value == topic for q in located), (
        "the location is on the dataset, which claims the DATA is from there"
    )
    # And no coverage claim crept in under another name.
    assert not any(q.predicate.value.endswith("terms/spatial") for q in quads)


def test_the_place_is_identified_and_not_only_named(client_for, store):
    """A string is not a place. The GeoNames URI is what lets a consumer
    reconcile it; the coordinates are what lets one that will not dereference
    still use it."""
    _, quads = _graph(client_for(store))
    place = next(q.object.value for q in quads if q.predicate.value == f"{SCHEMA}location")
    assert place == MAASTRICHT, place

    about = {q.predicate.value: q.object.value for q in quads if q.subject.value == place}
    assert about.get(f"{SCHEMA}name") == "Maastricht, Netherlands", about
    # Read from GeoNames rather than recalled, and a city does not move.
    assert about[f"{WGS84}lat"].startswith("50.8"), about
    assert about[f"{WGS84}long"].startswith("5.6"), about


def test_the_service_description_says_the_same_thing(client_for, store):
    """Two documents about one service must not be readable as disagreeing."""
    from pyoxigraph import RdfFormat, parse

    from app import SPARQL_PATH

    client = client_for(store)
    sd = list(parse(client.get(SPARQL_PATH).content, format=RdfFormat.TURTLE,
                    base_iri="http://testserver/"))
    in_sd = {q.object.value for q in sd if q.predicate.value == f"{SCHEMA}location"}
    _, void = _graph(client)
    in_void = {q.object.value for q in void if q.predicate.value == f"{SCHEMA}location"}
    assert in_sd == in_void == {MAASTRICHT}, (in_sd, in_void)


def test_the_location_does_not_make_this_an_endpoint_holding_geometry(client_for, store):
    """`geo-data` asks the STORE for geo:asWKT, and this service holds none.

    Publishing coordinates in a description is not holding geospatial data,
    and if these two were ever confused this service would start reporting
    `geo-data` about itself on the strength of its own address.
    """
    _, quads = _graph(client_for(store))
    assert not any("asWKT" in q.predicate.value for q in quads)
    rows = list(store.query(
        "SELECT ?g WHERE { { ?s <http://www.opengis.net/ont/geosparql#asWKT> ?g } "
        "UNION { GRAPH ?any { ?s <http://www.opengis.net/ont/geosparql#asWKT> ?g } } } LIMIT 1"
    ))
    assert rows == [], "the store now holds WKT geometry"
