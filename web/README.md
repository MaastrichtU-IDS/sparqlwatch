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
