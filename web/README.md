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
print(f"Loaded {result.quad_count} quads into {result.run_iri}")
```

`load_run()` **parses the entire file before touching the store**, which is
critical for safety. A prober crash writes a truncated N-Quads file; if the
store were touched before parsing failed, loading that file would erase a
previous successful run. Parsing first means a malformed file is rejected with
the store intact.

## Runs replace rather than merge

When `load_run()` encounters a run IRI it has seen before, it **drops the entire
named graph for that run and reloads it**. It does not merge. This matters
because a run IRI is reused by re-runs: if an endpoint's DNS flapped and came
back during a sweep, the same run is re-probed at the same instant and the same
run IRI is issued. Merging would leave one measurement carrying two verdicts
(verified and indeterminate, for example), which contradicts the immutability
that run-per-named-graph was built to guarantee. Replacing instead keeps the
measurement sound.

Two different runs coexist in one store (they have different IRIs, separate
named graphs, and can be loaded one after another without collision). It is the
re-loading of a run IRI that triggers replacement.

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
- `https://qlever.dev/api/osm-planet`: not sampled (class enumeration exceeded the
  30-second request budget)

## Run the tests

With the venv activated:

```bash
cd web
python -m pytest
```

All tests are offline: they read committed fixture files under
`web/tests/fixtures/` and never open a network connection. See
`web/tests/test_fixture.py` for what each fixture is and where it came from.
