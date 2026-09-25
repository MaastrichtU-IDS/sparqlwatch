"""The read-only SPARQL endpoint, and the four things standing in front of it.

THIS SERVICE MEASURES OTHER PEOPLE'S SPARQL ENDPOINTS. Publishing one of its
own is the obvious thing to do and also the most dangerous change in this
codebase, because every guard here is the only thing between a stranger's
query and a pod. Each one below was written against a behaviour that was
verified on this build, not assumed from documentation.

1. FEDERATION IS LIVE, AND IT IS AN SSRF HOLE. A query naming
   `SERVICE <http://169.254.169.254/...>` does not fail to parse: pyoxigraph
   0.5.9 opens the socket. Measured on 2026-09-25, the result was
   `TimeoutError: Connection timed out (os error 110)` -- a real outbound
   connection attempt to the cloud metadata address. Unmitigated, a public
   endpoint would let anyone make this pod fetch in-cluster Services, the
   Kubernetes API, or a metadata service.

   `reject_federation` below refuses those queries. It is NOT the security
   boundary and must not be mistaken for one: pyoxigraph exposes no parsed
   query object, so this reads query TEXT, and text checks lose to comments,
   case and whitespace eventually. The boundary is the NetworkPolicy denying
   this pod egress -- the site makes no outbound calls of its own, so denying
   it costs nothing. This function exists so a legitimate federated query gets
   an honest 400 instead of hanging until it times out.

2. THERE IS NO QUERY TIMEOUT. `Store.query()` takes no deadline and no result
   cap; a query runs until it finishes or until the OOM killer arrives.
   `stream` below consumes solutions with a deadline and a row cap, which works
   because Oxigraph yields lazily. Its limit is stated rather than hidden: a
   query that blocks INSIDE the engine before yielding its first solution --
   ORDER BY over a large join, say -- is not interrupted by this. That case is
   why the endpoint runs in its own container with its own memory limit, so the
   process that dies is the one serving queries and not the one serving pages.

3. QUERY ONLY, twice over. The store is opened `read_only`, and pyoxigraph's
   `query()` refuses update syntax outright (`INSERT DATA` raises SyntaxError
   there, verified). Neither is relied on alone.

4. CORS IS OPEN, DELIBERATELY. This service reports on other endpoints' CORS
   headers, and an endpoint that cannot be read from a browser is one this
   project would mark down. What is exposed is already public: the same facts
   these pages and their RDF representations serve.
"""

from __future__ import annotations

import re
import time
from dataclasses import dataclass

# Wall clock, per query. Generous for anything answerable and short enough that
# a pathological one does not hold a worker for long.
TIMEOUT_SECONDS = 30.0

# Rows returned before the endpoint stops and says so. A truncated answer that
# announces itself is honest; an OOM is not.
MAX_ROWS = 100_000

# Bytes of query text accepted. A megabyte of SPARQL is not a question.
MAX_QUERY_BYTES = 16_384

# SPARQL comments run from an unquoted # to end of line. Stripped before the
# federation check so a commented-out SERVICE does not trip it and, more to the
# point, so `SER#\nVICE`-style games have less room. The quote tracking keeps a
# # inside a literal -- common in IRIs and fragments -- from eating the rest of
# the line.
def strip_comments(query: str) -> str:
    out, quote, i = [], None, 0
    while i < len(query):
        c = query[i]
        if quote:
            out.append(c)
            if c == "\\" and i + 1 < len(query):
                out.append(query[i + 1])
                i += 2
                continue
            if c == quote:
                quote = None
        elif c in "'\"":
            quote = c
            out.append(c)
        elif c == "<":
            # An IRI: copied whole, because a # inside one is a fragment.
            end = query.find(">", i)
            if end == -1:
                out.append(c)
            else:
                out.append(query[i : end + 1])
                i = end + 1
                continue
        elif c == "#":
            while i < len(query) and query[i] != "\n":
                i += 1
            continue
        else:
            out.append(c)
        i += 1
    return "".join(out)


_FEDERATION = re.compile(r"\bSERVICE\b", re.IGNORECASE)

_LONG_QUOTES = ("'''", '"""')


def scannable(query: str) -> str | None:
    """`query` with literals and IRIs blanked out, or None if it cannot be read.

    Only keyword positions survive, so `?s ?p "a SERVICE outage"` and
    `<http://example.test/ns#SERVICE>` stop reading as federation. Both were
    refused before this existed, which is safe but wrong: an endpoint that
    rejects a legitimate query with a security message teaches people to
    distrust the message.

    None means the text could not be scanned -- an unterminated literal. The
    caller must treat that as a refusal. Returning the partial scan instead
    would let an unclosed quote swallow a SERVICE clause, turning a parse
    oddity into a way through.
    """
    out: list[str] = []
    i = 0
    while i < len(query):
        c = query[i]
        if c in "'\"":
            # Long forms first: ''' and \"\"\" may contain the short delimiter.
            head = query[i : i + 3]
            quote = head if head in _LONG_QUOTES else c
            j = i + len(quote)
            closed = False
            while j < len(query):
                if query[j] == "\\":
                    j += 2
                    continue
                if query.startswith(quote, j):
                    closed = True
                    break
                j += 1
            if not closed:
                return None
            out.append(" ")
            i = j + len(quote)
        elif c == "<":
            end = query.find(">", i)
            body = query[i + 1 : end] if end != -1 else ""
            # A bare `<` is the comparison operator, not an IRI, when what
            # follows holds whitespace or another `<` before any `>`.
            if end == -1 or any(ch.isspace() or ch == "<" for ch in body):
                out.append(" ")
                i += 1
            else:
                out.append(" ")
                i = end + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def reject_federation(query: str) -> str | None:
    """The refusal message, or None if the query names no SERVICE."""
    text = scannable(strip_comments(query))
    if text is None or _FEDERATION.search(text):
        return (
            "SERVICE is not available on this endpoint: it would let a query "
            "make this server fetch a URL of the query's choosing."
        )
    return None


@dataclass
class Outcome:
    """What a query produced, and whether it was allowed to finish."""

    rows: list
    truncated: bool = False
    timed_out: bool = False

    @property
    def complete(self) -> bool:
        return not (self.truncated or self.timed_out)


def stream(solutions, deadline: float | None = None, max_rows: int | None = None) -> Outcome:
    """Pull solutions until they run out, the cap is hit, or time is up.

    The deadline is checked per row rather than per batch: the point is to stop
    a query that keeps producing, and a query that keeps producing gives us a
    row to check on.
    """
    # Both limits are read at CALL time, not bound as default arguments. A
    # default argument freezes the module constant at import, so raising or
    # lowering either afterwards -- a test, or a future config knob -- would
    # silently do nothing.
    deadline = time.monotonic() + TIMEOUT_SECONDS if deadline is None else deadline
    max_rows = MAX_ROWS if max_rows is None else max_rows
    rows = []
    for solution in solutions:
        if len(rows) >= max_rows:
            return Outcome(rows, truncated=True)
        if time.monotonic() > deadline:
            return Outcome(rows, timed_out=True)
        rows.append(solution)
    return Outcome(rows)


# ---------------------------------------------------------------------------
# Serialising a bounded result
#
# pyoxigraph will serialise a QuerySolutions itself, but only the live
# iterator, and `stream` above has already consumed that to enforce the
# deadline and the cap. So SELECT and ASK are written here, against the SPARQL
# 1.1 Results JSON shape, and graph results go back to pyoxigraph, which is the
# authority on RDF syntax and should stay so.
# ---------------------------------------------------------------------------
import json as _json

from pyoxigraph import (
    BlankNode,
    Literal,
    NamedNode,
    Quad,
    QueryBoolean,
    RdfFormat,
    Triple,
)
from pyoxigraph import serialize as _serialize

SELECT_JSON = "application/sparql-results+json"

# Named in `head.link` when a result was cut short. A URN rather than a URL
# because this module does not know the hostname it is served under, and a
# link that is wrong is worse than one that is merely opaque.
_LIMIT_NOTE_IRI = "urn:sparqlwatch:result-incomplete"

# What a browser's fetch() may read. This service reports on other endpoints'
# CORS headers; one that could not be read from a browser is one it would mark
# down, so the endpoint wears what it measures.
CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Allow-Headers": "Accept, Content-Type",
    "Access-Control-Max-Age": "86400",
}


def term_json(term) -> dict:
    """One RDF term in SPARQL Results JSON form."""
    if isinstance(term, NamedNode):
        return {"type": "uri", "value": term.value}
    if isinstance(term, BlankNode):
        return {"type": "bnode", "value": term.value}
    if isinstance(term, Literal):
        out = {"type": "literal", "value": term.value}
        if term.language:
            # `xml:lang` is the key the spec names, oddly, and consumers key on
            # it exactly; `lang` is a different thing that nothing reads.
            out["xml:lang"] = term.language
        elif term.datatype and term.datatype.value != (
            "http://www.w3.org/2001/XMLSchema#string"
        ):
            # xsd:string is the datatype of a plain literal and is left out by
            # the spec, so emitting it makes identical results compare unequal.
            out["datatype"] = term.datatype.value
        return out
    if isinstance(term, Triple):
        return {
            "type": "triple",
            "value": {
                "subject": term_json(term.subject),
                "predicate": term_json(term.predicate),
                "object": term_json(term.object),
            },
        }
    return {"type": "literal", "value": str(term)}


def select_json(variables, outcome: Outcome) -> bytes:
    """SELECT results, with any truncation stated in the document itself.

    A truncated answer that does not say so is a wrong answer. It is reported
    in `head.link` -- the one place the results format allows extra information
    without breaking a consumer that does not look for it.
    """
    names = [str(v)[1:] if str(v).startswith("?") else str(v) for v in variables]
    bindings = []
    for row in outcome.rows:
        binding = {}
        for name in names:
            try:
                term = row[name]
            except (KeyError, IndexError):
                term = None
            # An UNBOUND variable is omitted from the binding, not written as
            # null: a consumer tells bound-from-unbound by the key's absence.
            if term is not None:
                binding[name] = term_json(term)
        bindings.append(binding)
    head: dict = {"vars": names}
    if not outcome.complete:
        head["link"] = [_LIMIT_NOTE_IRI]
    return _json.dumps(
        {"head": head, "results": {"bindings": bindings}}, ensure_ascii=False
    ).encode("utf-8")


def ask_json(value: bool) -> bytes:
    return _json.dumps({"head": {}, "boolean": bool(value)}).encode("utf-8")


def graph_bytes(triples, media_type: str) -> bytes:
    """CONSTRUCT / DESCRIBE output, serialised by pyoxigraph.

    Given as Quads in the default graph: `serialize` writes quads, and a triple
    format drops the graph component, so this is the same triples either way.
    """
    fmt = RdfFormat.from_media_type(media_type) or RdfFormat.N_TRIPLES
    quads = [Quad(t.subject, t.predicate, t.object) for t in triples]
    return _serialize(quads, format=fmt)


@dataclass
class Answer:
    """A finished query: what to send, as what, with which status."""

    body: bytes
    media_type: str
    status: int = 200
    complete: bool = True


def execute(store, query: str, default_graph, accept: str = "") -> Answer:
    """Run `query` under every guard in this module.

    The order is deliberate: refuse on size, then on federation, then run.
    Nothing reaches the engine until it has passed both, because the cheapest
    refusal is the one that never allocates.

    `default_graph` is this service's answer to a question the SPARQL spec
    leaves open. Every fact here lives in a named graph -- one per sweep -- so
    a spec-default empty default graph makes `?s ?p ?o` return nothing, which
    is correct and useless. The union of every run is the other obvious choice
    and is 1.4M triples of history. The default graph is `current` instead:
    one sweep's worth of "what is true now", the same view the pages render,
    bounded and fast. History is exactly where it was -- `GRAPH ?g { ... }`
    reaches every run, and the named graphs are unaffected by this.
    """
    if len(query.encode("utf-8")) > MAX_QUERY_BYTES:
        return Answer(
            b"query too long\n", "text/plain; charset=utf-8", 413, complete=False
        )
    refusal = reject_federation(query)
    if refusal:
        return Answer(
            refusal.encode("utf-8") + b"\n", "text/plain; charset=utf-8", 400,
            complete=False,
        )
    try:
        result = store.query(query, default_graph=default_graph)
    except (SyntaxError, ValueError) as exc:
        # The parser's own words. An endpoint that says only "bad query" makes
        # the author guess, and this one is read by people writing SPARQL by
        # hand against a schema they are still learning.
        return Answer(
            f"{exc}\n".encode("utf-8"), "text/plain; charset=utf-8", 400, complete=False
        )

    # ASK yields a QueryBoolean, not a Python bool -- it is truthy but it is
    # not an instance of bool, so an isinstance check against bool silently
    # falls through to the iteration branch and raises there.
    if isinstance(result, QueryBoolean):
        return Answer(ask_json(bool(result)), SELECT_JSON)
    if hasattr(result, "variables"):
        outcome = stream(result)
        return Answer(
            select_json(result.variables, outcome), SELECT_JSON, complete=outcome.complete
        )
    outcome = stream(result)
    media = "text/turtle" if "turtle" in accept else "application/n-triples"
    return Answer(graph_bytes(outcome.rows, media), media, complete=outcome.complete)
