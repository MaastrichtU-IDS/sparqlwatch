# sparqlwatch web

The Python side of sparqlwatch: it loads a prober run into an on-disk
Oxigraph store and holds the read queries against it. The Rust prober is
untouched by anything here; it keeps writing N-Quads files, and this side
starts where that file leaves off.

## The interpreter must be 3.12

Use `python3.12`, not the system `python3`.

On this machine the system `python3` is 3.9.6, which is past end of life.
pyoxigraph happens to be importable there too, which makes it tempting to
skip the venv, but this project exists partly because its predecessor
(umakadata) was rejected for running an entirely end-of-life stack. Building
a new subsystem on a dead interpreter would repeat exactly that mistake, so
the venv below is not optional scaffolding: it is the only sanctioned way to
run this code.

Before creating the venv, confirm the interpreter you are about to use is
3.12:

```bash
python3.12 --version
# Python 3.12.x, not 3.9.x
```

If `python3.12` is not on PATH, install one (for example via a Python
version manager or the official installer) before continuing. Do not
substitute `python3.11`: it has no pyoxigraph wheel verified for this
project, and `python3` (3.9) is explicitly out.

## Set up the venv

From the repository root:

```bash
python3.12 -m venv web/.venv
source web/.venv/bin/activate
pip install -r web/requirements.txt
```

`web/.venv/` is not committed (see the repository's `.gitignore`); recreate
it with the commands above whenever you check out a fresh clone.

Dependencies are pinned to exact versions in `web/requirements.txt`
(`pyoxigraph==0.5.9`, `pytest==9.1.1`), not compatible-release ranges. A
quality monitor whose own dependencies drift is not one anybody should
trust.

## Load a run into the store

The prober writes N-Quads files. Load them with `load_run()`:

```python
from pathlib import Path
from pyoxigraph import Store
from load_run import load_run

store = Store("path/to/sparqlwatch.db")
run_bytes = Path("path/to/run.nq").read_bytes()
result = load_run(store, run_bytes)
print(f"Loaded {result.quad_count} quads; replaced graphs: {result.replaced}")
```

`LoadResult` has exactly those two fields: `quad_count`, and `replaced`, the
list of graph IRIs that were already in the store and were dropped to make
room for this file's quads. An empty `replaced` means every graph the file
names was new.

The same thing from the command line, for one or more files at once:

```bash
web/.venv/bin/python web/load_run.py path/to/sparqlwatch.db path/to/run.nq
```

### The server has to be stopped first

An on-disk Oxigraph store is a RocksDB database and **only one process can hold
it open**. `app.py`'s `_opened_store` opens the store lazily, on the first
request the server serves, not at startup; from that request onward the
server process holds the store open for as long as it keeps running. A load
attempted before that first request can still succeed, but once the server
has served one request, the load fails:

```
OSError: IO error: While lock file: path/to/sparqlwatch.db/LOCK:
Resource temporarily unavailable
```

This applies to every load, not only the first one: **stop the server, load the
run, start the server again.** For a monitor whose whole point is repeated
sweeps that is the routine operation, not an exception, and a prober CronJob
writing runs on a schedule (spec stage 4) cannot load them into a store a
running server holds. Serving from a second store and swapping, or reading
through a process that does not hold the write lock, is a deployment question
this stage does not answer.

### What the load does and does not guarantee

`load_run()` **parses the entire file before touching the store**. A prober
crash writes a truncated N-Quads file; if the store were touched before parsing
failed, loading that file would erase a previous successful run. Parsing first
means a malformed file is rejected with the store intact.

That covers a bad file and nothing else. **The replacement is not atomic.**
Dropping the graphs and inserting the quads are two separate store operations,
because pyoxigraph 0.5.9's `Store` has no transaction API to group them in. An
insert that cannot complete (a full disk, an OOM kill, a power loss) leaves the
graphs dropped and the new quads not inserted.

The design can live with that window because **the store is a derived
artefact**. The prober's `.nq` files are the source of truth and a run graph is
immutable, so an interrupted load is a re-loadable state rather than lost data:
re-running `load_run()` with the same file restores the run exactly. What would
not be survivable is an interruption nobody notices, so after inserting,
`load_run()` counts the quads the store actually holds for the graphs it just
wrote and raises `RuntimeError` ("loaded 0 of 278 quads into [...]") when that
is not the number it parsed. The remedy is to re-run the load with the same
file.

## Runs replace rather than merge

`load_run()` takes the graph names from the quads it has just parsed (a file may
name more than one, and `run-two-sweeps.nq` does), **drops each of those graphs,
and then inserts**. It does not merge, and it has no notion of a run IRI of its
own: the graphs a file replaces are the graphs its own content names, which is
why `LoadResult.replaced` reports them after the fact rather than the caller
declaring them up front.

Replacing matters because the same run graph can be written twice with different
content. That happened during this project's development: an operator re-ran the
sweep with the same `--at` timestamp while one endpoint's DNS was flapping, so
the second file claimed the same run IRI and carried a different verdict for
that endpoint. Merging would leave one measurement carrying two `dqv:value`
triples (`verified` and `indeterminate`), which contradicts the immutability
that run-per-named-graph was built to guarantee. Replacing keeps a run graph
equal to the most recent file claiming it, never a blend of two.

Two different runs coexist in one store (they have different IRIs, separate
named graphs, and can be loaded one after another without collision). It is
re-loading a graph name the store already holds that triggers replacement.

## Query the store

The first read query is `endpoint_content()`, which answers "what is in this
endpoint?" (which classes does a recent sweep sample from it, how many, and is
the list complete).

```python
from pyoxigraph import Store
from endpoint_content import endpoint_content

store = Store("path/to/sparqlwatch.db")
r = endpoint_content(store, "https://data.kkg.kadaster.nl/query")
print(f"Endpoint holds {r.size} sampled classes")
print(f"Sample is truncated: {r.truncated}")
print(f"Run: {r.run}, generated at {r.generated_at}")
print(f"Classes: {r.classes}")
```

The query uses the **most recent run that sampled the requested endpoint**,
decided by comparing `prov:generatedAtTime` values (typed `xsd:dateTime`), not
by string-ordering the run IRI. This matters because today's run IRIs embed
ISO-8601 timestamps, so naive string ordering would agree; but the day that
shape changes, string ordering would start returning a stale run silently.

Real values from the test fixtures (a real sweep captured 2026-08-22T16:00:00Z):

- `https://data.kkg.kadaster.nl/query`: 59 classes, not truncated
- `https://ontop.certain.ai.ustp.at/sparql`: 50 classes, not truncated
- `https://qlever.dev/api/osm-planet`: not sampled. The sweep did probe it for
  classes: the same graph records that measurement as `dqv:value
  "indeterminate"` with `sw:elapsedMs "30003"`, the 30-second request budget
  running out. It was not a cost-ceiling decline, and this fixture contains no
  `sw:NotMeasured` resource at all, because it was captured with `--max-cost
  expensive`, where nothing is declined.

### What `sampled is False` means, and what it does not

It means one thing only: **no run in this store published a class sample for
that endpoint**. It does not mean nobody looked. At least three different
situations reach it: a sample the prober never attempted, a metric declined by
the cost ceiling (recorded as a `sw:NotMeasured` fact), and a probe that ran and
could not finish, which is what qlever above is. The graph distinguishes them,
because the measurement row for `sw:metric:classes` carries the verdict, but
**this query does not read that row and does not report which reason applied**.
A UI that renders `sampled=False` as "nobody has looked" would publish a
confident wrong answer about an endpoint this monitor measured for thirty
seconds.

## Run the tests

With the venv activated:

```bash
cd web
python -m pytest
```

All tests are offline: they read committed fixture files under
`web/tests/fixtures/` and never open a network connection. See
`web/tests/test_fixture.py` for what each fixture is and where it came from.

## Run the server

With the venv activated, first load a run into the store (see "Load a run into the
store" above; the server must be stopped while a run is loaded, every time). Then
set the store path and start the server:

```bash
cd web
export SPARQLWATCH_STORE="path/to/sparqlwatch.db"
python -m uvicorn app:app --reload
```

The server listens on `http://localhost:8000` by default. `--reload` watches for
Python changes and restarts the server; omit it for production.

`SPARQLWATCH_STORE` must name an existing directory with something in it. A path
that does not exist, or an empty one, is refused when the store is first opened
rather than created: `Store()` creates the RocksDB directory when it is missing,
so a typo used to produce a server whose every response was a 404 saying the
store held nothing about the endpoint. That was true of the empty store it had
just created and indistinguishable from a registry nobody has swept.

The single HTTP resource is the current state of a monitored endpoint:

```
GET /endpoint?url=<percent-encoded endpoint URL>
```

The endpoint URL must be percent-encoded. For example, `https://data.kkg.kadaster.nl/query`
becomes `https%3A%2F%2Fdata.kkg.kadaster.nl%2Fquery`:

```bash
curl 'http://localhost:8000/endpoint?url=https%3A%2F%2Fdata.kkg.kadaster.nl%2Fquery'
```

### Why a query parameter instead of a path

The thing being identified (a SPARQL endpoint URL) is itself a URL with slashes, so it
must travel either percent-encoded inside the path or as a query parameter. This service
uses the query parameter for two reasons:

1. **Path parameters are decoded twice.** A {url:path} parameter is unquoted by the ASGI
   server and again by the framework, so an endpoint URL containing a percent sequence of
   its own comes back as a different URL. A query string is unquoted exactly once, so the
   endpoint URL round-trips byte for byte.
2. **Encoded slashes are not reliably deliverable.** Apache's AllowEncodedSlashes defaults
   to Off (it answers 404), and nginx normalises path slashes by default. Behind such a
   front end, a path-based resource would stop resolving, while the query string is passed
   through untouched.

The cost: the resource IRI carries a query string, which is less pretty than a path and
cannot be extended by appending path segments. Correctness beats aesthetics, so the query
parameter is the right choice.

### Content negotiation

The same endpoint resource is served in two representations, chosen by the `Accept` header:

- `text/html` returns an HTML page with metrics, verdicts, and sampled classes
- `text/turtle`, `application/rdf+xml`, `application/n-triples`, `application/ld+json`
  return RDF serialised in that format. Dataset formats like N-Quads are not offered
  because the CONSTRUCT query yields triples, which a dataset format would serialise
  into a default graph, incorrectly implying that the run's named-graph structure was preserved.

The server honours quality values (`q=`). For example, `Accept: text/html;q=0.9, text/turtle`
prefers Turtle over HTML because Turtle has no explicit q-value (defaults to 1.0) while HTML
has q=0.9. `Accept: */*;q=1, text/html;q=0` returns Turtle, because a representation's q
comes from the most specific range that matches it rather than from the highest one: the
`text/html;q=0` is an explicit refusal of HTML, and reading the `*/*;q=1` as the winner
instead would turn that refusal into an offer.

A `q` value that does not match RFC 9110's `qvalue` grammar is ignored, so the range keeps
q=1.0: `text/html;q=abc`, `text/html;q=2` and `text/html;q=-1` all serve HTML. Clamping the
ones `float()` accepts would make `q=-1` an explicit refusal while `q=abc` was served, which
is two readings of two equally malformed headers. Two ranges of equal specificity matching one
representation, which RFC 9110 does not define, are resolved by taking the lower `q`, so
`text/html;q=1, text/html;q=0` and `text/html;q=0, text/html;q=1` agree, and both are 406.

A request with no `Accept` header defaults to HTML, and so does a header from which no media
range can be read at all (`*`, `garbage`, `,,,`): it expressed no preference. A header whose
ranges do parse and which names none we can serve returns 406, because that is a client saying
what it wants.

A request with no `url` parameter returns HTTP 400 with a plain-text body, not FastAPI's
default 422 JSON. It is the same class of client error as the malformed `url` below, and
negotiation runs first, so a client that can read none of these representations still gets
the 406 rather than a JSON validation body it did not ask for.

A `url` that cannot be an absolute IRI (an empty one, which is what a submitted-but-empty
form field sends, or one with a trailing space, which is what a copy-paste sends) returns
HTTP 400. The value is not trimmed or otherwise repaired: the endpoint IRI round-trips byte
for byte, and normalising it would make the response describe a different endpoint from the
one asked for.

An endpoint for which the store holds no measurement, no decline and no class sample returns
HTTP 404, and the body says exactly that rather than claiming the store holds no facts about
the endpoint at all. It can hold others: `endpoint_content.rq` asks only about
`sw:metric:classes`, so an endpoint known only by a sample from another metric 404s while the
store describes it (`tests/fixtures/run-properties-sample.nq` is that shape). A knownness test
that is not tied to one metric belongs to spec stage 2b, where properties sampling lands.

### Which sweep saw what

The two questions this resource answers are answered by two independent run selections:
`endpoint_measurements.rq` picks the newest run that measured or declined anything for the
endpoint, and `endpoint_content.rq` picks the newest run that published a class sample for
it. Those are different runs whenever the newest sweep declined `sw:metric:classes`, which
the prober does at its default cost ceiling, so it is the ordinary case rather than an odd
one.

Both facts are kept, and each is attributed to the sweep that observed it. When the two runs
are the same run, the page reads as one sweep's report and says nothing about a second. When
they differ:

- the header sentence covers the verdicts only, and names the measuring sweep's timestamp
- the class sample carries its own run and timestamp (`data-sample-run`,
  `data-sample-generated-at`) and a sentence beside it naming the sweep that took it
- a run that declined `sw:metric:classes` says so in the sample panel, so a reader is not left
  to assume the sample beside it is this sweep's
- the RDF emits both activities with their own `prov:generatedAtTime`, and links the sample to
  its own activity by `prov:wasGeneratedBy` where the store holds that link

Neither the page nor the RDF says which of the two sweeps is the later one. That is a
comparison of `xsd:dateTime` values, and both timestamps are printed so a reader or a
consumer can make it.

### When a run did not finish

The prober writes each endpoint's facts as that endpoint finishes, so the store can
hold a run that stopped halfway. Three facts on the run's activity are what let a
reader tell: `sw:emission "incremental"` says the run is written as a header, then one
chunk per endpoint, then a footer; `sw:completedEndpoint` names each endpoint the run
finished; `sw:finalised true` is the last line a finished run ever writes.

The page derives two separate statements from them, and they say different things:

- **the run whose facts are shown did not finish**: it carries `sw:emission` and no
  `sw:finalised`. Said as one sentence beside the timestamp the page already prints,
  because it is that timestamp being qualified. It does not claim anything above is
  missing: this endpoint's own chunk was written whole, which is why its facts are in
  the store at all.
- **a newer run exists, did not finish, and never recorded finishing this endpoint**.
  Nothing on such a page is stale: every verdict shown is the newest the store holds
  for the endpoint. What is true is that a later sweep died before it got here, and at
  548 endpoints that is every endpoint after the one it died on. Before the incremental
  write, such an endpoint carried `prober-failed` declines in the newest run and its
  rows read "not measured"; without this sentence the page would say nothing at all
  about the failed sweep, which is why one condition is not enough.

The second needs **the newest activity in the whole store**, so it is the one selection
in `endpoint_measurements.rq` and `endpoint_description.rq` that is not scoped to one
endpoint. Both queries say so in their headers, and both select it with the same
aggregate subquery: the "no run is newer" form of the same question cost 1.9 s and
2.2 s per request at 402 run graphs, against 217 ms and 106 ms for the subquery.

A run carrying **none** of the three facts is a run from before the incremental write.
It promised nothing, so its missing `sw:finalised` says nothing, and neither sentence
appears: such a page reads exactly as it did before this stage. Reading the absence
alone as a crash would stamp every historical run in the store as unfinished.

The RDF representation carries the **inputs** to both derivations and never their
result: the shown run's `sw:emission`, its `sw:finalised` where it has one, its
`sw:completedEndpoint` for this endpoint where it has one, and the same three for the
newest activity in the store. A derived "unfinished" flag is not emitted, because both
conditions rest partly on an absence and a CONSTRUCT emits only presence, so such a
flag could be built for the HTML and not for the RDF and the two would disagree about
exactly the run this exists for. A consumer draws the same two conclusions from the
same quads.

### RDF and HTML agreement

The HTML and RDF are two representations of one resource, derived independently:

- **HTML** comes from Python queries (`endpoint_measurements.py`, `endpoint_content.py`)
  that build a view model for the template to render
- **RDF** comes from a CONSTRUCT query (`queries/endpoint_description.rq`) serialised
  directly from the store without rebuilding in Python

Both come from the same store by two independent queries, neither re-derived from the other.
`test_the_two_representations_agree` asserts that the two renderings state the same verdict
for every one of the eight metrics its fixture measures, and that both state the current
run's values rather than merely agreeing with each other.

### Verdict encoding

Every verdict state (the state a metric is in: verified, undeclared-but-verified, etc.)
has exactly one definition: a border style, whether the swatch is filled, a border weight,
and a colour. That definition lives in exactly one place, `web/verdict_encoding.py`, and
both the HTML template and the test suite derive from it. The border, fill, and weight
are independent of colour so that a reader who cannot separate colours can still
distinguish every state. Colour is a redundant fourth channel. Nothing hard-codes any
of these properties.

The design artboards under `design/` predate `docs/design/verdict-encoding.md` and may
disagree with it. The artboards remain authoritative for layout, typography, spacing and
theme tokens; they are not authoritative for verdict encoding.

### What does not exist

The design spec lists four read paths and several write features. This slice delivers one.

**Not built:**
- Leaderboard (filterable, sortable by dimension)
- Per-metric pages (definition, computation method, which endpoints fail it)
- History (per-endpoint measurement history)
- Evidence (request, response headers, timing for each measurement)
- Faceted search
- Embedded query editor (`@sib-swiss/sparql-editor`)
- Read-only public SPARQL endpoint

All of these are in the spec under stages 3 onwards and remain future work.
