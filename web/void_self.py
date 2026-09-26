"""This service's own VoID description, served at `/.well-known/void`.

NOT void_document.py, and the difference is the whole point. That module
describes an endpoint we MEASURED -- its subject is our observation of
somebody else's dataset, and it exists because those operators published
nothing. This one describes the dataset we ourselves publish: the
measurements, the runs they came from, and the endpoint that answers
questions about them.

WHY `.well-known/void`. It is the discovery convention from the VoID note: a
consumer who has only a hostname can find the dataset description without
being told where to look. That is the same problem this project complains
about in others -- nine of nine catalogued endpoints publishing nothing a
consumer can read -- so not publishing one ourselves would be hard to defend.

IT DOES NOT CHANGE OUR OWN VERDICTS, and it should not be expected to. This
prober's `FetchWellKnown` probe is a queryless GET on the endpoint URL itself
and dereferences no well-known path -- the probe name is legacy, and
resolve.rs says so. So the five metrics that read `undeclared-but-verified`
about us still will. Making those read as declared means putting the same
claims in what `/sparql` returns to a queryless GET, which is a separate
change to the service description, not this file.

THE SUBJECT IS OUR OBSERVATIONS, never the endpoints observed. A reader must
not come away thinking this dataset contains Wikidata, or that its triple
count says anything about anyone's store but ours. The description says so in
its own first triples, for the same reason void_document.py does.

THE COUNTS ARE REAL, not declared-and-hoped. Each is a COUNT over the store
this process has open, measured on the deployed store at 0.1s to 1.9s, the
largest being 1.5M triples. They are cached for the life of the process
because the store cannot change under it: app.py opens it `read_only`, which
is a snapshot, and publishing new data is a restart. See _opened_store.
"""

from __future__ import annotations

from functools import lru_cache

from pyoxigraph import Store

from load_run import CURRENT_GRAPH_IRI

# The vocabularies a consumer will meet in this data. Named rather than
# derived: `void:vocabulary` is a statement about what the dataset uses, and
# deriving it from whatever happens to be in the store today would make it
# fluctuate with the fleet's behaviour rather than describe the schema.
VOCABULARIES = (
    "http://www.w3.org/ns/dqv#",
    "http://www.w3.org/ns/prov#",
    "http://rdfs.org/ns/void#",
    "urn:sparqlwatch:",
)

_COUNTS = {
    "triples": "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
    "distinctSubjects": "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
    "properties": "SELECT (COUNT(DISTINCT ?p) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
    "classes": "SELECT (COUNT(DISTINCT ?c) AS ?n) WHERE { GRAPH ?g { ?s a ?c } }",
    "graphs": "SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
    "current": (
        "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <" + CURRENT_GRAPH_IRI + "> { ?s ?p ?o } }"
    ),
}


@lru_cache(maxsize=4)
def counts(store: Store) -> dict[str, int]:
    """Every number in the description, measured once per process.

    KEYED ON THE STORE, not on nothing. A cache keyed on nothing is correct in
    production -- one store per process -- and wrong in every test, which is
    how a previous cache in this codebase passed its tests and would have
    served one store's numbers for another's.
    """
    out = {}
    for name, query in _COUNTS.items():
        rows = list(store.query(query))
        out[name] = int(rows[0]["n"].value) if rows else 0
    return out


def newest_run(store: Store) -> str | None:
    """The instant of the most recent run, for `dcterms:modified`.

    The dataset's modification date is when a sweep last added to it, which is
    a fact the store holds, rather than the moment this document was rendered.
    """
    rows = list(
        store.query(
            "SELECT (MAX(?at) AS ?newest) WHERE { GRAPH ?g { "
            "?a <http://www.w3.org/ns/prov#generatedAtTime> ?at } }"
        )
    )
    if not rows or rows[0]["newest"] is None:
        return None
    return rows[0]["newest"].value


# The dataset's own name and what it is. Kept to what is already written down
# rather than invented here: the description is the repository's own one-line
# summary, and the sentence after it is the honesty clause this project applies
# to every description it writes about somebody else.
TITLE = "sparqlwatch measurements"
DESCRIPTION = (
    "Quality monitoring for public SPARQL endpoints: scheduled probes, "
    "closed-vocabulary verdicts, and a site that shows what each endpoint "
    "actually answers. This dataset is what this service OBSERVED of those "
    "endpoints. It does not contain their data, and its counts describe these "
    "observations rather than any endpoint measured."
)
CURRENT_TITLE = "the newest reading for each endpoint and metric"

SOURCE = "https://github.com/MaastrichtU-IDS/sparqlwatch"

# THE DATA LICENCE, which is not the code licence and must not be confused
# with it. The software is Apache-2.0 (LICENSE); these measurements are CC BY
# 4.0 (LICENSE-DATA). Apache-2.0 is a software licence -- its terms speak of
# source and object form, of contributions and of patent grants, none of which
# map onto a set of observations -- and a catalogue that lists datasets looks
# for a data licence. This triple is what it looks for.
LICENSE = "https://creativecommons.org/licenses/by/4.0/"

# CC BY REQUIRES ATTRIBUTION, so a licence URI alone leaves a consumer unable
# to comply: they are told they must attribute and not told to whom. This says
# it in the document itself, which is the only place somebody working from the
# RDF will look.
#
# AN IDENTIFIER AND A NAME, not one or the other. The ORCID is what makes the
# creator a resource a consumer can resolve and reconcile against, rather than
# a string that has to be matched by spelling -- which is what a catalogue
# wants. But an identifier alone cannot be written into an attribution line, so
# the document also carries the name, on the ORCID itself, one hop away and in
# the same file. The checksum was verified before publishing it: ORCIDs carry
# an ISO 7064 MOD 11-2 check digit, and an identifier that fails it would point
# at nobody.
CREATOR_ID = "https://orcid.org/0000-0003-4727-9435"
CREATOR = "Michel Dumontier"


def _literal(text: str) -> str:
    """A Turtle string literal. Escaped, because a description is text."""
    escaped = (
        text.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
    )
    return f'"{escaped}"'


def document(store: Store, base: str, sparql_url: str) -> bytes:
    """The VoID description, as Turtle.

    `base` is this site's root as the request saw it, so the document names the
    host a reader actually reached rather than one baked in at build time.
    """
    n = counts(store)
    dataset = f"{base}#dataset"
    current = f"{base}#current"
    modified = newest_run(store)

    lines = [
        "@prefix void: <http://rdfs.org/ns/void#> .",
        "@prefix dcterms: <http://purl.org/dc/terms/> .",
        "@prefix foaf: <http://xmlns.com/foaf/0.1/> .",
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .",
        "@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .",
        "",
        f"<{base}.well-known/void> a void:DatasetDescription ;",
        f"    foaf:primaryTopic <{dataset}> ;",
        f"    dcterms:source <{SOURCE}> .",
        "",
        f"<{dataset}> a void:Dataset ;",
        f"    dcterms:title {_literal(TITLE)} ;",
        f"    dcterms:description {_literal(DESCRIPTION)} ;",
        f"    void:sparqlEndpoint <{sparql_url}> ;",
        f"    void:rootResource <{base}> ;",
        f"    dcterms:license <{LICENSE}> ;",
        f"    dcterms:creator <{CREATOR_ID}> ;",
        f"    dcterms:rightsHolder <{CREATOR_ID}> ;",
        # Every identifier this service mints lives under one URN scheme, which
        # is the one thing a consumer needs to tell our subjects from those of
        # the endpoints we describe.
        '    void:uriSpace "urn:sparqlwatch:" ;',
    ]
    lines += [f"    void:vocabulary <{v}> ;" for v in VOCABULARIES]
    lines += [
        f'    void:triples "{n["triples"]}"^^xsd:integer ;',
        f'    void:distinctSubjects "{n["distinctSubjects"]}"^^xsd:integer ;',
        f'    void:properties "{n["properties"]}"^^xsd:integer ;',
        f'    void:classes "{n["classes"]}"^^xsd:integer ;',
    ]
    if modified:
        lines.append(f'    dcterms:modified "{modified}"^^xsd:dateTime ;')
    lines += [
        f"    void:subset <{current}> .",
        "",
        # The default graph of the endpoint, declared because a consumer who
        # writes `?s ?p ?o` gets THIS and not the 1.5M triples above, and
        # nothing else on the open web would tell them why.
        f"<{current}> a void:Dataset ;",
        f"    dcterms:title {_literal(CURRENT_TITLE)} ;",
        f"    sd:name <{CURRENT_GRAPH_IRI}> ;",
        f'    void:triples "{n["current"]}"^^xsd:integer .',
        "",
        # The name the licence's attribution line needs, on the identifier the
        # catalogue wants. Neither alone is enough.
        f"<{CREATOR_ID}> a foaf:Person ;",
        f"    foaf:name {_literal(CREATOR)} .",
        "",
    ]
    return "\n".join(lines).encode("utf-8")


# The vocabulary this dataset actually uses, for the service description's
# `void:classPartition` and `void:propertyPartition`. Listed rather than
# counted, because that is what the predicates take and what makes
# `vocabulary-described` a comparison rather than a number.
#
# NO `ORDER BY`. Sorting these forces the engine to materialise every binding
# before deduplicating -- 1.5M of them -- and the endpoint's own memory watch
# stopped exactly that query at 320 MiB while this was being written. The
# result is sorted in Python instead, where it is 49 strings.
_VOCABULARY = {
    "classes": "SELECT DISTINCT ?c WHERE { GRAPH ?g { ?s a ?c } }",
    "properties": "SELECT DISTINCT ?p WHERE { GRAPH ?g { ?s ?p ?o } }",
}

# Verified against this build rather than assumed absent: pyoxigraph evaluates
# `geof:sfWithin`, returning true for a point inside a polygon, while a
# genuinely unknown function raises. So declaring it states something true.
# `geo-functions` read `undeclared-but-verified` about this service until it
# was declared here.
EXTENSION_FUNCTIONS = ("http://www.opengis.net/def/function/geosparql/sfWithin",)


@lru_cache(maxsize=4)
def vocabulary(store: Store) -> dict[str, tuple[str, ...]]:
    """The distinct classes and properties, sorted. Cached like `counts`."""
    out = {}
    for name, query in _VOCABULARY.items():
        var = "c" if name == "classes" else "p"
        out[name] = tuple(sorted(row[var].value for row in store.query(query)))
    return out
