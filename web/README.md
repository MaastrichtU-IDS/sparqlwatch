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

`LoadResult` reports what the load did. `quad_count` is how many quads it
parsed; `replaced` lists the graph IRIs that were already in the store and were
dropped to make room for this file's quads, so an empty `replaced` means every
graph the file names was new; `discarded_bytes` is how many trailing bytes were
cut off as an incomplete final section. The remaining four are about the derived
`urn:sparqlwatch:current` graph (below): `advanced` and `advanced_samples` name
the endpoints whose verdicts and whose class sample this load moved forward,
`kept_newer` names those left alone because `current` already pointed at a newer
run, and `drifted` names those whose pointer now names a run that no longer
mentions them.

The same thing from the command line, for one or more files at once:

```bash
web/.venv/bin/python web/load_run.py path/to/sparqlwatch.db path/to/run.nq
```

It prints one line per file, and two more when there is something to say. It
names the endpoints `current` already pointed at a newer run for, so an
out-of-order load cannot look like a publication; and it names the endpoints
that **drifted**, on stderr, and **exits 1**. Drift is the one thing a load can
report that the load cannot fix (see below), so it exits the way `--check` does
on the same condition, and a deploy step that reads the status hears about it.

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
so an insert that cannot complete (a full disk, an OOM kill, a power loss)
leaves the graphs dropped and the new quads not inserted.

An earlier version of this section said the reason was that pyoxigraph 0.5.9's
`Store` has no transaction API. **That was false.** `Store.update()` is
documented as transactional ("either the full operation succeeds, or nothing is
written to the database") and that holds across `;`-separated operations:
`DROP GRAPH <urn:g> ; DROP GRAPH <urn:missing>` raises on the second and leaves
`urn:g` in place. What pyoxigraph does not offer is an explicit transaction
**handle**, something that could group this module's own `remove_graph` and
`extend` calls. The real objection to writing the replacement as one update is
the size of the `INSERT DATA` body it would need: one run of the 543-endpoint
registry is 27,194 quads, and serialising them into SPARQL text to be re-parsed
is a different cost from handing parsed `Quad` objects to `extend`. So the
window stays, and is reported rather than hidden. The derived graph below does
use that transactionality, one update per endpoint.

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

## The derived `urn:sparqlwatch:current` graph

Beside the run graphs, `load_run()` maintains one more named graph, and all
three read queries read it instead of deciding for themselves which run is the
newest. The reason is measured, over one real 543-endpoint registry sweep
replayed under 1, 7 and 30 run IRIs with the run instant rewritten everywhere it
appears. **The store shape matters as much as the number of runs, so each table
below says which store it was taken on.**

Deciding recency at query time, over a store holding no class sample at all,
which is what a sweep at the default cost ceiling produces:

| history | run quads | `endpoint_measurements.rq` | `endpoint_content.rq` | `endpoint_description.rq` |
| --- | --- | --- | --- | --- |
| 1 run | 27,194 | 10.0 ms | 0.10 ms | 2.0 ms |
| 7 runs | 190,358 | 309.7 ms | 0.15 ms | 13.4 ms |
| 30 runs | 815,820 | 5,801.8 ms | 0.17 ms | 78.9 ms |

The content column there is a query matching nothing: the registry sweep
declined `sw:metric:classes` for all 543 endpoints, so those three figures
measure the absence of a sample rather than the cost of reading one. Replayed
with a class sample per endpoint per run, which is what an expensive ceiling
produces today and what spec stage 2b exists to produce more of, the same two
queries are:

| history | `endpoint_content.rq` | `endpoint_description.rq` |
| --- | --- | --- |
| 1 run | 1.2 ms | 3.3 ms |
| 7 runs | 115.6 ms | 200.0 ms |
| 30 runs (4,171,560 quads) | 6,488.5 ms | 11,704.3 ms |

So one month of daily sweeps made the endpoint page a 5.8 second load and its
RDF representation an 11.7 second one. Reading `current` instead, over those
same two 30-run stores:

| 30-run store | `endpoint_measurements.rq` | `endpoint_content.rq` | `endpoint_description.rq` |
| --- | --- | --- | --- |
| no samples (815,820 quads) | 0.79 ms | 0.18 ms | 1.52 ms |
| 200 classes per endpoint per run (4,171,560 quads) | 1.10 ms | 2.25 ms | 20.75 ms |

The query shape was not the mistake: per-endpoint recency has to be written as
`FILTER NOT EXISTS`, because a value substituted for `?endpoint` does not reach
inside a subquery's own projection. Deciding recency **at query time** was the
mistake, and it is now decided once per load.

**The property that matters is the shape, not the ratio.** `current` stays about
27,000 quads whether the store holds one run or thirty, because it is
`O(endpoints)` and not `O(history)`, so every one of those read paths is now flat
rather than growing. A ratio taken at 30 runs is only the ratio at 30 runs; the
flatness is what still holds at 300.

### What it costs, which is load time

Loading got slower, and that is where the whole cost of this went. Loading 30 runs
takes **86.5 s without samples and 173.3 s with them**, about 3 to 6 s per run,
which is what 543 atomic per-endpoint updates cost. Paying that once per sweep to
take 5.8 s off every page view is the right side of the trade, and it is a trade
rather than a free win: a deployment that loaded far more often than it served
would be on the wrong side of it. This one sweeps by hand and serves every page
from the result, so it is not.

### Why the earlier measurement looked reassuring

`endpoint_measurements.rq` and `endpoint_description.rq` carry an older
measurement of the same question, taken over the committed fixtures plus N
**activity-only** run graphs: 402 of them, then 1002. Whole requests came out at
217 ms and 106 ms there, which read as comfortable, and the 5.8 second page above
is the same query on a store of 30 graphs.

The reason the small number was not evidence is that **the cost tracks quad
count, not graph count.** 402 activity-only graphs are a few thousand quads; 30
real run graphs are 815,820, and 4,171,560 once every run carries a class sample.
Counting run graphs is counting the wrong thing, and it is why a measurement that
looked fine did not catch a 5.8 second page. Those older figures are not withdrawn:
they still say what they measured, and what they measured is the newest-run
aggregate discussed under "When a run did not finish" below, which is the one
selection `current` cannot absorb.

### What it holds

For each endpoint, in graph `urn:sparqlwatch:current`:

- `<endpoint> sw:currentRun <run>`, the newest run that recorded a measurement,
  a decline or a `sw:declarationsRead` for it
- `<endpoint> sw:currentSampleRun <run>`, the newest run that published a
  `sw:metric:classes` sample of it
- that endpoint's `sw:declarationsRead` quad, and every quad of every
  `dqv:QualityMeasurement` and every `sw:NotMeasured` about it, copied verbatim
  out of the run `sw:currentRun` names

and nothing else. In particular **no `rdf:type prov:Activity` and no
`prov:generatedAtTime`, ever**: all three read queries select their run as
`GRAPH ?run { ?a a prov:Activity ; prov:generatedAtTime ?t }` with no
restriction on which graph, so a `current` graph carrying a typed activity would
be a run to every one of them, and the newest by construction, and both readers
would raise "runs tied as most recent" on every page. No run-level fact either:
`sw:emission`, `sw:finalised` and `sw:completedEndpoint` belong to a run, and
the readers reach them in one hop through the pointer, so nothing in `current`
states anything a run graph does not.

**And no sample quads.** The class sample itself was deliberately not copied: only
its pointer moved, and `endpoint_content.rq` and `endpoint_description.rq` reach
the `sw:sampledValue` triples in one hop into the graph `sw:currentSampleRun`
names. The reason is that the index reads `current` for verdicts and never for
sample values, so copying several hundred values per endpoint would grow the graph
the index scans and buy nothing. Two consequences, both real:

- the reads do not scan history, because the hop is into a graph the pointer names
  rather than a search across graphs, but **the store still grows with history**,
  since every run keeps its own sample of every endpoint it sampled: 4,171,560
  quads at 30 sampled runs against 815,820 unsampled ones
- the RDF representation is the one read whose timing moves with the sample at all:
  20.75 ms on the 30-run sampled store against 1.52 ms on the 30-run unsampled one.
  That is the endpoint resource's most expensive read, and it is milliseconds where
  it was 11.7 seconds

**And not an input.** A run file naming `urn:sparqlwatch:current` as its graph is
refused before any store is opened. One hand-written line naming it used to wipe
the graph, insert that line's own triples into it, and report a clean load with
nothing drifted, because the drift check asks which pointers name a run that no
longer states their facts and an emptied graph holds no pointers to ask about.

**Two pointers, not one.** The newest run that measured an endpoint and the
newest run that sampled it are different runs the moment a cheap sweep declines
`sw:metric:classes`, and that is the steady state: the registry sweep declined
`classes` for all 543 endpoints. One pointer loses the class sample outright.

### How it is updated, and what it cannot follow

For every endpoint the incoming run mentions, `load_run()` replaces what
`current` holds for that endpoint in **one `store.update()`**, so an endpoint is
never half-updated. It refuses to advance only when the run `current` already
points at is **strictly newer**, so re-loading the same run IRI does refresh
(which is the documented recovery) while an out-of-order older run is still
refused. A **tie** between two different runs at the same instant is refused
rather than resolved by load order, naming both runs, because both readers
refuse such a store and deciding it silently would be a behaviour change.

Three things the rule cannot fix:

- a run graph that **shrinks**, which is the full sweep followed by the
  truncated file a crashed prober leaves under the same run IRI
- a run graph **dropped** wholesale, which is the whole reason the design keeps
  one graph per run
- the **finished/unfinished flip**, which needs nothing: the run-level facts are
  read through the pointer, so a footer arriving changes what the page says
  without a quad of `current` moving

The first two are detected, not left to a reader. An endpoint whose pointer
names a run whose graph no longer mentions it comes back in
`LoadResult.drifted`, which `load_run.py` prints and which makes the load exit
non-zero, and the fix is a rebuild.

`drifted` is computed inside a load, though, and dropping a run graph is not a
load. So the dropped case is caught a second time, where nothing has to be run
for it to be noticed: **the server refuses to open a store in which any pointer
names a run graph the store does not hold**, names an affected endpoint and the
count and the run, and names the rebuild. Answering "no run in this store has
measured this endpoint" out of a store whose older run graph holds every verdict
for it is the same wrong answer the two refusals beside it exist to prevent.

**So dropping a run graph is a two-step operation.** Drop it, then rebuild:

```bash
web/.venv/bin/python web/load_run.py --rebuild path/to/sparqlwatch.db
```

Nothing falls back at read time, deliberately. Recency is decided in one place,
which is what moving it into `current` bought, and a reader that fell back to an
older run would put a second derivation beside the one the three read queries
use. A rebuild derives `current` from the run graphs alone, so it points every
endpoint at the newest run that still measures it, which after a drop is the
newest survivor.

### Check and rebuild

```bash
web/.venv/bin/python web/load_run.py --check   path/to/sparqlwatch.db
web/.venv/bin/python web/load_run.py --rebuild path/to/sparqlwatch.db
```

`--check` recomputes, from the run graphs alone, which run each endpoint's facts
should come from and what those facts are, and names every endpoint that
disagrees with `current`. It exits non-zero when anything drifted. It is
deliberately a second derivation rather than a call into the writing path,
because a check that used the writer's own answer could only ever agree with it,
and it is deliberately the expensive shape: it does the per-endpoint recency scan
the read queries no longer do, paid by an operator running a check rather than by
a page load.

`--rebuild` derives `current` from the run graphs alone. **The algorithm:** walk
the run graphs once, oldest first, to learn the newest run that measured each
endpoint and the newest that sampled each one, drop `current`, then write each
endpoint with the same per-endpoint update the load path uses. That is
`O(run graphs)` queries plus one update per endpoint, not one per endpoint per
run, and it shares its writing code with the load path so the two cannot derive
different graphs. **Measured: 0.8 s to 3.2 s** over the 30-run stores above.

That is not the figure this stage was planned against. Deriving the whole graph in
one SPARQL update instead measures **43.5 s** over the 30-run store, because every
endpoint's recency is then decided by scanning the whole history, which is the 5.8
second page again, once per endpoint. The plan took that shape's 43.5 s for the
cost of a rebuild and concluded a rebuild could not be the maintenance mechanism.
The conclusion still holds, for a smaller reason: an incremental update touches
only the endpoints one run mentions, while even a 3.2 s rebuild re-derives all 543.
So rebuild is a repair tool and a migration tool, and it is cheap enough to run
whenever an operator has a doubt.

### The migration: one rebuild pass

A store built before this graph existed holds run graphs and no `current`
graph, and every endpoint would then answer as though no sweep had ever
measured it. `app.py`'s `_opened_store` refuses to serve such a store and names
the `--rebuild` invocation, for the same reason it refuses a missing or empty
one: answering "nothing measured" out of a store that does hold the
measurements is the failure to prevent.

**Upgrading an existing store is one pass, and it is the rebuild:**

```bash
web/.venv/bin/python web/load_run.py --rebuild path/to/sparqlwatch.db
```

Stop the server first, the way every write to the store needs it stopped. Nothing
else has to change: the run graphs are untouched, the `.nq` files do not have to
be re-loaded, and the pass costs the 0.8 s to 3.2 s measured above rather than a
re-load's 86.5 s to 173.3 s. Run `--check` afterwards if you want a second
derivation to agree with it. From then on every load maintains `current` itself,
and a store that has never been through either is refused rather than served
wrong.

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
read from that endpoint's `sw:currentSampleRun` pointer in the derived
`urn:sparqlwatch:current` graph (above). The loader decides which run that is by
comparing `prov:generatedAtTime` values (typed `xsd:dateTime`), not by
string-ordering the run IRI. This matters because today's run IRIs embed
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

There are three HTTP resources. The index is every endpoint this service holds
facts for, and it is the way in:

```
GET /
```

the current state of one monitored endpoint:

```
GET /endpoint?url=<percent-encoded endpoint URL>
```

and the page the prober's `User-Agent` points at, which says who is querying a
stranger's endpoint and how to make it stop:

```
GET /about
```

The endpoint URL must be percent-encoded. For example, `https://data.kkg.kadaster.nl/query`
becomes `https%3A%2F%2Fdata.kkg.kadaster.nl%2Fquery`:

```bash
curl 'http://localhost:8000/endpoint?url=https%3A%2F%2Fdata.kkg.kadaster.nl%2Fquery'
```

### The index

`GET /` lists one row per endpoint the store holds facts for, with one chip per
metric, grouped by the availability verdict's own value. There is no pagination,
because the derived `urn:sparqlwatch:current` graph makes the whole registry a
flat scan: the 543-endpoint sweep is 4,344 rows in about 60 ms of query and
about 85 ms end to end, and neither number grows as sweeps accumulate.

The groups are the availability verdict's values, one per value present, in the
order of the table in `web/verdict_encoding.py`, followed by one group for
endpoints whose newest run recorded no availability verdict at all. They are not
"up" and "down", and there is no "did not answer" heading, because `absent` and
`indeterminate` are not one fact: `absent` means the host answered with
something that was not a SPARQL result (one of the four in the 543-endpoint
sweep is a `.ttl` file on raw.githubusercontent.com), while `indeterminate`
covers a timeout, a transport error, a DNS failure and an HTML front end.
Collapsing them would turn "we could not determine this" into a determined
negative.

Each chip reads two letters, the metric's abbreviation, and the metric key at
the top of the page writes every abbreviation out in full. The abbreviations are
generated from the metric names the store holds, so they change with the metric
set rather than being a list in the template. That is the page's size budget at
work: 543 rows carrying full metric names, or the metric repeated in an
attribute beside each chip, measured 610 KB against the 750 KB the page is
allowed. It is 437,404 bytes as it stands, which is 427.2 KiB and 437 KB. An earlier
sentence here said 424.6 KiB, which is 434,790 bytes and no measurement anyone
took.

**THE PAGE IS OVER THAT BUDGET AS SOON AS ROWS ARE MARKED, AND THE REMEDY IS
NOT DECIDED HERE.** Every row whose facts are not the newest run's carries the
instant of the sweep that measured it, in words and in `data-measured-by-sweep`,
and a row the newest sweep declined to ask carries about 144 bytes more. Measured
over the same 543 endpoints, in UTF-8 bytes of the served page, against the
decimal 750,000 the budget is written in. It was 500,000 until 2026-08-27, and the numbers below
are why it moved: at 808 bytes a row with no markers at all, 500,000 runs out at **619
endpoints**, so it was sized for the markup and not for a registry that only grows. The plan
that sets it records the change and the reasoning.

| Store shape | Rows marked | Dormant | Bytes | Per row | Against 750,000 |
| --- | --- | --- | --- | --- | --- |
| one sweep, the shape above | 0 | 0 | 438,652 | 808 | under by 311,348 |
| a two-endpoint newer sweep | 543 | 0 | 501,609 | 924 | under by 248,391 |
| the same, plus the 57 endpoints the measured set relegates | 543 | 57 | 510,188 | 940 | under by 239,812 |
| every row marked and dormant | 543 | 543 | 580,357 | 1,069 | under by 169,643 |

The last row is the worst case FOR THIS MARKER and the third is the realistic
near-term shape: a narrow re-probe plus the endpoints the cadence has taken. All
four are inside 750,000, and three of the four were outside 500,000, which is the
change. Two things the table does not say and a reader should know. **The page is
95% rows**: 552,913 bytes of rows against 27,444 for the head, the CSS, the
legend and all four explanation panels together, so per-row markup is the only
lever with leverage. And **the chips are 576 of a 1,014 byte row, 57%**, more
than half the row before this stage touched it, so the cheap reductions all live
outside them: dropping the visible instant saves 71 bytes a row, the
`data-measured-by-sweep` attribute 45, halving the marker text 37, about 184
bytes a row or 100 KB in total, none of it taken. What it is not is a decision
that was taken here:
`docs/superpowers/plans/2026-08-25-a-ui-you-can-use.md` sets the budget and says
an overrun comes back to the plan owner rather than being decided in a task, so
the markup stands as written and the number is with them. The alternative to the
marker is 543 rows of one sweep's verdicts under a header naming another sweep,
which is the same wrong answer 543 times, so the fix is not simply to delete it.
Roughly 11 KB of the marked figure is the instant's second spelling in
`data-measured-by-sweep`, which is the cheapest thing to give up if something has
to go. See "Rows whose facts are not the newest sweep's" below.

**The link to `/about` costs 1,227 bytes and not one byte per row.** Measured on
the served index of `store_registry_sample`, nine rows, against the same page
before it: 27,449 to 28,676 bytes, and the identical 1,227 on a store with no rows
at all, which is what makes it fixed. Almost all of it is the marked-row panel's
prose and the CSS comment beside the header rule; the `href` itself appears twice
per page, once in the header and once in that panel, and
`test_the_index_links_to_about_once_and_not_once_per_row` asserts the count. A
link in each row would have been the version of this fix that made the overrun
above worse by roughly 25 KB.

**The row filter is an inline `<script>`, and the page does not depend on it.**
Every row is in the document as served; with JavaScript off or blocked, all of
them are visible and only the filtering is missing. The spec's risk table flags
the `ids3` content security policy for a later stage's editor bundle, and this
script raises the same question one stage early: under a policy that forbids
inline script it will need a nonce or to become a static file, and nothing about
what the page says changes either way.

### `/about`, and how somebody asks to be left alone

`GET /about` is the page the prober's `User-Agent` points at: `client.rs` sends
`sparqlwatch/0.1.0 (+https://sparqlwatch.dev.k8s.semanticscience.org/about)` with
every request, so this page is where a sysadmin arrives after finding an
unfamiliar agent in their own log. It answers, in that order,
who is querying, how often, how politely, what "dormant" means if their endpoint
is marked that way, why this endpoint, and how to stop it.

**Both other pages link to it, and until this stage neither did.** The page
asserts that a reader arrives two ways, the second being a row on the index saying
the newest sweep did not ask their endpoint, and no template held an `href` to
`/about`: the marked row said what happened and stopped, so the contact address
that changes it and the four thresholds behind it were reachable only by pasting
the `User-Agent` URL into a browser. There are now four links: the header of the
index and of an endpoint page, the index's marked-row panel, and the endpoint
page's own dormancy sentence, the last two being the places a reader who followed
the mark is standing when the question becomes "who changes this".

**The dormancy section states each reason separately, because one description of
it was false of the others.** The section described the automatic case and
presented it as what the word means, which is wrong for `operator-hold` in three
ways: nothing was observed (`dormancy.rs` skips a `Dormant` hold before it sends
anything), the cadence does not apply (a held endpoint is asked by no sweep until a
person lifts it), and answering cannot end it (an endpoint never asked cannot
answer, and the promotion is suppressed regardless). The four `data-politeness`
numbers belong to the automatic case alone and the page now says so. It was the
last of four surfaces to distinguish the reasons, and the only real prober output
in this repository carries `operator-hold`.

**It takes no store dependency.** `about_resource` has no `store` parameter, and
that absence is deliberate: the one page a stranger needs is the one page that must
not be able to fail because a database is missing, locked by a load, or empty. It
negotiates through the same `choose_representation` as the other two resources, so
an operator's tooling can read the contact address and the request rate as RDF
rather than parsing English. The RDF representation mints its own predicates under
`urn:sparqlwatch:about:`, which is the opposite of the rule the other two follow
("a CONSTRUCT here may only emit triples some run graph already holds"), and the
difference is what the document is about: a triple about an endpoint must come from
a measurement, and there is no measurement of who we are.

**Every number on it comes from the prober's own source, and a test enforces it.**
`web/tests/test_about.py` reads `prober/src/politeness.rs`, `prober/src/main.rs`,
`prober/src/budget.rs`, `prober/metrics.toml`, `prober/registry/lod-cloud.toml`, its
provenance file, `prober/src/client.rs`, `prober/Cargo.toml` and, for the one measured
sweep duration, `prober/README.md`, and fails on a mismatch. So the page states 2 s minimum between requests to one host, 4 hosts in
flight, a 20 s cap on an honoured `Retry-After`, budgets of 30 s per request, 60 s
per metric and 600 s per endpoint, 7 requests per endpoint at the default cost
ceiling, 543 endpoints from one LOD Cloud dump, and one full sweep of 1h26m21s,
because those files say so. The politeness chain is pinned at both ends: one test
asserts the constants in `politeness.rs`, a second asserts that `main.rs`'s
`default_value_t` still names them, so the page cannot drift from either the
constant or the flag that uses it.

**How a request to be excluded is handled.** Email the address the page names (one
constant, `CONTACT_ADDRESS` in `app.py`), naming the host. A person then adds an
entry to `prober/registry/exclusions.toml`:

```toml
[[exclusion]]
host = "sparql.example.org"
reason = "A person asked, 2026-08-25"
```

Both fields are required, `host` is a bare host and not a URL, and a malformed or
unreadable list **fails the run** rather than being treated as empty. The entry is
effective **at the next sweep**: both binaries read the file from disk at every run,
so no rebuild and no redeployment stands in between, and a sweep already in flight
finishes under the list it started with. It is applied in `load_endpoints` and in
`seed::candidates`, so a re-seed of the registry from the public dump cannot put the
host back.

The limits are documented rather than implied, on the page and at length under
"Asking not to be probed" in `prober/README.md`: nothing watches the mailbox, so a
person has to make the edit; nothing already published is retracted, because a run
graph is written once and the endpoint URL is part of the identity of every fact
about it; one entry covers one whole host and nothing below it; no alias is resolved;
and the entry itself is public. The page also says the thing a sysadmin most needs
to hear, which is that blocking our requests does not stop them: a refusing endpoint
is recorded as unreachable and asked again on the next sweep, because nothing yet
drops an endpoint from the list for failing.

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

All three resources are served in two kinds of representation, chosen by the
`Accept` header and by the same code. The index's RDF representation is every
endpoint's measurements and declines, which is 2.2 MB of Turtle for the
543-endpoint sweep: a bulk representation, deliberately, and it carries no class
samples and no triple about the index itself, because a CONSTRUCT here may only
emit triples some run graph already holds.

The endpoint resource is served in two representations, chosen by the `Accept` header:

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
`endpoint_measurements.rq` reads the endpoint's `sw:currentRun`, the newest run that measured
or declined anything for it, and `endpoint_content.rq` reads its `sw:currentSampleRun`, the
newest run that published a class sample for it. Those are different runs whenever the newest
sweep declined `sw:metric:classes`, which the prober does at its default cost ceiling, so it
is the ordinary case rather than an odd one, and it is why there are two pointers rather than
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
endpoint. It is therefore the one selection the derived `current` graph cannot absorb:
`current` holds no typed activity and no `prov:generatedAtTime`, on purpose, so this
question is still asked of the run graphs. Both queries say so in their headers, and
both select it with the same aggregate subquery, rather than as `FILTER NOT EXISTS`
("no run is newer"), which compares every run against every other run once per row.

The figures behind that choice need reading with care, and they are the useful lesson
of this whole stage. They were taken over the committed fixtures plus 402 and then
1002 **activity-only** run graphs. At 402 graphs, whole requests came out at 217 ms
and 106 ms with the aggregate, against 1.9 s and 2.2 s with `FILTER NOT EXISTS`; at
1002 graphs the aggregate form was 1,288 ms and 682 ms, while the aggregate on its own
is 0.4 ms at 402 graphs and 1.0 ms at 1002, so finding the maximum is not what either
form costs. That comparison is still valid, and it is why the shape here stays an
aggregate. What those numbers were not is
evidence that the request was fast: **the cost tracks quad count, not graph count**,
and 1002 activity-only graphs hold a few thousand quads where 30 real run graphs hold
815,820. The same request, on 30 real runs, was the 5.8 second page the section on the
derived graph above measures and removes. A measurement over a store shaped nothing
like production looked reassuring for two stages and hid a page that took 5.8 seconds.

A run carrying **none** of the three facts makes no claim about finishing either way,
which is what a run from before the incremental write looks like. It promised nothing,
so its missing `sw:finalised` says nothing, and neither sentence appears: such a page
reads exactly as it did before this stage. Reading the absence alone as a crash would
stamp every historical run in the store as unfinished.

The bytes cannot establish that a run with none of the three facts really predates the
incremental write, because a run of the new format cut inside its header carries none
of them either. `load_run.py` separates the two on a fact it does hold: a run from
before this format still measured endpoints, so a file with no terminator and no
endpoint fact at all is refused rather than loaded. That refusal is what keeps a
truncated header out of this table: such a graph would carry the store's greatest
`prov:generatedAtTime`, win the newest-run subquery, and silence the second sentence
for every endpoint.

The RDF representation carries the **inputs** to both derivations and never their
result: the shown run's `sw:emission`, its `sw:finalised` where it has one, its
`sw:completedEndpoint` for this endpoint where it has one, and the same three for the
newest activity in the store. A derived "unfinished" flag is not emitted, because both
conditions rest partly on an absence and a CONSTRUCT emits only presence, so such a
flag could be built for the HTML and not for the RDF and the two would disagree about
exactly the run this exists for. A consumer draws the same two conclusions from the
same quads.

### Rows whose facts are not the newest sweep's

The index prints one timestamp in its header, the newest sweep in the store, and on
this site an absent qualifier is a positive claim. A row that says nothing therefore
says its verdicts were measured then. Four things a row can say, and each is a
different claim about a different run:

- **`from a sweep that stopped`.** This row's own run carries `sw:emission` and no
  `sw:finalised`. The endpoint's chunk was written whole, which is how its facts got
  here, so the row is what that sweep had written for it when it stopped.
- **`a later sweep never got here`.** A newer run stopped and recorded no
  `sw:completedEndpoint` for this endpoint. Nothing in the row is out of date: it is
  the newest the store holds. What is not true is that it reports the latest sweep.
- **`the newest sweep did not ask`**, with the reason the run published:
  `automatic` for the admission policy, `operator-hold` for a person,
  `not-in-this-sweep` for a sweep replaying an instant that had already run and so
  asking exactly the set that instant asked, the value verbatim for anything else,
  and "gave no reason" for a declaration with none. The three slugs are
  `SkipReason::slug`'s match arms, and `test_about.py` asserts set equality
  between them and both of `app.py`'s reason maps, so a slug the prober adds or
  renames fails here rather than degrading quietly into the verbatim branch while
  the prose goes on naming a value no run graph can carry. **What follows from the
  reason travels with the reason**: the cadence is inside the `automatic` gloss, a
  hold is asked by no sweep until a person lifts it, and `not-in-this-sweep`
  changes how often the endpoint is asked not at all. The panel used to close with
  one sentence covering all of them ("Either way the endpoint is asked at most one
  sweep in every 7 days"), which is true of the first and false of the other two.
  The **group note counts the reasons rather than assuming them**: it reads "and
  said why" only where every marked row in the group carries one, because
  `_row_dormancy_text` renders "and gave no reason" for a declaration with none and
  the two used to appear one above the other. Carried in
  `data-newest-sweep-dormant` and `data-dormancy-reason`. **Dormancy is
  not a verdict**: it gets no chip, no column and no entry in the legend, which
  counts states from the closed table in `verdict_encoding.py` and is shared with the
  endpoint page. The row keeps the verdict its last real probe produced and stays in
  that verdict's group; the group carries a note saying how many of its rows are
  marked, because moving them out would file a measured endpoint under a heading
  about this service's rotation.
- **`measured by the sweep at <instant>`**, in `data-measured-by-sweep`. The widest
  of the four: any row whose facts did not come from the newest run in the store, for
  any reason at all. The three above each say something further about why; a row can
  satisfy none of them and still be older, which is what `store_later_sample` is (its
  newest run sampled one endpoint and measured nothing, so all three of its rows are
  the earlier sweep's and none of the other markers fires).

**Worded against the sweep, never as an age from today.** No sweep here runs on a
timer, so "six days ago" is a claim the store cannot support: it holds two instants
and nothing about the gap expected between two of them. Both are printed and neither
is given as a distance from the other, which is the rule `_provenance` states for the
endpoint page's two timestamps.

All four are explained in full sentences in the page's own "What a marked row means"
panel, because a marker only a test can read qualifies nothing, and three words on a
row cannot carry the reasoning. The RDF representation of the index carries the inputs
and never the conclusions, for the reason the section above gives.

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

The design spec lists four read paths and several write features. This slice delivers
three resources: the index, the endpoint page, and `/about`.

**Not built:**
- Leaderboard (sortable by dimension; the index has one text filter and no sorts)
- Per-metric pages (definition, computation method, which endpoints fail it)
- History (per-endpoint measurement history)
- Evidence (request, response headers, timing for each measurement)
- Embedded query editor (`@sib-swiss/sparql-editor`)
- Read-only public SPARQL endpoint

**Blocked rather than merely unbuilt:** faceted search. Faceting endpoints by the
vocabularies and the classes they hold is the part that needs content data, and the
store holds no vocabulary or property data at all and one class sample per endpoint at
best, since `sw:metric:classes` is expensive and declined at the default cost ceiling.
The spec's stage 3 row already depends on stage 2b for exactly this reason, so these
facets wait on 2b producing something to facet on rather than on UI work. Grouping by
verdict does exist: the index groups by the availability verdict's own values.

### Known gaps in what this service tells a stranger

Both of these are gaps in the project rather than in a page, and both matter because
`/about` invites a reader to act on what it says. They are recorded here rather than
argued away, because a gap nobody wrote down is a gap the next reader has to
rediscover.

- **Nothing in this repository states who operates the service.** No person and no
  institution is named anywhere: `/about` carries one contact address, and the
  operator is whatever a reader infers from its domain. A page that asks a stranger
  to trust a request rate and a removal promise is a page that should say who is
  making the promise, and naming an institution is not a decision this stage took.
- **No repository URL exists anywhere in the project.** `/about` tells a reader that
  the exclusion list is "committed to this project's public source repository", and
  that is true and not actionable, because nothing on the page or in these files says
  where that repository is. The claim a reader is invited to check is the one claim
  they cannot currently reach.

Two smaller ones, stated on `/about` itself rather than here: nothing watches the
contact mailbox, and there is no channel for disputing a measurement.

The rest of the read tier is in the spec under stages 3 onwards and remains future
work.
