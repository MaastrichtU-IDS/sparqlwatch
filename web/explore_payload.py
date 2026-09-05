"""Build the vocabulary explorer's payload from the store.

Replaces the static `explore_payload.json`, which was captured from a probe of
two endpoints on 2026-08-28 and could only ever describe those two. The route
served it because the store held no content samples: `classes` was declined at
the default cost ceiling on every sweep. The content-profile work changed that,
and this reads what those sweeps published.

WHAT MAKES A STATE. Each term carries one state per endpoint, from the pair of
answers the query returns:

    declared  observed   state
    yes       yes        verified
    no        yes        undeclared-but-verified
    yes       no         declared-only
    no        no         not in the payload at all

There is no `indeterminate` here and no `absent`. An endpoint whose content was
never profiled has no sample pointer, so the query returns nothing for it and it
is absent from the payload entirely, which is a different thing from a term
being absent from an endpoint that WAS profiled. The explorer offers
`indeterminate` as a filter because the vocabulary is shared with the index;
nothing produces it yet, and a chip that can only read 0 says something was
looked for. That is the next thing to fix here, not a defect to paper over.
"""

from __future__ import annotations

import json
import re
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlparse

from pyoxigraph import Store

_QUERY = (Path(__file__).resolve().parent / "queries" / "explore_vocabulary.rq").read_text()

# Namespaces belonging to a SPARQL engine rather than to the data its operator
# published. Hidden by default and counted separately, because "this endpoint
# uses 400 terms" is a claim about the publisher and half of them being the
# server's own bookkeeping makes it a false one.
#
# Matched on the HOST of the namespace IRI, not by substring, for the reason
# registry.rs gives about exclusions: `notopenlinksw.com` is a name somebody may
# legitimately publish from.
ENGINE_HOSTS = frozenset({
    "www.openlinksw.com",
    "openlinksw.com",
    "www.ontotext.com",
    "ontotext.com",
    "jena.apache.org",
    "rdf4j.org",
})

# Prefixes for namespaces a reader recognises on sight. Not exhaustive and not
# meant to be: an unknown namespace gets a generated prefix below, which is
# honest about being generated rather than inventing an authoritative one.
_WELL_KNOWN = {
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#": "rdf",
    "http://www.w3.org/2000/01/rdf-schema#": "rdfs",
    "http://www.w3.org/2002/07/owl#": "owl",
    "http://www.w3.org/2001/XMLSchema#": "xsd",
    "http://www.w3.org/2004/02/skos/core#": "skos",
    "http://xmlns.com/foaf/0.1/": "foaf",
    "http://purl.org/dc/terms/": "dcterms",
    "http://purl.org/dc/elements/1.1/": "dc",
    "http://rdfs.org/ns/void#": "void",
    "http://www.w3.org/ns/dcat#": "dcat",
    "http://www.w3.org/ns/prov#": "prov",
    "http://www.w3.org/ns/sparql-service-description#": "sd",
    "http://www.opengis.net/ont/geosparql#": "geo",
    "http://purl.obolibrary.org/obo/": "obo",
    "http://schema.org/": "schema",
    "https://schema.org/": "schema",
}


def split_iri(iri: str) -> tuple[str, str]:
    """An IRI as (namespace, local name).

    Splits at the last `#` or `/`, which is the convention every RDF serialiser
    uses to abbreviate. A term that has neither is its own namespace with an
    empty local name rather than an error: it is a real IRI and dropping it
    would silently shrink an endpoint's vocabulary.
    """
    for sep in ("#", "/"):
        i = iri.rfind(sep)
        if i != -1 and i + 1 < len(iri):
            return iri[: i + 1], iri[i + 1 :]
    return iri, ""


def _prefix_for(ns: str, taken: set[str]) -> str:
    """A short label for a namespace, generated when it is not well known.

    Generated prefixes are derived from the namespace's own last path segment,
    so `http://purl.org/ozo/onz-g#` reads as `onz-g` rather than as `ns7`. A
    collision takes a numeric suffix, because two namespaces sharing a label in
    a facet list would merge two vocabularies into one chip.
    """
    if ns in _WELL_KNOWN:
        return _WELL_KNOWN[ns]
    parsed = urlparse(ns.rstrip("#/"))
    # The last PATH segment first: `http://purl.org/ozo/onz-g#` reads as
    # `onz-g`, which is what its publisher calls it. Only when there is no path
    # does the host supply the label, and then its FIRST label rather than the
    # whole name: `https://synthetic.sparqlwatch.test/` is `synthetic`, not the
    # mangled `syntheticspa` that stripping dots out of the whole host produced.
    segment = parsed.path.rstrip("/").rsplit("/", 1)[-1]
    if not segment:
        segment = (parsed.hostname or ns).split(".")[0]
    stem = re.sub(r"[^a-zA-Z0-9-]+", "", segment)[:12]
    base = (stem or "ns").lower()
    candidate, n = base, 2
    while candidate in taken:
        candidate, n = f"{base}{n}", n + 1
    return candidate


def _is_engine(ns: str) -> bool:
    host = urlparse(ns).hostname
    return host is not None and host in ENGINE_HOSTS


def _short(url: str) -> str:
    """The endpoint label the explorer shows: host, plus enough path to tell
    two services on one host apart."""
    parsed = urlparse(url)
    host = parsed.hostname or url
    path = parsed.path.rstrip("/")
    return f"{host}{path}" if path and path not in ("/sparql", "/query") else host


@dataclass
class Term:
    iri: str
    kind: str
    at: dict[str, str] = field(default_factory=dict)


def build_payload(store: Store) -> dict:
    """The explorer's payload, read out of `store`.

    One pass over one query. The facet counts the explorer computes need every
    endpoint at once, so this cannot be assembled per endpoint.
    """
    # (endpoint, kind, term) -> [declared, observed]
    seen: dict[tuple[str, str, str], list[bool]] = defaultdict(lambda: [False, False])
    for row in store.query(_QUERY):
        key = (row["endpoint"].value, row["kind"].value, row["term"].value)
        if row["declared"].value == "true":
            seen[key][0] = True
        else:
            seen[key][1] = True

    terms: dict[tuple[str, str], Term] = {}
    endpoints: dict[str, dict[str, int]] = defaultdict(
        lambda: {
            "classes-used": 0,
            "classes-declared": 0,
            "properties-used": 0,
            "properties-declared": 0,
        }
    )
    for (endpoint, kind, iri), (declared, observed) in seen.items():
        state = (
            "verified"
            if declared and observed
            else "undeclared-but-verified"
            if observed
            else "declared-only"
        )
        terms.setdefault((kind, iri), Term(iri=iri, kind=kind)).at[endpoint] = state
        counts = endpoints[endpoint]
        if observed:
            counts[f"{kind}es-used" if kind == "class" else "properties-used"] += 1
        if declared:
            counts[
                f"{kind}es-declared" if kind == "class" else "properties-declared"
            ] += 1

    payload_terms = []
    namespaces: dict[str, int] = defaultdict(int)
    for term in terms.values():
        ns, local = split_iri(term.iri)
        namespaces[ns] += 1
        payload_terms.append(
            {
                "i": term.iri,
                "k": term.kind,
                "l": local or term.iri,
                "n": ns,
                "p": "",  # filled below, once every namespace is known
                "e": _is_engine(ns),
                "at": term.at,
            }
        )

    taken: set[str] = set()
    prefixes: dict[str, str] = {}
    # Busiest namespace first, so the most-seen vocabulary gets the unsuffixed
    # prefix when two would collide.
    for ns in sorted(namespaces, key=lambda n: (-namespaces[n], n)):
        prefixes[ns] = _prefix_for(ns, taken)
        taken.add(prefixes[ns])
    for entry in payload_terms:
        entry["p"] = prefixes[entry["n"]]

    # Sorted for a stable payload: an unstable order looks like the data
    # changed between two identical questions, and this file is diffed.
    payload_terms.sort(key=lambda t: (t["k"], t["i"]))
    return {
        "endpoints": [
            {
                "url": url,
                "short": _short(url),
                "questions": {
                    q: {"count": n, "capped": False, "verdict": None}
                    for q, n in sorted(counts.items())
                },
            }
            for url, counts in sorted(endpoints.items())
        ],
        "terms": payload_terms,
        "engineHosts": sorted(ENGINE_HOSTS),
        "vocabularies": [
            {"prefix": prefixes[ns], "ns": ns, "total": namespaces[ns]}
            for ns in sorted(namespaces, key=lambda n: (-namespaces[n], n))
        ],
    }


def build_payload_json(store: Store) -> str:
    return json.dumps(build_payload(store), sort_keys=False)
