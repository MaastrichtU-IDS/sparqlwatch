"""A VoID description of an endpoint, derived from what we observed of it.

WHY THIS EXISTS. Nine of the nine endpoints the DBpedia KG catalogue declares
publish no service description this prober can read, and not one of the seven
that can be counted states its own size. A consumer who wants to know what is
in one of them, and a query editor that wants to autocomplete against it, have
nowhere to look. This is that description, written by us because nobody wrote
it there.

WHAT MAKES IT HONEST, and it is the whole design. The subject of the document
is OUR OBSERVATION of the endpoint and never the endpoint's own dataset. A
reader who dereferences it gets something that says, in its own first triples,
who looked and when.

And the counts are split across two vocabularies on purpose:

  * `void:` carries POPULATION claims. A `void:entities` is a statement about
    every instance of a class, and this module emits one only where the profile
    pass scanned every instance -- `profileSampling` of `exact`.
  * `sw:` carries SAMPLE observations. Where the pass sampled -- a hash prefix,
    or the bounded rung that exists for stores refusing aggregates -- the
    number is true of the sample and not of the class, so it is published as
    `sw:sampledEntities` beside the `sw:sampling` that produced it.

That split is not pedantry. `Sampling`'s own header records that a bounded
sample was wrong by 0.950 against 0.002 for a hash prefix, so a frequency
derived from one and published as `void:entities` would be a confident wrong
statement about somebody else's data, which is the failure this whole project
is arranged against.
"""

from __future__ import annotations

from pathlib import Path

from pyoxigraph import (
    BlankNode,
    Literal,
    NamedNode,
    Quad,
    Store,
    Triple,
    Variable,
)

_QUERY = (Path(__file__).resolve().parent / "queries" / "endpoint_void.rq").read_text()
_ENDPOINT = Variable("endpoint")

VOID = "http://rdfs.org/ns/void#"
DCT = "http://purl.org/dc/terms/"
PROV = "http://www.w3.org/ns/prov#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
SW = "urn:sparqlwatch:"
XSD_INTEGER = NamedNode("http://www.w3.org/2001/XMLSchema#integer")
XSD_DATETIME = NamedNode("http://www.w3.org/2001/XMLSchema#dateTime")

# The metric ids whose observed counts are population figures. Named here
# because they are the prober's, and a typo would silently emit no count rather
# than fail: web/tests pins them against prober/metrics.toml.
_POPULATION_COUNTS = {
    "urn:sparqlwatch:metric:triple-count": NamedNode(VOID + "triples"),
    "urn:sparqlwatch:metric:class-count": NamedNode(VOID + "classes"),
    # No VoID term states a number of NAMED GRAPHS -- `void:Dataset` has no
    # such property -- so this one is published in our own vocabulary rather
    # than bent into a term that means something else.
    "urn:sparqlwatch:metric:graph-count": NamedNode(SW + "namedGraphCount"),
}

# The one sampling that licenses a population claim.
_EXACT = "exact"


def _integer(value: str) -> Literal:
    return Literal(value, datatype=XSD_INTEGER)


def void_triples(store: Store, endpoint: str, document_iri: str) -> list[Triple]:
    """The description, as triples, or an empty list if nothing was observed.

    EMPTY RATHER THAN A STUB. A document asserting only "this is a description
    of X" says an endpoint was profiled and found to hold nothing, which is a
    claim about the endpoint. The caller turns an empty list into a 404, which
    says the only true thing: we have not profiled this.
    """
    rows = list(store.query(_QUERY, substitutions={_ENDPOINT: NamedNode(endpoint)}))
    if not rows:
        return []

    doc = NamedNode(document_iri)
    target = NamedNode(endpoint)
    out: list[Triple] = [
        Triple(doc, NamedNode(RDF + "type"), NamedNode(VOID + "Dataset")),
        # Said in the document rather than only in this file: a reader who
        # dereferences it and nothing else must still learn that these are
        # observations and whose.
        Triple(
            doc,
            NamedNode(RDFS + "comment"),
            Literal(
                "What sparqlwatch observed of this endpoint by probing it. "
                "Not the endpoint's own description of itself. Counts stated "
                "with void: terms were scanned exactly; counts stated with "
                "sw:sampledEntities were drawn from the sample named beside "
                "them and do not generalise to the whole class."
            ),
        ),
        Triple(doc, NamedNode(VOID + "sparqlEndpoint"), target),
        Triple(doc, NamedNode(PROV + "wasDerivedFrom"), target),
    ]

    # Class IRI -> the blank node standing for its partition, so the property
    # rows can hang off the class they were observed on rather than off the
    # dataset, which is what makes the document a partition rather than a bag.
    partitions: dict[str, BlankNode] = {}
    sampling_of: dict[str, str] = {}

    def partition(class_iri: str, sampling: str) -> BlankNode:
        node = partitions.get(class_iri)
        if node is None:
            node = BlankNode()
            partitions[class_iri] = node
            sampling_of[class_iri] = sampling
            out.append(Triple(doc, NamedNode(VOID + "classPartition"), node))
            out.append(Triple(node, NamedNode(VOID + "class"), NamedNode(class_iri)))
            out.append(Triple(node, NamedNode(SW + "sampling"), Literal(sampling)))
        return node

    for row in rows:
        kind = row["kind"].value
        if kind == "run":
            out.append(
                Triple(
                    doc,
                    NamedNode(PROV + "generatedAtTime"),
                    Literal(row["generatedAt"].value, datatype=XSD_DATETIME),
                )
            )
            out.append(Triple(doc, NamedNode(DCT + "source"), row["run"]))
        elif kind == "count":
            predicate = _POPULATION_COUNTS.get(row["class"].value)
            if predicate is not None:
                out.append(Triple(doc, predicate, _integer(row["count"].value)))
        elif kind == "class":
            sampling = row["sampling"].value
            node = partition(row["class"].value, sampling)
            # The population/sample split, at the one place it is decided.
            predicate = (
                NamedNode(VOID + "entities")
                if sampling == _EXACT
                else NamedNode(SW + "sampledEntities")
            )
            out.append(Triple(node, predicate, _integer(row["denominator"].value)))
        elif kind == "property":
            sampling = row["sampling"].value
            node = partition(row["class"].value, sampling)
            prop = BlankNode()
            out.append(Triple(node, NamedNode(VOID + "propertyPartition"), prop))
            out.append(
                Triple(prop, NamedNode(VOID + "property"), NamedNode(row["property"].value))
            )
            predicate = (
                NamedNode(VOID + "entities")
                if sampling == _EXACT
                else NamedNode(SW + "sampledEntities")
            )
            out.append(Triple(prop, predicate, _integer(row["subjects"].value)))
            out.append(
                Triple(prop, NamedNode(SW + "datatypeCount"), _integer(row["datatypes"].value))
            )
    return out
