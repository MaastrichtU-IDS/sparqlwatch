"""A real SPARQL endpoint over the synthetic dataset, for probing against.

Run from web/:  .venv/bin/python ../tools/synthetic/serve.py [--declare MODE]

NOT one of the testenv fakes. Those return canned bodies and misbehave on a
schedule, which is what history tests need and what a COUNT cannot be checked
against. This is a real SPARQL 1.1 engine (pyoxigraph) over a dataset whose
counts are known exactly, so every number the prober publishes about it can be
compared with the truth rather than with another guess.

`--declare` is the point of the whole thing for the count metrics:

  correct  the description states the dataset's real counts       -> verified
  wrong    it states numbers that are confidently, plainly wrong  -> declared-but-wrong
  stale    it states counts about 3% out, inside a 5% tolerance   -> verified
  none     it states no counts at all                             -> undeclared-but-verified

So all four arms of the declared/observed axis can be produced on demand,
against an endpoint that cannot be wrong about itself by accident.
"""

from __future__ import annotations

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pyoxigraph as px
from dataset import EX, GRAPHS, SD, VOID, build

SPARQL_JSON = "application/sparql-results+json"
TURTLE = "text/turtle"


def description(truth, mode: str) -> str:
    """The service description served at the queryless GET.

    Carries `sd:endpoint` naming this service, so the prober's scoping rule has
    something to match and the counts below are read as OURS rather than
    through the single-service fallback.
    """
    if mode == "correct":
        triples, classes, entities = truth.triples, truth.classes, truth.entities
    elif mode == "stale":
        # Within a few percent: the ordinary case of a description written once
        # while the dataset kept growing. A monitor that calls this wrong is
        # crying wolf on almost every real endpoint.
        triples, classes, entities = (
            round(truth.triples * 0.97),
            truth.classes,
            truth.entities,
        )
    elif mode == "wrong":
        # Not a near miss. An order of magnitude out, so no tolerance can
        # excuse it and `declared-but-wrong` is the only honest verdict.
        triples, classes, entities = truth.triples * 10, truth.classes + 40, 1
    else:
        triples = classes = entities = None

    counts = ""
    if triples is not None:
        counts = (
            f"    void:triples {triples} ;\n"
            f"    void:classes {classes} ;\n"
            f"    void:entities {entities} ;\n"
        )

    graphs = " ,\n        ".join(f"<{g}>" for g in GRAPHS)

    # VoID partitions naming the vocabulary, which is the DECLARED half of the
    # explorer's axis. Deliberately incomplete: Person and Measurement are
    # declared and really present, Ghost is declared and absent, and Record,
    # Organisation and Dataset are present and undeclared. So one endpoint
    # produces three of the explorer's four states at once, and a UI that
    # renders them all the same is visibly wrong against it.
    partitions = ""
    if mode != "none":
        declared_classes = [EX + "Person", EX + "Measurement", EX + "Ghost"]
        declared_props = [EX + "name", EX + "value", EX + "neverUsed"]
        partitions = (
            "".join(
                f"    void:classPartition [ void:class <{c}> ] ;\n"
                for c in declared_classes
            )
            + "".join(
                f"    void:propertyPartition [ void:property <{p}> ] ;\n"
                for p in declared_props
            )
        )

    return f"""@prefix sd: <{SD}> .
@prefix void: <{VOID}> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

<{EX}service> a sd:Service ;
    sd:endpoint <http://127.0.0.1:9200/sparql> ;
    sd:supportedLanguage sd:SPARQL11Query ;
    sd:defaultDataset <{EX}dataset> ;
    sd:namedGraph
        {graphs} .

<{EX}dataset> a void:Dataset ;
{counts}{partitions}    rdfs:label "The synthetic sparqlwatch dataset" ;
    void:exampleResource <{EX}dataset/1> .
"""


class Handler(BaseHTTPRequestHandler):
    store: px.Store
    truth = None
    declare = "correct"

    def _cors(self) -> None:
        # A well-behaved endpoint: both metrics that ask about CORS should find
        # what they are looking for, so this endpoint is a positive control.
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Accept, Content-Type")

    def _body(self, code: int, payload: bytes, content_type: str) -> None:
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self._cors()
        self.end_headers()
        self.wfile.write(payload)

    def do_OPTIONS(self) -> None:
        self.send_response(204)
        self._cors()
        self.end_headers()

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        if parsed.path == "/truth":
            self._body(200, self.truth.as_json().encode(), "application/json")
            return
        if parsed.path != "/sparql":
            self._body(404, b"only /sparql is served here\n", "text/plain")
            return

        query = parse_qs(parsed.query).get("query", [None])[0]
        if query is None:
            # No query: this is the description fetch.
            payload = description(self.truth, self.declare).encode()
            self._body(200, payload, TURTLE)
            return

        try:
            result = self.store.query(query)
        except Exception as exc:  # noqa: BLE001 - a bad query is the client's
            self._body(400, f"{exc}\n".encode(), "text/plain")
            return

        if isinstance(result, px.QueryTriples):
            self._body(200, px.serialize(result, format=px.RdfFormat.TURTLE), TURTLE)
            return
        self._body(200, result.serialize(format=px.QueryResultsFormat.JSON), SPARQL_JSON)

    def log_message(self, *args) -> None:  # quiet by default
        if self.server.verbose:  # type: ignore[attr-defined]
            sys.stderr.write("  %s\n" % (args[0] % args[1:]))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9200)
    ap.add_argument(
        "--declare",
        choices=("correct", "stale", "wrong", "none"),
        default="correct",
        help="what the description claims about the dataset's size",
    )
    ap.add_argument("--verbose", action="store_true", help="log every request")
    args = ap.parse_args()

    store, truth = build()
    Handler.store, Handler.truth, Handler.declare = store, truth, args.declare

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.verbose = args.verbose  # type: ignore[attr-defined]
    print(f"synthetic endpoint on http://127.0.0.1:{args.port}/sparql")
    print(f"  declaring: {args.declare}")
    print(f"  truth: {truth.as_json()}")
    sys.stdout.flush()
    server.serve_forever()


if __name__ == "__main__":
    main()
