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
XSD_BOOLEAN = NamedNode("http://www.w3.org/2001/XMLSchema#boolean")

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

# The metric whose count tells us how many classes the endpoint holds, which is
# the only way to know the class list in this document is the whole list.
_CLASS_COUNT = "urn:sparqlwatch:metric:class-count"


def _provably_complete(described: int, reported: int | None, samplings: set[str]) -> bool:
    """Whether this document accounts for the whole endpoint, provably.

    PROVABLY, and the word is doing work. The question a consumer has is not
    "did we try hard" but "may I treat this as the endpoint's content". Four
    things have to hold, and every one of them is checkable from what the store
    already recorded rather than asserted by us:

      * the endpoint told us how many classes it has, so there is a total to
        check against. Without it the class list might be missing anything and
        nothing here would show it.
      * the document describes exactly that many. The profile pass enumerates
        with a LIMIT, so a truncated list is the ordinary outcome on a large
        endpoint and looks identical to a complete one from the inside.
      * every class was scanned exactly. A sampled class gives a property list
        that is sound and counts that are not the class's.
      * at least one class is described, because a document about nothing is
        not a complete account of something.

    Any of those failing makes it sampled, and the page says which. The default
    is therefore false: an endpoint that answers no count is not complete, it
    is unproven, and those have to read the same way here.
    """
    return (
        described > 0
        and reported is not None
        and described == reported
        and samplings == {_EXACT}
    )


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
    classes_reported: int | None = None

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
            if row["class"].value == _CLASS_COUNT:
                classes_reported = int(row["count"].value)
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

    # THE ONE THING A CONSUMER HAS TO READ FIRST, so it is stated rather than
    # left to be inferred from counting partitions and comparing samplings.
    complete = _provably_complete(
        len(partitions), classes_reported, set(sampling_of.values())
    )
    out.append(
        Triple(
            doc,
            NamedNode(SW + "provablyComplete"),
            Literal("true" if complete else "false", datatype=XSD_BOOLEAN),
        )
    )
    out.append(Triple(doc, NamedNode(SW + "classesDescribed"), _integer(str(len(partitions)))))
    if classes_reported is not None:
        out.append(Triple(doc, NamedNode(SW + "classesReported"), _integer(str(classes_reported))))
    return out


def void_summary(store: Store, endpoint: str) -> dict | None:
    """What the page says about the document, read out of the document.

    READ BACK RATHER THAN RECOMPUTED, which costs a second pass over rows that
    take hundredths of a second and buys the thing this codebase keeps paying
    for elsewhere: one implementation of "is this complete". A page that worked
    it out from the same store by its own arithmetic would be a second reader
    of the same question, and the two would eventually disagree in front of
    somebody deciding whether to trust the document.

    `None` when there is nothing to describe, which is what the route turns
    into a 404.
    """
    triples = void_triples(store, endpoint, "urn:sparqlwatch:void-summary")
    if not triples:
        return None
    read = {t.predicate.value: t.object.value for t in triples}
    described = int(read.get(SW + "classesDescribed", "0"))
    reported = read.get(SW + "classesReported")
    return {
        "complete": read.get(SW + "provablyComplete") == "true",
        "described": described,
        "reported": int(reported) if reported is not None else None,
        # Which sampling the profiles used, so the page can say "a sample of"
        # rather than only "not complete". Sorted for a stable rendering.
        "samplings": sorted({
            t.object.value for t in triples if t.predicate.value == SW + "sampling"
        }),
    }


# ---------------------------------------------------------------------------
# What the terms above mean, in words
# ---------------------------------------------------------------------------
# PUBLISHED VOCABULARY NEEDS A DEFINITION SOMEWHERE A READER CAN REACH. Every
# term here goes into a document a machine dereferences, under a `urn:` IRI
# that resolves to nothing, so without this a consumer meeting
# `sw:provablyComplete` has the word and no way to learn what licenses it.
#
# Keyed by the local name, so `test_every_published_term_is_documented` can
# compare this against the terms `void_triples` actually emits and fail when a
# term is added without one. That is the same guard `_DECLINE_DETAILS` has
# against prober/src/emit.rs, and it caught a missing sentence earlier today.
TERM_DOCS: dict[str, str] = {
    "provablyComplete": (
        "Whether this document accounts for the whole endpoint. True only when "
        "the endpoint stated how many classes it holds, this document "
        "describes exactly that many, and every one of them was scanned "
        "instance by instance. Any of those unmet makes it false, including "
        "the case where the endpoint would not answer a class count: unproven "
        "and incomplete are the same thing to act on."
    ),
    "classesDescribed": (
        "How many classes this document describes. Compared against "
        "classesReported to decide provablyComplete."
    ),
    "classesReported": (
        "How many classes the endpoint itself says it holds, when it answered "
        "that question. Absent when it did not, which is why a document "
        "without it can never be provably complete: there is no total to check "
        "against, and a truncated class list looks like a whole one."
    ),
    "sampling": (
        "How the instances behind one class partition were chosen. `exact` "
        "scanned every one. `sha256-prefix` kept those whose subject IRI "
        "hashes to a prefix, which is representative: measured within 0.002 of "
        "the true frequencies. `first-n` took the first instances the store "
        "returned, which is NOT representative -- the same measurement put it "
        "0.950 out -- and exists only for endpoints that refuse to aggregate "
        "at all, where the alternative is no profile."
    ),
    "sampledEntities": (
        "A count drawn from a sample rather than from every instance. It is "
        "true of the sample and does not generalise to the class. VoID's own "
        "void:entities is used instead wherever the scan was exact, so a "
        "consumer can tell the two apart by which predicate carries the number."
    ),
    "datatypeCount": (
        "How many distinct datatypes the objects of one property had, so a "
        "property with mixed datatypes is visible as mixed rather than reduced "
        "to whichever the store returned first."
    ),
    "namedGraphCount": (
        "How many named graphs the endpoint holds. In our vocabulary and not "
        "VoID's because void:Dataset has no term for it, and bending one that "
        "means something else would be worse than coining this."
    ),
}
