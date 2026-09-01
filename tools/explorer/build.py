"""Rebuild the vocabulary explorer from a preserved content run.

RECOVERED 2026-09-01. The original generator was written in /tmp during the
2026-08-28 content probe and was lost when /tmp was cleaned. What survived was
the generated HTML, in a directory outside git, with nothing able to rebuild it.
This file is the generator rewritten from that artifact and verified against it:
the payload it produces is byte-identical to the one the running page serves.

Input is `run-2026-08-28T21-07-34Z-content-probe-2.nq` in ~/code/sparqlwatch-runs,
which is checksummed and irreproducible: it is two live endpoints answering four
exploratory content questions, and re-running it would get different answers.

TWO HONESTY RULES decide what a term's chip may claim, and both were forced by
what that probe actually returned.

1. A capped sample cannot carry a negative claim. If the declared list was
   truncated at the sample limit, "used but not declared" is unsupportable: the
   declaration may sit past the cap.
2. A sample that is empty because the endpoint REFUSED the query is not evidence
   of absence either. dbpedia answered `properties-used` with `indeterminate` in
   44 ms, so its 200 declared properties cannot be called "declared, not
   confirmed".

Both collapse to `indeterminate`, the one state that means the measurement did
not decide. `verified` needs neither rule: two observations, no absence claimed.

The effect on dbpedia is the whole point of the page. Of 572 terms, 13 are
claimable and 559 read "not determined", and both negative-evidence chips sit at
zero. A page that showed a set difference there would be inventing one.
"""
import argparse, collections, json, re
from pathlib import Path

RUNS = Path.home() / "code" / "sparqlwatch-runs"
DEFAULT_RUN = RUNS / "run-2026-08-28T21-07-34Z-content-probe-2.nq"
HERE = Path(__file__).resolve().parent

# Virtuoso exposes its own internal schema through the same endpoint, so some
# terms describe the SERVER rather than the data it serves. Keyed on the vendor
# HOST, not a namespace list: keying on `.../schemas/virtrdf#` caught 97 and let
# two through (`.../schemas/VSPX#` and `.../ontology/acl#`). A vendor ships more
# than one internal namespace and will ship more later.
ENGINE_HOSTS = ("openlinksw.com",)

PREFIX = {
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#": "rdf",
    "http://www.w3.org/2000/01/rdf-schema#": "rdfs",
    "http://www.w3.org/2002/07/owl#": "owl",
    "http://www.w3.org/2004/02/skos/core#": "skos",
    "http://xmlns.com/foaf/0.1/": "foaf",
    "http://purl.org/dc/terms/": "dcterms",
    "http://purl.org/dc/elements/1.1/": "dc",
    "http://dbpedia.org/ontology/": "dbo",
    "http://dbpedia.org/property/": "dbp",
    "http://www.wikidata.org/entity/": "wd",
    "http://dati.camera.it/ocd/": "ocd",
    "http://www.openlinksw.com/schemas/virtrdf#": "virtrdf",
    "http://purl.org/goodrelations/v1#": "gr",
    "http://purl.org/ontology/bibo/": "bibo",
    "http://www.geonames.org/ontology#": "gn",
    "http://umbel.org/umbel/rc/": "umbel-rc",
    "http://linkedgeodata.org/ontology/": "lgdo",
    "http://www.ontologydesignpatterns.org/ont/dul/DUL.owl#": "dul",
    "http://schema.org/": "schema",
    "http://culturalis.org/oad#": "oad",
    "http://www.w3.org/ns/prov#": "prov",
    "http://rdfs.org/ns/void#": "void",
}
KINDQ = {"class": ("classes-used", "classes-declared"),
         "property": ("properties-used", "properties-declared")}


def is_engine(ns):
    host = ns.split("//", 1)[-1].split("/", 1)[0]
    return any(host == h or host.endswith("." + h) for h in ENGINE_HOSTS)


def split(iri):
    i = max(iri.rfind("#"), iri.rfind("/"))
    return (iri[:i + 1], iri[i + 1:]) if i > 0 else (iri, "")


def prefix_for(ns):
    if ns in PREFIX:
        return PREFIX[ns]
    body = ns.rstrip("#/").rsplit("/", 1)[-1].rsplit("#", 1)[-1]
    return body[:16] if body else ns


def parse_run(path):
    """Pull samples and verdicts out of the run's N-Quads."""
    ep, mt, vd, sfrom, sby, sval, strunc = {}, {}, {}, {}, {}, collections.defaultdict(list), {}
    for line in path.read_text().splitlines():
        m = re.match(r"<([^>]+)> <([^>]+)> (.*) <urn:sparqlwatch:run:", line)
        if not m:
            continue
        s, p, o = m.group(1), m.group(2), m.group(3).strip()
        if p.endswith("computedOn"):
            ep[s] = o.strip("<>")
        elif p.endswith("isMeasurementOf"):
            mt[s] = o.strip("<>").rsplit(":", 1)[-1]
        elif p.endswith("dqv#value"):
            g = re.search(r'"([^"]+)"', o)
            if g:
                vd[s] = g.group(1)
        elif p.endswith("sampledFrom"):
            sfrom[s] = o.strip("<>")
        elif p.endswith("sampledBy"):
            sby[s] = o.strip("<>").rsplit(":", 1)[-1]
        elif p.endswith("sampledValue"):
            sval[s].append(o.strip("<>"))
        elif p.endswith("sampleTruncated"):
            strunc[s] = "true" in o

    by_ep = collections.defaultdict(dict)
    for s, e in sfrom.items():
        if s in sby:
            by_ep[e][sby[s]] = {"values": sval.get(s, []), "capped": strunc.get(s, False)}
    # A question that produced NO sample still has a measurement, and its verdict
    # is what tells rule 2 from rule 1: refused is not the same as empty.
    for s, e in ep.items():
        if s in mt and mt[s] not in by_ep[e]:
            by_ep[e][mt[s]] = {"values": [], "capped": False, "verdict": vd.get(s)}
    return by_ep


def build(by_ep):
    def decisive(q):
        return (not q["capped"]) and q.get("verdict") is None

    support = {e: {q: decisive(v) for q, v in qs.items()} for e, qs in by_ep.items()}

    seen = {}
    for e, questions in by_ep.items():
        for question, data in questions.items():
            kind = "class" if question.startswith("classes") else "property"
            evidence = "declared" if question.endswith("declared") else "used"
            for iri in data["values"]:
                rec = seen.setdefault((iri, kind), {"iri": iri, "kind": kind, "at": {}})
                got = rec["at"].setdefault(e, {"used": False, "declared": False})
                got[evidence] = True

    terms = []
    for (iri, kind), rec in seen.items():
        used_q, decl_q = KINDQ[kind]
        at = {}
        for e, got in rec["at"].items():
            if got["used"] and got["declared"]:
                state = "verified"
            elif got["used"]:
                state = "undeclared-but-verified" if support[e].get(decl_q) else "indeterminate"
            else:
                state = "declared-only" if support[e].get(used_q) else "indeterminate"
            at[e] = state
        ns, local = split(iri)
        terms.append({"i": iri, "k": kind, "l": local or ns.rstrip("#/").rsplit("/", 1)[-1],
                      "n": ns, "p": prefix_for(ns), "e": is_engine(ns), "at": at})

    terms.sort(key=lambda t: (t["l"].lower(), t["i"]))
    vocab = collections.Counter((t["p"], t["n"]) for t in terms)
    return {
        "endpoints": [
            {"url": e, "short": e.split("//", 1)[-1].split("/", 1)[0],
             "questions": {q: {"count": len(d["values"]), "capped": d["capped"],
                               "verdict": d.get("verdict")} for q, d in qs.items()}}
            for e, qs in sorted(by_ep.items())
        ],
        "terms": terms,
        "engineHosts": list(ENGINE_HOSTS),
        "vocabularies": [{"prefix": p, "ns": n, "total": c}
                         for (p, n), c in sorted(vocab.items(), key=lambda kv: (-kv[1], kv[0][0]))],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", type=Path, default=DEFAULT_RUN)
    ap.add_argument("--out", type=Path, default=HERE / "explore.html")
    ap.add_argument("--payload-only", action="store_true")
    args = ap.parse_args()

    payload = json.dumps(build(parse_run(args.run)), separators=(",", ":"))
    if args.payload_only:
        print(payload)
        return
    template = (HERE / "template.html").read_text()
    assert "__PAYLOAD__" in template, "template lost its payload placeholder"
    assert "</script" not in payload
    args.out.write_text(template.replace("__PAYLOAD__", payload))
    print(f"wrote {args.out} ({args.out.stat().st_size / 1024:.0f} KB)")


if __name__ == "__main__":
    main()
