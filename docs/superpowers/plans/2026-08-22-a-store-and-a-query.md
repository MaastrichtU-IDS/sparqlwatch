# Stage 2-0: A Store, and the First Query That Answers a Task

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Get a run into Oxigraph and answer "what is in this endpoint" with a SPARQL query, so the UI has something to read.

**Architecture:** The prober keeps writing N-Quads to a file, which is what makes a run reproducible and crash-safe. A new Python side loads a run into an on-disk Oxigraph store and holds the read queries. No change to the prober.

**Tech Stack:** Python 3.12 in a virtualenv, pyoxigraph 0.5.9, pytest. The Rust prober is untouched.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`, which commits to Oxigraph as this service's own instance exposing a read-only public SPARQL endpoint, and a Python/FastAPI web tier whose read paths are all SPARQL-backed.

## Why this slice, and why it is small

The owner asked to work on the UI and picked "understand what is in it before
writing a query" as the first task. Stage 2b-1 made that answerable in principle:
a sweep now publishes the class IRIs it sampled. But nothing loads a run into a
store, so nothing can query it. The prober writes a file and the only viewer
parses that file directly.

That is the whole gap this slice closes, and it is smaller than it sounds because
the hard part already works. Everything below was verified before this plan was
written, not assumed:

- pyoxigraph 0.5.9 installs into a Python 3.12.13 venv, alongside pytest 9.1.1.
- An on-disk `Store` loads a real run (278 quads, 1 named graph) and reopens with
  the same contents.
- One SPARQL query already answers the chosen task against real data:
  `data.kkg.kadaster.nl/query` 59 sampled classes,
  `ontop.certain.ai.ustp.at/sparql` 50.

So this slice is mostly about doing that durably, idempotently and with tests,
rather than discovering whether it can be done.

## Design decisions, with their reasoning

**D1. Python 3.12 in a venv, not the system interpreter.** pyoxigraph is already
installed for the system `python3`, which is **3.9.6 and past end of life**. This
project rejected umakadata substantially because every component of its stack was
EOL, so starting a new subsystem on a dead interpreter would contradict the reason
this project exists. A venv on 3.12 costs one bootstrap step.

**D2. The loader REPLACES a run's named graph, it does not merge into it.** Not
for idempotency: RDF is a set, so loading identical content twice is already a
no-op, and I measured that (278 quads before and after a naive reload). The real
hazard is the same run IRI with **changed** content, which is not hypothetical:
during this project's own development the same `--at` was re-run several times
while one endpoint's DNS flapped. Merging those produces a graph where one
measurement carries two verdicts, which I demonstrated: after a naive merge of a
run against itself with a single verdict altered, exactly one measurement had two
`dqv:value`s.

A graph that asserts a measurement is both `verified` and `indeterminate` is worse
than either, and the run graph is supposed to be immutable.

**D2a. PARSE EVERYTHING BEFORE DESTROYING ANYTHING.** An earlier revision of this
plan said "drop the graphs named in the file, then load", and a review found that
**loses an entire run**. I verified it: with a real run in the store and a
truncated re-run file, dropping first and then loading leaves the store at **0
quads and 0 graphs**, because the load fails after the destruction. A truncated
`.nq` is not a contrived input either: it is exactly what a crashed or interrupted
prober produces, which is the whole reason stage 1c-b4 exists.

So the order is: parse the incoming bytes **completely** (`pyoxigraph.parse`, which
never touches the store), derive the graph names from the parsed quads, and only
then drop and insert. I verified the safe order: the same malformed file is
rejected before the store is touched, leaving all 278 quads and 1 graph intact,
and a subsequent good load still replaces cleanly.

Never destroy until the replacement is in hand. Say that in a comment, because
"drop then load" reads as the obvious implementation and is the wrong one.

**D3. Queries are files, not strings inside code.** This project's stated thesis
is that scoring is computed as queries over stored observations, so a query is a
first-class artefact: it lives in a file, it is version-controlled, and a test
asserts what it returns against a fixture run. Burying them in Python string
literals would make the thing the project is *about* the least reviewable part
of it.

**D4. The first query answers the owner's chosen task and nothing else.** Not a
leaderboard, not a fleet view. "Understand what is in it" needs, per endpoint: the
sampled class IRIs, how many, whether the sample was truncated, and which run it
came from. Anything more is speculation about a UI that does not exist yet.

## Global Constraints

- **The prober is not touched.** No `.rs` file, no `Cargo.toml`, no
  `metrics.toml`. `cargo test` must still report 271 passed, 0 failed, 2 ignored
  at the end, and that is a check, not a formality.
- Python 3.12, and code must run on it without warnings. Dependencies pinned to
  exact versions in a requirements file, because a monitor whose own stack drifts
  is not a monitor anybody should trust.
- **A query must not assert more than the data supports.** In particular a sample
  marked truncated must never be presented as a complete class list, and a metric
  that was declined must be distinguishable from one that found nothing. Both
  facts are already in the graph: `sw:sampleTruncated`, and the `sw:NotMeasured`
  resource.
- No em-dashes anywhere.
- Tests offline. No network in any test: the fixture is a committed run file.
- Prove every test by mutation, and verify each mutation applied **semantically**,
  not just textually. On this project a mutation once replaced an error's format
  string while leaving the interpolated argument in place, so it looked applied
  and was inert.
- `git status --porcelain` clean when you finish. **Do not use `git add -A`**:
  stage files by name.

## Starting state

`main` at the stage 2b-1 merge. 271 Rust tests pass, 2 ignored, clippy clean.
The repository has `design/`, `docs/`, `prober/`, `tools/` and no Python at all.

Facts you will need, verified:

- `python3.12` is on PATH at 3.12.13. `python3` is 3.9.6 and must not be used.
- `python3.11` exists but has no pyoxigraph; do not use it either.
- A real run with content samples is at
  `/private/tmp/claude-501/-Users-micheldumontier-code/e23f497b-40e0-4087-bab0-a34dc8cdfcf7/scratchpad/final-sample.nq`
  (278 quads, samples for two endpoints). Copy it into the repository as the test
  fixture rather than depending on a scratch path.
- `pyoxigraph.Store(path)` opens an on-disk store; `store.load(bytes, format=RdfFormat.N_QUADS)`
  loads; `store.named_graphs()` lists graphs; `store.remove_graph(g)` drops one.

---

## Task 1: The Python side exists and is reproducible

**Files:**
- Create: `web/requirements.txt`, `web/README.md`
- Create: `web/tests/fixtures/run-with-samples.nq` (copied from the real run above)
- Create: `web/tests/test_fixture.py`

**Interfaces:** none yet. This task makes the next two possible and provable.

- [ ] **Step 1: Pin the dependencies**

`web/requirements.txt` with exact versions: `pyoxigraph==0.5.9` and
`pytest==9.1.1`. Exact, not compatible-release: a quality monitor whose own
dependencies float is not one anybody should trust, and the spec's rejection of
umakadata cites its EOL stack as the decisive reason.

- [ ] **Step 2: Copy the fixture in, and build the two the real run cannot provide**

Copy the real run into `web/tests/fixtures/run-with-samples.nq` and commit it. It
is 278 quads and carries content samples for two endpoints. State its provenance
in a comment in the test file that uses it: which endpoints, which date, and that
it is a real sweep.

**Two more fixtures are needed, because the real run cannot exercise two cases the
query must get right.** A review found that without them, a wrong implementation
passes everything:

- `run-truncated.nq`: a sample with `sw:sampleTruncated true`. No endpoint in the
  registry holds more than 200 classes, so this state cannot be captured live.
  Hand-write a small one and mark it **synthetic** in a comment saying why. Note
  that the limit is not in the graph, only the size and the flag, so a small
  truncated sample is perfectly coherent.
  Without this fixture, an implementation that hardcodes `truncated = False`
  passes every other test in this plan.

- `run-two-sweeps.nq`: the **same endpoint** sampled in two runs with **different**
  values, so "the most recent run" is testable. Derive it from the real run by
  rewriting the run IRI and altering a value, and say so. Without it, a query that
  unions every run's samples passes every other test, which the review verified by
  building exactly that query.

- [ ] **Step 3: A test that proves the store works**

```python
def test_the_fixture_loads_and_reopens(tmp_path):
    """A store must persist. An in-memory store would pass every query test in
    this suite and be useless to a web tier that opens the store in a different
    process."""
    store = Store(str(tmp_path / "s"))
    store.load(FIXTURE.read_bytes(), format=RdfFormat.N_QUADS)
    loaded = len(store)
    assert loaded == 278, f"the fixture is 278 quads, got {loaded}"
    assert len(list(store.named_graphs())) == 1, "one run, one named graph"
    del store
    assert len(Store(str(tmp_path / "s"))) == loaded, "reopening must see the same quads"
```

Assert the exact quad count, not merely that it is non-zero: a fixture that
silently truncates would pass a non-zero check and quietly weaken every test
built on it.

- [ ] **Step 4: Write the README**

`web/README.md`: how to create the venv, that it must be 3.12 and why the system
3.9 will not do, how to install, and how to run the tests. Somebody returning in
six months should not have to rediscover the interpreter constraint.

- [ ] **Step 5: Commit**

```bash
git add web/requirements.txt web/README.md web/tests/fixtures/run-with-samples.nq web/tests/test_fixture.py
git commit -m "feat(web): a pinned Python side, and a real run as its fixture"
```

---

## Task 2: The loader, which replaces a run rather than merging it

**Files:**
- Create: `web/load_run.py`
- Create: `web/tests/test_load_run.py`

**Interfaces:**
- `load_run(store, nquads: bytes) -> LoadResult` where the result reports the
  graph IRIs it replaced and the quad count it loaded. A caller needs to know
  whether it replaced something, because replacing is the destructive case.
- A `__main__` entry point taking a store path and one or more `.nq` files, so a
  sweep can be loaded from the command line.

- [ ] **Step 1: Write the failing tests**

```python
def test_loading_the_same_run_twice_leaves_one_graph(tmp_path):
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    first = len(store)
    load_run(store, FIXTURE.read_bytes())
    assert len(store) == first
    assert len(list(store.named_graphs())) == 1

def test_a_changed_rerun_replaces_rather_than_merges(tmp_path):
    """The hazard this function exists to prevent. The same --at re-run after an
    endpoint's DNS recovers produces the same run IRI with different verdicts.
    Merging leaves one measurement carrying two of them, which is a graph that
    contradicts itself, and the run graph is meant to be immutable."""
    store = Store(str(tmp_path / "s"))
    original = FIXTURE.read_text()
    changed = original.replace('"verified"', '"indeterminate"', 1)
    load_run(store, original.encode())
    load_run(store, changed.encode())
    rows = list(store.query("""
        SELECT ?m (COUNT(DISTINCT ?v) AS ?n) WHERE {
          GRAPH ?g { ?m <http://www.w3.org/ns/dqv#value> ?v }
        } GROUP BY ?m HAVING (COUNT(DISTINCT ?v) > 1)"""))
    assert rows == [], f"no measurement may carry two verdicts, got {len(rows)}"

def test_the_result_says_what_it_replaced(tmp_path):
    store = Store(str(tmp_path / "s"))
    first = load_run(store, FIXTURE.read_bytes())
    assert first.replaced == [], "nothing was there to replace"
    second = load_run(store, FIXTURE.read_bytes())
    assert len(second.replaced) == 1, "the second load replaced the run's graph"

def test_two_different_runs_coexist(tmp_path):
    """Runs are per-sweep named graphs, so loading a second run must not disturb
    the first. Without this, 'replace' could be implemented as 'clear the store'
    and every test above would still pass."""
    store = Store(str(tmp_path / "s"))
    load_run(store, FIXTURE.read_bytes())
    other = FIXTURE.read_text().replace("2026-08-22T16:00:00Z", "2026-08-22T17:00:00Z")
    load_run(store, other.encode())
    assert len(list(store.named_graphs())) == 2
```

That last test matters: without it, "replace the graph" could be implemented as
"empty the store" and everything else would still pass.

- [ ] **Step 2: Run them and watch them fail**

- [ ] **Step 3: Implement**

Read the graph names out of the incoming quads, drop exactly those graphs, then
load. Do not clear the store, and do not assume one graph per file: the format
permits several and a future slice may batch runs.

Say in a comment why replacing is right, citing the two-verdict case rather than
duplication, since RDF set semantics already handle duplication and a reader who
thinks that is the reason will "simplify" this away.

- [ ] **Step 3a: The input shapes a review found untested**

Add a test for each, because each is reachable and none is covered above:

- **a malformed file** must leave an existing run untouched. This is the D2a case
  and the most important test in this task: I measured a real run going from 278
  quads to 0 under the naive order.
- **a file with no named graphs** (only default-graph triples). Decide what that
  means, state it, and test it. Refusing is defensible; silently loading into the
  default graph is not, since every run this project emits is a named graph and a
  file without one is a different thing than it claims to be.
- **a file with several named graphs** must replace all of them and nothing else.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each verified applied: load without dropping (the changed-rerun test
must fail); drop every graph rather than the named ones (the coexist test must
fail); report `replaced` as always empty (the result test must fail); drop before
parsing (the malformed-file test must fail, and this is the one that would have
shipped data loss). Restore between each.

- [ ] **Step 5: Commit**

```bash
git add web/load_run.py web/tests/test_load_run.py
git commit -m "feat(web): load a run by replacing its graph, never merging into it"
```

---

## Task 3: The query that answers the task

**Files:**
- Create: `web/queries/endpoint_content.rq`
- Create: `web/queries/__init__.py` or a small loader for query files, your choice
- Create: `web/tests/test_endpoint_content.py`

**Interfaces:**
- The query takes an endpoint IRI and returns, for the most recent run that
  sampled it: each sampled class IRI, the sample size, and whether it was
  truncated.
- However you parameterise it, a test must show that asking for one endpoint does
  not return another's values.

- [ ] **Step 1: Write the failing tests**

One correction to this plan's earlier narrative, found by review and worth knowing
before you write these: **the fixture contains no `sw:NotMeasured` resource at
all.** It was captured with `--max-cost expensive`, where nothing is declined, so
`not_measured` was zero. qlever's absence of a sample there is a 30 second request
timeout producing `dqv:value "indeterminate"`, not a cost-ceiling decline. The
test below is still the right test, but its reason is the timeout, not the ceiling.
Distinguishing a declined metric from a timed-out one is worth a second fixture in
a later slice; do not claim this one covers it.

Against the fixture, whose real values are known:

```python
def test_kadaster_content(store):
    r = endpoint_content(store, "https://data.kkg.kadaster.nl/query")
    assert r.size == 59
    assert r.truncated is False
    assert len(r.classes) == 59, "size must match the values actually returned"
    assert "http://www.w3.org/2002/07/owl#Class" in r.classes

def test_ontop_content_is_its_own(store):
    """Two endpoints in one graph. A query that ignores its parameter returns 109
    values and passes any test that only checks a count is plausible."""
    r = endpoint_content(store, "https://ontop.certain.ai.ustp.at/sparql")
    assert r.size == 50
    assert len(r.classes) == 50
    assert "https://w3id.org/aidoc-ap#AISystemCapability" in r.classes
    assert "http://www.w3.org/2002/07/owl#Class" not in r.classes, "that is kadaster's"

def test_an_endpoint_with_no_sample_is_not_an_empty_endpoint(store):
    """qlever is in the fixture with no sample, because its enumeration exceeded
    the request budget. The answer must be distinguishable from an endpoint that
    genuinely holds no classes: returning an empty list for both would publish a
    confident wrong answer in the UI."""
    r = endpoint_content(store, "https://qlever.dev/api/osm-planet")
    assert r.sampled is False
    assert r.classes == []
```

```python
def test_a_truncated_sample_is_reported_as_truncated(store_truncated):
    """An implementation that hardcodes truncated=False passes every other test
    here, because no real endpoint in the registry exceeds the 200 cap. A list a
    reader believes is complete when it is not is the failure the whole sampling
    design guards against, so it needs its own fixture."""
    r = endpoint_content(store_truncated, TRUNCATED_ENDPOINT)
    assert r.truncated is True

def test_only_the_most_recent_run_is_returned(store_two_sweeps):
    """The same endpoint sampled twice. A query that unions every run passes all
    the tests above and reports stale classes beside current ones, with no way for
    a reader to tell which is which. Assert the VALUES, not just the count: a
    union of two runs can coincidentally have a plausible size."""
    r = endpoint_content(store_two_sweeps, REPEATED_ENDPOINT)
    assert CURRENT_ONLY_CLASS in r.classes
    assert STALE_ONLY_CLASS not in r.classes, "a stale run's values must not appear"
```

The parameter test catches a query ignoring its argument; the not-sampled test
keeps the UI honest; and these two catch the wrong implementations a review
actually built against this plan.

**Define your pytest fixtures explicitly.** The tests above name `store`,
`store_truncated` and `store_two_sweeps`, and this plan does not say what they
are. Put them in a `conftest.py`, each loading its own fixture file into a
`tmp_path` store, and do **not** make them session-scoped over a shared store: a
test that mutates a shared store makes every later test's result depend on
ordering.

- [ ] **Step 2: Run them and watch them fail**

- [ ] **Step 3: Write the query and the thin wrapper**

Keep the SPARQL in the `.rq` file. The Python around it should do no filtering
that the query could do itself: if the query returns another endpoint's values and
Python filters them out, the test above passes and the query is still wrong.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each verified applied: drop the endpoint filter from the query (the
ontop test must fail); return the count of all sampled values rather than the
endpoint's (the size assertions must fail); report `sampled` as always true (the
qlever test must fail). Restore between each.

- [ ] **Step 5: Commit**

```bash
git add web/queries web/tests/test_endpoint_content.py
git commit -m "feat(web): answer what is in an endpoint, from the store"
```

---

## Task 4: Documentation

**Files:**
- Modify: `web/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: Document the loader and the query**

In `web/README.md`: how to load a run, that loading replaces rather than merges
and why, and how to run the query. Include the real numbers from the fixture so a
reader can tell whether their own run worked.

- [ ] **Step 2: Spec**

The delivery sequence has no entry for this, and a review flagged that inventing a
"Stage 2-0" row is inconsistent with how that table is structured and with how
earlier slices were recorded. Read the table first and follow its existing
convention for a partial delivery, rather than adding a new row shaped unlike its
neighbours.

Mark stage 2's status honestly whichever way you record it: the storage and one
read query exist; scoring as queries does not. Do not claim stage 2 or stage 3.

- [ ] **Step 3: Commit**

---

## Done criteria

- `cargo test --manifest-path prober/Cargo.toml` still reports 271 passed, 0
  failed, 2 ignored. The prober was not touched.
- The Python tests pass on 3.12 with the pinned dependencies.
- A run loads, reloads without duplicating, and a changed re-run replaces rather
  than merging, proven by the two-verdict test.
- Two different runs coexist in one store.
- The content query returns 59 classes for kadaster and 50 for ontop, neither
  leaking into the other, and reports qlever as not sampled rather than as empty.
