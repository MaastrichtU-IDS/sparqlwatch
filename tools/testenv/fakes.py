"""Fake SPARQL endpoints that misbehave on a schedule, so history has shape.

The point of this file is to make VERDICT TRANSITIONS happen. A test environment
where every endpoint is healthy every day produces a history with nothing in it,
and the history display this exists to support would have nothing to draw.

Each fake listens on its own port, which matters: `politeness::host_key` strips
only :80 and :443, so 127.0.0.1:9001 and 127.0.0.1:9002 are different hosts and
the sweep probes them in parallel instead of serialising behind the 2 second
per-host gap.

Outages are 503, not hangs. A hanging socket costs the prober its full 30 second
request budget, and the budgets are compiled in rather than flags, so a single
hanging fake would add minutes to every sweep. A 5xx is `indeterminate` by the
same rule a real gateway error is, and it is instant.

Queries are answered for real, by pyoxigraph, against a small graph per fake. So
the verdicts this environment produces are the prober's actual judgments about
actual responses, not verdicts asserted by a mock. Where a fake cannot answer
something (geo functions, say) the failure is a real failure.
"""
import json, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pyoxigraph as ox

HERE = Path(__file__).resolve().parent
DAY_FILE = HERE / "run" / "day.txt"

EX = "http://example.org/"
BASE_TRIPLES = f"""
<{EX}alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person> .
<{EX}alice> <http://xmlns.com/foaf/0.1/name> "Alice" .
<{EX}bob>   <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person> .
<{EX}bob>   <http://xmlns.com/foaf/0.1/name> "Bob" .
<{EX}rome>  <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.opengis.net/ont/geosparql#Feature> .
"""

SERVICE_DESCRIPTION = """@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix void: <http://rdfs.org/ns/void#> .
<> a sd:Service ;
   sd:endpoint <{url}> ;
   sd:supportedLanguage sd:SPARQL11Query ;
   sd:defaultDataset [ a sd:Dataset ; sd:defaultGraph [ a sd:Graph ] ] .
"""

# port -> (name, behaviour(day) -> one of the BEHAVIOURS below)
#
# Ten days is the default run. Each lambda takes the 1-based day and returns how
# that fake behaves that day, so a transition is a change in what this returns.
FAKES = {
    9001: ("steady",       lambda d: "ok"),
    9002: ("flaky",        lambda d: "down" if 4 <= d <= 6 else "ok"),
    9003: ("gains-cors",   lambda d: "ok" if d >= 5 else "no-cors"),
    9004: ("loses-sd",     lambda d: "no-sd" if d >= 7 else "ok"),
    9005: ("html-console", lambda d: "html"),
    9006: ("garbage",      lambda d: "garbage"),
    9007: ("no-cors",      lambda d: "no-cors"),
    9008: ("newcomer",     lambda d: "down" if d <= 3 else "ok"),
}
BEHAVIOURS = {"ok", "down", "no-cors", "no-sd", "html", "garbage"}


def current_day() -> int:
    try:
        return int(DAY_FILE.read_text().strip())
    except (OSError, ValueError):
        return 1


def build_store() -> ox.Store:
    store = ox.Store()
    store.load(BASE_TRIPLES.encode(), format=ox.RdfFormat.N_TRIPLES)
    return store


STORE = build_store()


def results_json(res) -> bytes:
    # pyoxigraph 0.5.9 returns a QueryBoolean object, not a Python bool, so
    # `isinstance(res, bool)` is False for every ASK and the ASK path is never
    # taken. `availability` is a Liveness ASK, so getting this wrong makes every
    # fake look broken.
    if isinstance(res, ox.QueryBoolean):
        return json.dumps({"head": {}, "boolean": bool(res)}).encode()
    out = {"head": {"vars": [v.value for v in res.variables]},
           "results": {"bindings": []}}
    for sol in res:
        b = {}
        for v in res.variables:
            t = sol[v]
            if t is None:
                continue
            if isinstance(t, ox.NamedNode):
                b[v.value] = {"type": "uri", "value": t.value}
            elif isinstance(t, ox.BlankNode):
                b[v.value] = {"type": "bnode", "value": t.value}
            else:
                d = {"type": "literal", "value": t.value}
                if t.datatype and t.datatype.value != "http://www.w3.org/2001/XMLSchema#string":
                    d["datatype"] = t.datatype.value
                b[v.value] = d
        out["results"]["bindings"].append(b)
    return json.dumps(out).encode()


def make_handler(port: int, name: str, behaviour):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        # ---- helpers -------------------------------------------------------
        def _cors(self, mode: str):
            if mode == "no-cors":
                return
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "content-type, accept")

        def _send(self, code: int, ctype: str, body: bytes, mode: str):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self._cors(mode)
            self.end_headers()
            self.wfile.write(body)

        def _unavailable(self):
            body = b"upstream is not answering right now\n"
            self.send_response(503)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        # ---- verbs ---------------------------------------------------------
        def do_OPTIONS(self):
            mode = behaviour(current_day())
            if mode == "down":
                return self._unavailable()
            # A preflight must answer with an ok status or a browser will not
            # send the real request. 204 with the headers is the healthy shape.
            self.send_response(204)
            self._cors(mode)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_GET(self):
            mode = behaviour(current_day())
            if mode == "down":
                return self._unavailable()
            u = urlparse(self.path)
            qs = parse_qs(u.query)
            if "query" not in qs:
                # No query: this is the service-description fetch.
                if mode == "no-sd":
                    return self._send(404, "text/plain", b"not found\n", mode)
                sd = SERVICE_DESCRIPTION.format(
                    url=f"http://127.0.0.1:{port}/sparql").encode()
                return self._send(200, "text/turtle", sd, mode)
            self._answer(qs["query"][0], mode)

        def do_POST(self):
            mode = behaviour(current_day())
            if mode == "down":
                return self._unavailable()
            n = int(self.headers.get("Content-Length", 0))
            raw = self.rfile.read(n).decode("utf-8", "replace")
            ctype = (self.headers.get("Content-Type") or "").split(";")[0].strip()
            q = raw if ctype == "application/sparql-query" else parse_qs(raw).get("query", [""])[0]
            self._answer(q, mode)

        def _answer(self, query: str, mode: str):
            if mode == "html":
                # The query-console case the prober calls out by name: a 200 with
                # a human page where results should be.
                body = b"<!doctype html><html><body><h1>Query console</h1></body></html>"
                return self._send(200, "text/html", body, mode)
            if mode == "garbage":
                return self._send(200, "application/sparql-results+json",
                                  b'{"head": {"vars": ["s"]}, "results": {"bind', mode)
            try:
                res = STORE.query(query)
                body = results_json(res)
            except Exception as e:
                # A real evaluation failure, reported the way a real endpoint
                # reports one. geo functions land here, which is honest: this
                # fake does not implement GeoSPARQL.
                return self._send(400, "text/plain", str(e).encode(), mode)
            self._send(200, "application/sparql-results+json", body, mode)

    return Handler


def main():
    (HERE / "run").mkdir(exist_ok=True)
    if not DAY_FILE.exists():
        DAY_FILE.write_text("1\n")
    servers = []
    for port, (name, behaviour) in FAKES.items():
        srv = ThreadingHTTPServer(("127.0.0.1", port), make_handler(port, name, behaviour))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        servers.append(srv)
        print(f"  {name:14} http://127.0.0.1:{port}/sparql", file=sys.stderr)
    print(f"{len(servers)} fakes listening", file=sys.stderr, flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
