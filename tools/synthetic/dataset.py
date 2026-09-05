"""The synthetic dataset, generated deterministically so its counts are known.

Every number this module reports is COMPUTED FROM THE STORE it just built,
never written down twice. A hand-maintained expected count is a second source
of truth that drifts the first time the generator changes, and the whole point
of a synthetic endpoint is that the right answer is not in doubt.

The shape is chosen to exercise the things the prober asks about:

  * triples in the DEFAULT graph and in NAMED graphs both, because a count that
    reads only one of the two is the specific wrong answer measured on
    2026-09-05: `SELECT (COUNT(*)) WHERE { ?s ?p ?o }` returned 1 against a
    store holding 4.
  * several classes with DIFFERENT property sets, so a class profile has
    something to distinguish.
  * properties whose objects are IRIs, properties whose objects carry one
    datatype, and one property carrying TWO datatypes, because the profile
    reports `datatypes` per property and a fixture where every property has
    exactly one cannot tell a working count from a hardcoded 1.
"""

from __future__ import annotations

import json
from dataclasses import dataclass

import pyoxigraph as px

EX = "https://synthetic.sparqlwatch.test/"
VOID = "http://rdfs.org/ns/void#"
SD = "http://www.w3.org/ns/sparql-service-description#"
XSD = "http://www.w3.org/2001/XMLSchema#"

# Three named graphs plus the default graph. Named after what they hold rather
# than g1/g2/g3, so a profile read by eye is legible.
GRAPHS = [EX + "graph/catalogue", EX + "graph/measurements", EX + "graph/people"]


@dataclass(frozen=True)
class GroundTruth:
    """What the endpoint really holds. Every field read back out of the store."""

    triples: int
    named_graphs: int
    classes: int
    entities: int

    def as_json(self) -> str:
        return json.dumps(self.__dict__, indent=2, sort_keys=True)


def _quads() -> list[str]:
    """The dataset as N-Quads lines, deterministic and hand-countable."""
    lines: list[str] = []

    def q(s: str, p: str, o: str, g: str | None = None) -> None:
        lines.append(f"<{s}> <{p}> {o} {f'<{g}> ' if g else ''}.")

    def iri(v: str) -> str:
        return f"<{v}>"

    def lit(v, dt: str | None = None) -> str:
        return f'"{v}"^^<{XSD}{dt}>' if dt else f'"{v}"'

    a = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
    label = "http://www.w3.org/2000/01/rdf-schema#label"

    # --- default graph: a small catalogue of Datasets -----------------------
    # In the DEFAULT graph on purpose. An endpoint whose every triple sits in a
    # named graph cannot tell a union-aware count from a graph-only one.
    for i in range(1, 4):
        s = f"{EX}dataset/{i}"
        q(s, a, iri(EX + "Dataset"))
        q(s, label, lit(f"Synthetic dataset {i}"))
        q(s, EX + "version", lit(i, "integer"))

    # --- catalogue graph: Records, each pointing at a Dataset ---------------
    for i in range(1, 6):
        s = f"{EX}record/{i}"
        g = GRAPHS[0]
        q(s, a, iri(EX + "Record"), g)
        q(s, EX + "inDataset", iri(f"{EX}dataset/{(i % 3) + 1}"), g)
        q(s, label, lit(f"Record {i}"), g)

    # --- measurements graph: the two-datatype case --------------------------
    # `EX + "value"` carries xsd:integer on some subjects and xsd:decimal on
    # others, so a profile of Measurement must report datatypes = 2 for it.
    for i in range(1, 5):
        s = f"{EX}measurement/{i}"
        g = GRAPHS[1]
        q(s, a, iri(EX + "Measurement"), g)
        if i % 2:
            q(s, EX + "value", lit(i * 10, "integer"), g)
        else:
            q(s, EX + "value", lit(f"{i}.5", "decimal"), g)
        q(s, EX + "unit", lit("mg"), g)

    # --- people graph: two classes, one of them tiny ------------------------
    for i in range(1, 4):
        s = f"{EX}person/{i}"
        g = GRAPHS[2]
        q(s, a, iri(EX + "Person"), g)
        q(s, EX + "name", lit(f"Person {i}"), g)
        q(s, EX + "affiliation", iri(f"{EX}org/1"), g)

    # One instance only, so a reader can tell a per-class count from a global
    # one at a glance.
    q(f"{EX}org/1", a, iri(EX + "Organisation"), GRAPHS[2])
    q(f"{EX}org/1", label, lit("Synthetic Institute"), GRAPHS[2])

    return lines


def build() -> tuple[px.Store, GroundTruth]:
    """An in-memory store holding the dataset, and its counts read back out."""
    store = px.Store()
    store.load("\n".join(_quads()).encode(), format=px.RdfFormat.N_QUADS)

    def scalar(query: str) -> int:
        body = json.loads(
            store.query(query).serialize(format=px.QueryResultsFormat.JSON)
        )
        return int(body["results"]["bindings"][0]["n"]["value"])

    # The UNION form on every one of these, for the reason the module docstring
    # gives: the default-graph-only form undercounts this very dataset.
    truth = GroundTruth(
        triples=scalar(
            "SELECT (COUNT(*) AS ?n) WHERE "
            "{ { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }"
        ),
        named_graphs=scalar(
            "SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"
        ),
        classes=scalar(
            "SELECT (COUNT(DISTINCT ?c) AS ?n) WHERE "
            "{ { ?s a ?c } UNION { GRAPH ?g { ?s a ?c } } }"
        ),
        entities=scalar(
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE "
            "{ { ?s a ?c } UNION { GRAPH ?g { ?s a ?c } } }"
        ),
    )
    return store, truth
