# Metric-agnostic sample pointers implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the web tier read a content sample from any sampling metric. Today it can only read `classes`, because the metric is hardcoded.

**Architecture:** Today one triple per endpoint says which run took its sample:

    <endpoint> sw:currentSampleRun <run>

That cannot say "classes from run A, properties from run B". Replace it with a small pointer resource per endpoint AND metric:

    <ptr> sw:sampleRunFor    <endpoint> ;
          sw:sampleRunMetric <metric> ;
          sw:sampleRunIs     <run> .

`web/load_run.py` writes one of these per (endpoint, metric) pair a run sampled. The four read queries follow it. The prober does not change.

**Tech Stack:** Python 3.12 (`web/.venv`), pyoxigraph 0.5.9, pytest, FastAPI, SPARQL 1.1.

**Spec:** `docs/superpowers/specs/2026-08-29-content-profiles-design.md`. Read Ruling 3 and the section "The recency pointer has to become per metric". The parent spec is `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`, section 1b.

## Why this is its own plan

The spec has two halves. The other half derives property profiles in the prober. Nothing could read those profiles until this half lands.

That is not hypothetical. The prober can already write a `properties` sample correctly, and the web tier cannot see it. There is even a test fixture for it, `web/tests/fixtures/run-properties-sample.nq`, committed to prove the sample stays invisible. This plan makes it visible, so that fixture is the proof this plan works.

## Global Constraints

- **Never use an em-dash.** Not in code, comments, docstrings, test names, commit messages or page copy. No exceptions.
- **Never `git add -A`.** Stage files by name.
- Commit messages end with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- **The governing rule: never report a confident wrong answer.** In particular, these two are different answers and must stay tellable apart:
  - we have no sample for this metric
  - we sampled, and found nothing
- **`urn:sparqlwatch:current` is derived.** It can always be rebuilt from the run graphs alone. Two rules follow:
  - never put a triple in it that no run graph supports
  - `rebuild_current` must produce exactly what normal loading produces
- **`Store.update()` cannot take `substitutions` in pyoxigraph 0.5.9.** So a value gets into an update as a prefix with an empty local name, written `endpoint:` or `run:`. To pass a new value, add a new prefix.
- **`Store.query()` CAN take `substitutions`,** but only for variables the query projects. So a variable you substitute must appear in the `SELECT`.
- **An update body has no `PREFIX` lines of its own.** pyoxigraph only allows them before the first operation. `_update_text` joins the bodies and adds one prologue for all of them.
- **Test fixtures: one named pytest fixture per store, and no generic loader.** In `web/tests/conftest.py` each scenario gets two things:

  ```python
  RUN_TWO_SWEEPS = FIXTURES / "run-two-sweeps.nq"      # a path constant

  @pytest.fixture
  def store_two_sweeps(tmp_path):                       # and a fixture
      return _loaded_store(tmp_path, "store-two-sweeps", RUN_TWO_SWEEPS)
  ```

  Always build through `_loaded_store`, which calls `load_run()`. Never call `Store.load()`: that skips the derived `current` graph, giving a store shape the real deployment never has.

  Need a new scenario? Add a constant and a fixture to `conftest.py`.

  **Do not invent a `load_fixture(...)` helper.** There isn't one. An earlier draft of this plan assumed there was, and every test in it was unrunnable.
- Run tests with `cd web && source .venv/bin/activate && python -m pytest -q`. There are 342 before this plan. Keep them green.

## File structure

| File | Responsibility after this plan |
|---|---|
| `web/load_run.py` | The only writer. Owns the pointer's name, its predicates, keeping it up to date, spotting when it goes stale, and rebuilding it. |
| `web/queries/endpoint_content.rq` | One endpoint's sample for one metric, passed in. No metric hardcoded. |
| `web/queries/endpoint_description.rq` | Follows the same pointer, for one endpoint's RDF. |
| `web/queries/index_description.rq` | Follows the same pointer, for the index's RDF. |
| `web/app.py` | Passes the metric down. Its comment about the old pointer needs updating. |
| `web/tests/fixtures/run-two-metrics-sampled.nq` | NEW. One run, two sampling metrics. Gives the new pointer something to get right. |
| `web/tests/fixtures/run-properties-later.nq` | NEW. A later run sampling only `properties`, so the two pointers must name different runs. |


---

### Task 1: The pointer's identity and the write side

Swap the one-triple-per-endpoint pointer for one pointer resource per (endpoint, metric) pair, and teach `load_run` to maintain it.

**Files:**
- Modify: `web/load_run.py`
  - delete: `CURRENT_SAMPLE_RUN`, `_SAMPLED_ENDPOINTS`
  - add: the three `SAMPLE_RUN_*` predicates, `_sample_pointer_iri`, `_SAMPLED_PAIRS`, `_run_sampled_pairs`, `_pair_run`
  - rewrite: `_POINTERS`, `_REPLACE_SAMPLED`, the sample half of `load_run`'s per-run loop, and two docstring sections (QUAD SHAPE and TWO POINTERS)
  - leave alone: `_endpoint_run`. The measurement half still uses it as is.
- Create: `web/tests/fixtures/run-two-metrics-sampled.nq`
- Create: `web/tests/fixtures/run-properties-later.nq`
- Modify: `web/tests/conftest.py` (two path constants and three fixtures, per the convention in Global Constraints)
- Test: `web/tests/test_load_run.py`

**Interfaces:**
- Consumes: nothing. This is the first task.
- Produces, for Tasks 2 to 4:
  - `SAMPLE_RUN_FOR = "urn:sparqlwatch:sampleRunFor"`
  - `SAMPLE_RUN_METRIC = "urn:sparqlwatch:sampleRunMetric"`
  - `SAMPLE_RUN_IS = "urn:sparqlwatch:sampleRunIs"`
  - `def _sample_pointer_iri(endpoint: str, metric: str) -> str`
  - `_run_sampled_pairs(store, run) -> set[tuple[str, str]]`, which replaces `_run_endpoints(store, run, _SAMPLED_ENDPOINTS)`
  - `_newest_per_endpoint` still returns `(measured, sampled)`, but `sampled` is now keyed by (endpoint, metric): `dict[tuple[str, str], tuple[str, str]]`

- [ ] **Step 1: Write the two fixtures**

First, `web/tests/fixtures/run-two-metrics-sampled.nq`: one run, one endpoint, two sampling metrics. Copy the style of `run-properties-sample.nq`, which already uses `sw:metric:properties`, so nothing here is a new vocabulary.

```
<urn:sparqlwatch:activity:2026-01-01T00:00:00Z> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/prov#Activity> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:activity:2026-01-01T00:00:00Z> <http://www.w3.org/ns/prov#generatedAtTime> "2026-01-01T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<http://example.org/sparql> <urn:sparqlwatch:declarationsRead> "true"^^<http://www.w3.org/2001/XMLSchema#boolean> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:sparqlwatch:ContentSample> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <urn:sparqlwatch:sampledFrom> <http://example.org/sparql> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:classes> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <urn:sparqlwatch:sampledValue> <http://xmlns.com/foaf/0.1/Person> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <urn:sparqlwatch:sampleTruncated> "false"^^<http://www.w3.org/2001/XMLSchema#boolean> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:classes> <urn:sparqlwatch:sampleSize> "1"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:sparqlwatch:ContentSample> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <urn:sparqlwatch:sampledFrom> <http://example.org/sparql> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <urn:sparqlwatch:sampledBy> <urn:sparqlwatch:metric:properties> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <urn:sparqlwatch:sampledValue> <http://xmlns.com/foaf/0.1/name> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <urn:sparqlwatch:sampleTruncated> "false"^^<http://www.w3.org/2001/XMLSchema#boolean> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
<urn:sparqlwatch:sample:2026-01-01T00:00:00Z:http%3A%2F%2Fexample.org%2Fsparql:properties> <urn:sparqlwatch:sampleSize> "1"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:sparqlwatch:run:2026-01-01T00:00:00Z> .
```

Second, `web/tests/fixtures/run-properties-later.nq`: a later run that samples only `properties`, for the same endpoint.

This fixture is the reason the plan exists. Load both files, and the classes pointer must still name the January run while the properties pointer names the February one. One pointer per endpoint cannot do that.

Build it from the block above:
- change every `2026-01-01T00:00:00Z` to `2026-02-01T00:00:00Z`
- delete the six `:classes` lines
- change the `sampledValue` to `<http://xmlns.com/foaf/0.1/mbox>`

- [ ] **Step 2: Declare the fixtures in conftest.py**

Following the convention in Global Constraints. Add beside the existing constants:

```python
RUN_TWO_METRICS_SAMPLED = FIXTURES / "run-two-metrics-sampled.nq"
RUN_PROPERTIES_LATER = FIXTURES / "run-properties-later.nq"
```

and three fixtures beside the existing ones:

```python
@pytest.fixture
def store_two_metrics(tmp_path):
    """One run sampling two metrics for one endpoint, so a pointer keyed on
    (endpoint, metric) has something to be right about. See the comment in
    web/tests/fixtures/run-two-metrics-sampled.nq."""
    return _loaded_store(tmp_path, "store-two-metrics", RUN_TWO_METRICS_SAMPLED)


@pytest.fixture
def store_metrics_diverged(tmp_path):
    """Two runs whose newest samples are for DIFFERENT metrics: classes from
    the first, properties from the second. The case a single per-endpoint
    sample pointer cannot express at all, and the reason this shape changed."""
    return _loaded_store(
        tmp_path, "store-metrics-diverged", RUN_TWO_METRICS_SAMPLED, RUN_PROPERTIES_LATER
    )


@pytest.fixture
def store_two_metrics_reloaded(tmp_path):
    """The same run loaded twice under the same run IRI, which is the
    documented recovery, so the idempotence of the pointer replace is
    exercised in earnest rather than only by mistake."""
    return _loaded_store(
        tmp_path, "store-two-metrics-reloaded", RUN_TWO_METRICS_SAMPLED, RUN_TWO_METRICS_SAMPLED
    )
```

- [ ] **Step 3: Write the failing test**

Add these to `web/tests/test_load_run.py`. They take the fixtures from the last step. The endpoint is `http://example.org/sparql`, which is what both new run files describe. If that file already has a constant for a fixture endpoint, use it.

```python
def test_two_metrics_sampled_in_different_runs_both_keep_a_pointer(store_metrics_diverged):
    # The reason this plan exists. One pointer per endpoint cannot say "classes
    # came from January and properties from February", and picking either run
    # loses the other metric's sample outright: the stage 3-1 defect one level
    # down.
    rows = {
        (r["metric"].value, r["run"].value)
        for r in store_metrics_diverged.query(
            """
            SELECT ?metric ?run WHERE {
              GRAPH <urn:sparqlwatch:current> {
                ?ptr <urn:sparqlwatch:sampleRunFor> <http://example.org/sparql> ;
                     <urn:sparqlwatch:sampleRunMetric> ?metric ;
                     <urn:sparqlwatch:sampleRunIs> ?run .
              }
            }
            """
        )
    }
    assert rows == {
        ("urn:sparqlwatch:metric:classes", "urn:sparqlwatch:run:2026-01-01T00:00:00Z"),
        ("urn:sparqlwatch:metric:properties", "urn:sparqlwatch:run:2026-02-01T00:00:00Z"),
    }, f"each metric keeps its own newest run: {sorted(rows)}"


def test_the_sample_pointer_is_addressable_by_subject(store_two_metrics):
    # load_run replaces a pointer with DELETE WHERE keyed on the pointer's own
    # subject, so that subject has to be derivable in Python from (endpoint,
    # metric) without reading the store first. A blank node could not serve
    # that, which is why Ruling 3 chose a derived IRI.
    expected = load_run_module._sample_pointer_iri(
        "http://example.org/sparql", "urn:sparqlwatch:metric:classes"
    )
    held = [
        r["ptr"].value
        for r in store_two_metrics.query(
            """
            SELECT ?ptr WHERE {
              GRAPH <urn:sparqlwatch:current> {
                ?ptr <urn:sparqlwatch:sampleRunMetric> <urn:sparqlwatch:metric:classes> .
              }
            }
            """
        )
    ]
    assert held == [expected], f"the derived IRI is the subject actually written: {held}"


def test_reloading_the_same_run_does_not_duplicate_a_pointer(store_two_metrics_reloaded):
    n = int(
        next(
            iter(
                store_two_metrics_reloaded.query(
                    """
                    SELECT (COUNT(*) AS ?n) WHERE {
                      GRAPH <urn:sparqlwatch:current> {
                        ?ptr <urn:sparqlwatch:sampleRunIs> ?run
                      }
                    }
                    """
                )
            )
        )["n"].value
    )
    assert n == 2, f"two metrics, two pointers, not four: {n}"


def test_no_currentsamplerun_triple_survives(store_two_metrics):
    # The old predicate is replaced, not kept alongside. Two spellings of one
    # fact is how a reader ends up preferring the stale one.
    n = int(
        next(
            iter(
                store_two_metrics.query(
                    """
                    SELECT (COUNT(*) AS ?n) WHERE {
                      GRAPH <urn:sparqlwatch:current> {
                        ?e <urn:sparqlwatch:currentSampleRun> ?run
                      }
                    }
                    """
                )
            )
        )["n"].value
    )
    assert n == 0, f"sw:currentSampleRun is retired: {n} left"
```

In the second test, `load_run_module` is a placeholder. Check how `test_load_run.py` already reaches private names in that module and match it. If it imports them directly, import `_sample_pointer_iri` directly.

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -k "pointer or metrics_sampled or currentsamplerun" -v`

Expected: all four FAIL.
- the first three: an empty result set, or `ImportError` on `_sample_pointer_iri`
- the fourth: `1 left`, because today's code writes exactly that one triple

- [ ] **Step 5: Add the pointer's identity and predicates**

In `web/load_run.py`, next to `CURRENT_RUN`, delete `CURRENT_SAMPLE_RUN` and put this in its place. Delete it outright; do not leave it sitting there unused.

```python
CURRENT_RUN = "urn:sparqlwatch:currentRun"

# The sample pointer, keyed on (endpoint, metric). One triple per endpoint
# cannot say "classes came from January and properties from February", and
# picking either run loses the other metric's sample: the same loss that
# sw:currentSampleRun exists to prevent, one level down. See Ruling 3 in
# docs/superpowers/specs/2026-08-29-content-profiles-design.md.
SAMPLE_RUN_FOR = "urn:sparqlwatch:sampleRunFor"
SAMPLE_RUN_METRIC = "urn:sparqlwatch:sampleRunMetric"
SAMPLE_RUN_IS = "urn:sparqlwatch:sampleRunIs"

_SAMPLE_POINTER_PREFIX = "urn:sparqlwatch:sampleptr:"


def _sample_pointer_iri(endpoint: str, metric: str) -> str:
    """The pointer resource for one (endpoint, metric) pair.

    A derived IRI rather than a blank node because the replace path addresses
    the pointer by its own subject in a DELETE WHERE, which a blank node cannot
    serve without matching on its properties: slower, and fragile against a
    partial write.

    Both components are percent-encoded with an empty safe set, so the ':'
    separators below are the only unencoded ones and the IRI is unambiguous.
    This encoding does not have to match the prober's `encode_unreserved`: this
    resource lives only in the derived current graph, which the prober never
    writes, and it is reconstructible from the run graphs by rebuild_current.
    """
    return (
        _SAMPLE_POINTER_PREFIX
        + quote(endpoint, safe="")
        + ":"
        + quote(metric, safe="")
    )
```

Add `from urllib.parse import quote` to the imports if it is not already there.

- [ ] **Step 6: Discover (endpoint, metric) pairs instead of endpoints**

Replace `_SAMPLED_ENDPOINTS` with `_SAMPLED_PAIRS`, and add its reader beside `_run_endpoints`.

```python
# Which (endpoint, metric) pairs a run published a sample for. No metric is
# named: a sample from any sampling metric gets a pointer, which is the point of
# this shape. web/tests/fixtures/run-properties-sample.nq used to exist to prove
# the sw:metric:classes pin held and now proves it is gone.
_SAMPLED_PAIRS = _PREAMBLE + """
SELECT DISTINCT ?endpoint ?metric WHERE {
  GRAPH run: {
    ?sample sw:sampledFrom ?endpoint ; sw:sampledBy ?metric .
  }
}
"""


def _run_sampled_pairs(store: Store, run: str) -> set[tuple[str, str]]:
    """The (endpoint, metric) pairs ``run`` published a content sample for."""
    return {
        (str(row["endpoint"].value), str(row["metric"].value))
        for row in store.query(_SAMPLED_PAIRS, prefixes={_RUN_PREFIX: run})
    }
```

- [ ] **Step 7: Replace the pointer per pair**

`_REPLACE_SAMPLED` becomes pair-keyed, and needs the metric and the pointer IRI as prefixes because `Store.update` cannot take substitutions.

```python
_METRIC_PREFIX = "metric"
_POINTER_PREFIX = "ptr"

# One (endpoint, metric) pair's sample pointer, replaced. The sample's own quads
# stay in their run graph and are read through this pointer, because the index
# scans current for verdicts and never for sample values, so copying several
# hundred sw:sampledValue triples per endpoint into current would buy nothing
# and would grow the graph the index scans.
#
# DELETE WHERE on ptr: and not on the endpoint, so replacing one metric's
# pointer cannot disturb another's. That is the whole difference from the
# predicate this replaced.
_REPLACE_SAMPLED = """
DELETE WHERE { GRAPH sw:current { ptr: ?p ?o } } ;
INSERT DATA { GRAPH sw:current {
  ptr: sw:sampleRunFor endpoint: ;
       sw:sampleRunMetric metric: ;
       sw:sampleRunIs run: .
} }
"""


def _pair_run(endpoint: str, metric: str, run: str) -> dict[str, str]:
    """Prefix bindings for one pair's pointer update."""
    return {
        _ENDPOINT_PREFIX: endpoint,
        _RUN_PREFIX: run,
        _METRIC_PREFIX: metric,
        _POINTER_PREFIX: _sample_pointer_iri(endpoint, metric),
    }
```

- [ ] **Step 8: Move the bookkeeping to pair keys**

In `load_run`'s per-run loop, the sample half currently does two things this step changes. It calls `_run_endpoints(store, run, _SAMPLED_ENDPOINTS)`, and it keys `live` on `(endpoint, CURRENT_SAMPLE_RUN)`.

Make it this:

```python
        sampled_pairs = _run_sampled_pairs(store, run)
        ...
        # The measured half is unchanged and still keyed (endpoint, CURRENT_RUN).
        # The sample half is a separate update per pair, because two pairs can
        # name different runs and so cannot share one prefix binding. Each is
        # transactional on its own; an interrupted load is repaired by running
        # it again, exactly as rebuild_current documents for its own two units.
        for endpoint, metric in sorted(sampled_pairs):
            if _advance(live.get((endpoint, metric)), run, instant):
                store.update(
                    _update_text(_REPLACE_SAMPLED),
                    prefixes=_pair_run(endpoint, metric, run),
                )
                advanced_samples.add(endpoint)
                live[(endpoint, metric)] = (run, instant)
            else:
                kept_newer.add(endpoint)
```

**This costs something, and the plan is not hiding it.** The sample pointer no longer moves in the same `store.update()` call as the endpoint's measurements. It cannot: each pair needs its own prefix bindings, so each pair needs its own call.

What is still safe: `_REPLACE_MEASURED` is one call, so an endpoint's measurements are never half-updated.

What is no longer safe: measurements and sample pointers moving together as one unit.

`rebuild_current` already accepts the same tradeoff for its own two steps, and Task 2's drift detector is what catches a load that stopped halfway.

Update `_POINTERS` so the sample pointers come back too:

```python
_POINTERS = _PREAMBLE + """
SELECT ?endpoint ?which ?run ?instant WHERE {
  {
    GRAPH sw:current { ?endpoint sw:currentRun ?run }
    BIND (sw:currentRun AS ?which)
  } UNION {
    GRAPH sw:current {
      ?ptr sw:sampleRunFor ?endpoint ;
           sw:sampleRunMetric ?which ;
           sw:sampleRunIs ?run .
    }
  }
  OPTIONAL {
    GRAPH ?run { ?activity a prov:Activity ; prov:generatedAtTime ?instant }
  }
}
"""
```

`?which` now holds one of two things: `sw:currentRun` for a run pointer, or the metric IRI for a sample pointer. So the dict `_pointers` builds ends up keyed `(endpoint, CURRENT_RUN)` for one and `(endpoint, metric)` for the other. `_pointers` itself needs no change.

The two key spaces cannot collide. A metric IRI always starts `urn:sparqlwatch:metric:`, and `metrics.rs` limits a metric id to `[a-z0-9][a-z0-9-]*`, so no metric can ever be spelled `urn:sparqlwatch:currentRun`.

- [ ] **Step 9: Update the module docstring**

Two passages in the docstring describe the old shape and are now wrong. In QUAD SHAPE, replace the `E sw:currentSampleRun <run>` line with this:

```
  <ptr> sw:sampleRunFor E       one pointer resource per (E, metric) pair,
        sw:sampleRunMetric M    naming the newest run that published an
        sw:sampleRunIs <run>    M sample of E. See _sample_pointer_iri.
```

In TWO POINTERS, keep the argument that is already there and add to it. The argument itself is what carries over:

```
TWO KINDS OF POINTER, which is the subtle half. The newest run that MEASURED an
endpoint and the newest run that SAMPLED it are different runs the moment a
cheap sweep declines a sampling metric, and that is the steady state: the
543-endpoint registry sweep declined sw:metric:classes for every one of them.
One pointer with one notion of recency loses the sample outright.

The same argument applies once more, one level down, which is why the sample
pointer is keyed on (endpoint, METRIC) rather than on the endpoint. A run may
sample classes and decline properties, so a single per-endpoint sample pointer
loses whichever metric came from the older run. Ruling 3 in
docs/superpowers/specs/2026-08-29-content-profiles-design.md.
```

- [ ] **Step 10: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -v`

Expected: the four new tests PASS.

Other tests in this file that check `sw:currentSampleRun` will FAIL. That is correct: they pin the shape being replaced. Update each one to the new shape, keeping whatever it was actually checking.

Do not delete a test to get a green suite. If a test's subject is genuinely gone, say so in its comment and point it at the fact that replaced it.

- [ ] **Step 11: Run the whole suite**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected: some failures, and only in these four files, which Tasks 2 to 4 fix:
`test_endpoint_content.py`, `test_reader_golden.py`, `test_negotiation.py`, `test_page.py`.

Write down which failed and how many, in the task report. A failure in any other file belongs to this task, so fix it here.

- [ ] **Step 12: Commit**

```bash
git add web/load_run.py web/tests/test_load_run.py \
        web/tests/fixtures/run-two-metrics-sampled.nq \
        web/tests/fixtures/run-properties-later.nq
git commit -m "Key the sample pointer on the metric, not just the endpoint"
```

---

### Task 2: Drift detection and rebuild for the new shape

Two things know the old pointer shape and must learn the new one.

`load_run` reports a `current` graph that has drifted away from its run graphs. `rebuild_current` repairs one.

Skip this and two things break quietly. A stale sample pointer stops being detectable, and a rebuild starts producing a different graph from the one normal loading produces.

**Files:**
- Modify: `web/load_run.py` (`_DRIFTED_SAMPLE_POINTERS`, `_newest_per_endpoint`, `rebuild_current`, and the verification block near line 910)
- Test: `web/tests/test_load_run.py`

**Interfaces:**
- Consumes from Task 1: the three `SAMPLE_RUN_*` predicates, plus `_sample_pointer_iri`, `_run_sampled_pairs`, `_REPLACE_SAMPLED` and `_pair_run`.
- Produces: `_newest_per_endpoint` with `sampled` keyed `(endpoint, metric)`. `LoadResult.drifted` keeps its existing shape, a list of endpoint strings.

- [ ] **Step 1: Write the failing test**

**There is no public `detect_drift`.** Drift comes from the private `_drifted(store)`, which runs both drift queries and returns a sorted list of endpoint strings. It reaches callers as `LoadResult.drifted` and reaches operators through `_drift_advice`. Call what exists; do not add a public entry point just for this plan.

**Pick the case that actually discriminates.** It is a run that sampled only a non-classes metric. Under the old shape such a run got no sample pointer at all, so there was nothing to go stale and nothing to detect.

`run-properties-sample.nq` is exactly that run, and `conftest.py` already declares it as `store_properties_sample`.

```python
def test_a_dropped_run_graph_drifts_a_non_classes_sample_pointer(store_properties_sample):
    # Before this plan no pointer existed for a properties sample, so its loss
    # was undetectable: current could not be attributing a properties sample to
    # a missing run because it never claimed one. Now it can, so it must be
    # caught, or a dropped run graph leaves the site publishing a sample that no
    # run states.
    store_properties_sample.remove_graph(
        NamedNode("urn:sparqlwatch:run:2026-01-01T00:00:00Z")
    )
    drifted = _drifted(store_properties_sample)
    assert drifted == ["http://example.org/sparql"], (
        f"the endpoint whose properties pointer now names a gone run: {drifted}"
    )


def test_rebuild_reproduces_what_incremental_loading_produced(store_metrics_diverged):
    # rebuild_current exists to repair drift, so it has to land on exactly the
    # graph incremental loading lands on. If the two disagree, a repair is a
    # silent change of what the site says. This is the case that can disagree:
    # two metrics whose newest samples come from different runs, which the
    # rebuild has to reconstruct from the run graphs alone.
    incremental = _current_triples(store_metrics_diverged)

    rebuild_current(store_metrics_diverged)
    rebuilt = _current_triples(store_metrics_diverged)

    assert rebuilt == incremental, (
        "a rebuild is a repair, not a rewrite: "
        f"only in incremental {sorted(incremental - rebuilt)}, "
        f"only in rebuilt {sorted(rebuilt - incremental)}"
    )
```

Add this helper next to them in `web/tests/test_load_run.py`. If the file already has an equivalent, use that instead:

```python
def _current_triples(store) -> set[tuple[str, str, str]]:
    """Every triple of the derived graph, as comparable strings."""
    return {
        (str(q.subject), str(q.predicate), str(q.object))
        for q in store.quads_for_pattern(
            None, None, None, NamedNode("urn:sparqlwatch:current")
        )
    }
```

Two things to check before running:
- the run IRI in the first test. Read it out of the fixture file rather than trusting the spelling above.
- that `NamedNode` and `_drifted` are imported in that module. Add them to the existing import lines if not.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -k "drift_naming or reproduces" -v`

Expected: both FAIL.

The drift test returns `[]`. After Task 1 there IS a properties pointer, but the detector still asks "did this run sample `sw:metric:classes`?" It never did, so the pointer looks fine.

The rebuild test fails because `rebuild_current` still writes `sw:currentSampleRun` while loading now writes pointer resources. The two graphs differ on every sample pointer.

- [ ] **Step 3: Generalise the drift detector**

```python
# Which sample pointers name a run whose graph no longer holds the sample they
# claim. Both components are returned, because "this endpoint drifted" does not
# say which metric to look at once an endpoint can carry several pointers.
_DRIFTED_SAMPLE_POINTERS = _PREAMBLE + """
SELECT ?endpoint ?metric WHERE {
  GRAPH sw:current {
    ?ptr sw:sampleRunFor ?endpoint ;
         sw:sampleRunMetric ?metric ;
         sw:sampleRunIs ?run .
  }
  FILTER NOT EXISTS {
    GRAPH ?run {
      ?sample sw:sampledFrom ?endpoint ; sw:sampledBy ?metric .
    }
  }
}
"""
```

The important change is in the `FILTER NOT EXISTS`. It now joins on `?metric` instead of a fixed IRI, so the question changes from:

- "did this run sample classes?" to
- "did this run sample the metric this pointer claims it did?"

The second is the question that was always meant.

**`_drifted` needs no edit. Leave it alone.** It reads `row["endpoint"].value` from both queries and unions the results, so an extra `?metric` column changes nothing for it.

`?metric` is projected because the `FILTER NOT EXISTS` has to join on it. That is the real fix. Having the column available for a better message later is a bonus, not a reason to change `LoadResult.drifted` now.

And the reporting granularity is already right: the repair is per endpoint, since you rebuild the graph. Naming the endpoint once is correct even when two of its pointers went stale.

- [ ] **Step 4: Generalise `_newest_per_endpoint`**

Its `sampled` return becomes keyed `(endpoint, metric)`:

```python
        for endpoint, metric in sorted(_run_sampled_pairs(store, run)):
            existing = pointers.get((endpoint, metric))
            if _tied(existing, run, instant):
                raise ValueError(_tie_message(endpoint, existing[0], run, instant))
            if _advance(pointers.get((endpoint, metric)), run, instant):
                pointers[(endpoint, metric)] = (run, instant)
```

`_tie_message` currently takes an endpoint and two runs. A tie is now per pair, so add the metric to it.

Without that, two different metrics tying on one endpoint print the same message twice and nobody can tell them apart. Extend the signature. There is one other caller, and both are in this module.

- [ ] **Step 5: Generalise `rebuild_current`**

```python
    runs = _run_graphs(store)
    measured, sampled = _newest_per_endpoint(store, runs)

    if store.contains_named_graph(CURRENT_GRAPH):
        store.remove_graph(CURRENT_GRAPH)

    for endpoint in sorted(measured):
        store.update(
            _update_text(_REPLACE_MEASURED),
            prefixes=_endpoint_run(endpoint, measured[endpoint][0]),
        )

    # One call per (endpoint, metric) pair. The pairs can name different runs,
    # which is the whole point of keying on the metric, so they cannot share one
    # prefix binding. Each is transactional on its own, and a rebuild
    # interrupted between them is repaired by running it again.
    for (endpoint, metric), (run, _instant) in sorted(sampled.items()):
        store.update(
            _update_text(_REPLACE_SAMPLED),
            prefixes=_pair_run(endpoint, metric, run),
        )
```

The old loop does `sorted(set(measured) | set(sampled))` and branches inside it. That union no longer works, because the two dicts are keyed differently now.

Two separate loops are clearer anyway. Move the comment about the two units naming different runs down to the second loop, where it is now the point.

- [ ] **Step 6: Generalise the post-load verification**

The block near line 910 checks that every endpoint the run sampled has a pointer naming the run you expect, and raises an error mentioning `sw:currentSampleRun` when it does not.

Point it at the pair-keyed dict and the new predicates. Keep both of its messages, because they are different failures:
- there is no pointer at all
- there is a pointer, and it names the wrong run

Add the metric to both messages.

- [ ] **Step 7: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -v`

Expected: all PASS. This file is the entire test surface for the write side, so get it green before touching the read side.

- [ ] **Step 8: Commit**

```bash
git add web/load_run.py web/tests/test_load_run.py web/tests/conftest.py
git commit -m "Detect and repair a drifted sample pointer per metric"
```

---

### Task 3: `endpoint_content.rq` reads any metric's sample

This is the HTML path.

Today `endpoint_content` answers: "what classes did the newest sampling run find?" The metric is baked into the query.

After this task it answers: "what did the newest run that sampled THIS metric find?" The metric is an argument.

**Files:**
- Modify: `web/queries/endpoint_content.rq`
- Modify: `web/endpoint_content.py` (`endpoint_content`, `EndpointContent`)
- Modify: `web/app.py` (the one call site, and the prose naming `sw:currentSampleRun`)
- Test: `web/tests/test_endpoint_content.py`

**Interfaces:**
- Consumes from Task 1: the three pointer predicates.
- Produces: `endpoint_content(store, endpoint, metric="urn:sparqlwatch:metric:classes")`, plus a new `EndpointContent.metric` field saying which metric the answer is about.

The default keeps every existing caller working and keeps this diff small. Task 4's callers pass the metric explicitly.

The default is honest rather than a hedge: `classes` is the only sampling metric the shipped `metrics.toml` declares, so it really is what "the sample" means until a second one ships.

- [ ] **Step 1: Write the failing test**

Add these to `web/tests/test_endpoint_content.py`. They use fixtures the file already has, plus the two Task 1 added.

Before writing `content.values`, check what `EndpointContent` actually calls that field and use its name. Do not rename it in this task.

```python
def test_a_properties_sample_is_now_visible(store_properties_sample):
    # This fixture was committed to prove the sw:metric:classes pin held: its
    # sample was deliberately invisible, and a test in this file asserts that.
    # The pin is gone, so the fixture's meaning inverts, and this records the
    # inversion rather than quietly deleting the old assertion.
    content = endpoint_content(
        store_properties_sample,
        "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:properties",
    )
    assert content.sampled, "a properties sample is a sample"
    assert content.metric == "urn:sparqlwatch:metric:properties"
    assert content.values, "and it carries the values the run published"


def test_asking_for_a_metric_that_was_not_sampled_is_not_an_error(store_two_metrics):
    # The governing rule. "This run did not sample geo-data" and "geo-data was
    # sampled and found nothing" are different answers and the caller has to be
    # able to tell them apart. Neither is an exception.
    absent = endpoint_content(
        store_two_metrics, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:geo-data",
    )
    assert not absent.sampled, "no sample is not an empty sample"

    present = endpoint_content(
        store_two_metrics, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:classes",
    )
    assert present.sampled and present.values


def test_two_metrics_do_not_bleed_into_each_other(store_two_metrics):
    # The failure this plan is insurance against: reading one metric's pointer
    # and getting another metric's values. The fixture publishes exactly one
    # value per metric, chosen to be distinguishable.
    classes = endpoint_content(
        store_two_metrics, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:classes",
    )
    properties = endpoint_content(
        store_two_metrics, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:properties",
    )
    assert classes.values == ["http://xmlns.com/foaf/0.1/Person"]
    assert properties.values == ["http://xmlns.com/foaf/0.1/name"]


def test_each_metric_reads_its_own_run(store_metrics_diverged):
    # Two metrics whose newest samples come from different runs, which a single
    # per-endpoint pointer could not express at all.
    classes = endpoint_content(
        store_metrics_diverged, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:classes",
    )
    properties = endpoint_content(
        store_metrics_diverged, "http://example.org/sparql",
        metric="urn:sparqlwatch:metric:properties",
    )
    assert classes.run == "urn:sparqlwatch:run:2026-01-01T00:00:00Z"
    assert properties.run == "urn:sparqlwatch:run:2026-02-01T00:00:00Z"
    assert properties.values == ["http://xmlns.com/foaf/0.1/mbox"]
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_endpoint_content.py -k "properties or bleed or own_run or not_sampled" -v`

Expected: FAIL with `TypeError: endpoint_content() got an unexpected keyword argument 'metric'`.

- [ ] **Step 3: Parameterise the query**

In `web/queries/endpoint_content.rq`, change the pointer lookup to this:

```sparql
  # The run that took this endpoint's sample OF THIS METRIC. Two quads now
  # rather than one, and still no recency work: the pointer resource carries
  # the pair, so asking for one metric cannot reach another's run.
  GRAPH sw:current {
    ?ptr sw:sampleRunFor ?endpoint ;
         sw:sampleRunMetric ?metric ;
         sw:sampleRunIs ?run .
  }
```

Then inside the hop, drop the hardcoded filter and join on the same variable instead:

```sparql
      ?sample sw:sampledFrom ?endpoint ;
              sw:sampleSize ?size ;
              sw:sampleTruncated ?truncated ;
              sw:sampledBy ?metric .
```

Delete `FILTER (?sampledBy = sw:metric:classes)`, and the `?sampledBy` variable with it.

Keep the comment above it, pointed at `?metric` instead. It explains why this is a bound variable plus a filter rather than a bound object, and that reasoning still holds: `?metric` is bound too, by substitution.

Add `?metric` to the `SELECT`. pyoxigraph only substitutes variables a query projects.

That is exactly why `?endpoint` is projected, and the header already explains it. Extend that sentence rather than writing a second one.

Update the header's "Most recent IS NOT COMPUTED HERE" paragraph. It names `sw:currentSampleRun` and describes a per-endpoint pointer.

Leave the timing figures in it alone. They measure the shape of the hop, not the pointer's key, so they are still correct.

- [ ] **Step 4: Parameterise the reader**

```python
CLASSES_METRIC = "urn:sparqlwatch:metric:classes"
_METRIC = Variable("metric")


def endpoint_content(
    store: Store, endpoint: str, metric: str = CLASSES_METRIC
) -> EndpointContent:
    """Return what the most recent run that sampled ``metric`` on ``endpoint`` found.

    ``metric`` defaults to sw:metric:classes, which is the only sampling metric
    the shipped metrics.toml declares, so it is the answer to "the sample" until
    a second one ships.

    An endpoint with no sample for this metric returns ``sampled=False``. That
    is not the same answer as a sample that found nothing, and the two must stay
    distinguishable: see the OPTIONAL on sw:sampledValue in the query.

    Raises ValueError if two distinct runs tie for most recent on this pair,
    which means two run graphs carry the same prov:generatedAtTime. That is a
    corrupt store rather than a question with two answers, and picking one of
    them silently would hide it.
    """
    rows = list(
        store.query(
            _QUERY,
            substitutions={
                _ENDPOINT: NamedNode(endpoint),
                _METRIC: NamedNode(metric),
            },
        )
    )
    if not rows:
        return EndpointContent(endpoint=endpoint, metric=metric, sampled=False)
    ...
```

Add `metric: str` to `EndpointContent` and set it on both return paths.

Then find every place that constructs an `EndpointContent`, in the code and in the tests, and give it the new field. Do not add a default for it: a default would let a caller publish a sample without saying what the sample is of.

Add the metric to the tie message, for the same reason Task 2 did it to `_tie_message`.

- [ ] **Step 5: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_endpoint_content.py -v`

Expected: PASS. The existing tests in this file check the classes sample, and the default keeps them working.

- [ ] **Step 6: Update app.py's prose**

`web/app.py:359` describes the lookup through `sw:currentSampleRun`, either in a comment or in page copy. Point it at the new shape.

If it is text a visitor reads, keep it plain and accurate. Say the fact, not the predicate: "the newest sweep that sampled this" beats naming an IRI.

- [ ] **Step 7: Run the whole suite**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected:
- green: `test_load_run.py`, `test_endpoint_content.py`
- may still fail: `test_reader_golden.py`, `test_negotiation.py`, `test_page.py`. Those are the RDF path, which is Task 4.

Record the count in the report.

- [ ] **Step 8: Commit**

```bash
git add web/queries/endpoint_content.rq web/endpoint_content.py web/app.py \
        web/tests/test_endpoint_content.py
git commit -m "Read a content sample by metric instead of assuming classes"
```

---

### Task 4: The RDF representations read it too

`endpoint_description.rq` and `index_description.rq` are CONSTRUCTs, serialised straight out of the store. Both still follow the old pointer.

So until this task lands, the HTML and the RDF for one endpoint say different things. That disagreement is exactly what `test_negotiation.py` exists to catch.

**Files:**
- Modify: `web/queries/endpoint_description.rq` (the sample branch, near line 255)
- Modify: `web/queries/index_description.rq` (the sample branch, near line 24)
- Test: `web/tests/test_negotiation.py`, `web/tests/test_reader_golden.py`

**Interfaces:**
- Consumes from Task 1: the three pointer predicates. From Task 3: the idea that a sample is identified by an endpoint AND a metric.

- [ ] **Step 1: Write the failing test**

```python
def test_the_rdf_and_the_html_agree_about_a_properties_sample(store_two_metrics):
    # The hazard app.py's module docstring names: two representations of one
    # resource derived two ways, which is a strength only while they agree. A
    # properties sample visible in HTML and missing from the RDF is the failure
    # this test exists for, and it is newly reachable because Task 3 made the
    # HTML side see properties at all.
    turtle = _serve(store_two_metrics,
        "http://example.org/sparql", accept="text/turtle")
    assert "foaf/0.1/name" in turtle or "sampledValue" in turtle, (
        "the RDF must carry the properties sample the HTML now shows"
    )
    assert "sampleRunIs" not in turtle, (
        "the derived pointer is our bookkeeping, not a fact about the endpoint"
    )
```

**`_serve` above is a placeholder. Do not add it.**

Read `test_negotiation.py` first. It already has a way to fetch both representations of one endpoint and compare them. If that helper can express this case, put the case inside it rather than next to it. Either way, use that file's own helper and its own request convention exactly as written.

An earlier draft of this plan invented a `load_fixture` fixture and a `client` fixture, neither of which exists, and every test using them was unrunnable. Do not repeat that by inventing `_serve`.

The second assertion is the important one, and it settles a real design question.

The pointer lives in the derived `current` graph. It is our bookkeeping. The published RDF is about somebody else's endpoint. A CONSTRUCT that emitted `sw:sampleRunIs` would publish our bookkeeping as though it were a fact about their server. It must not.

Check the old shape before assuming this is a new problem:
- if `sw:currentSampleRun` was already kept out of both CONSTRUCTs, keep it that way
- if it leaked, say so in the report. That is a pre-existing bug this task can close.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_negotiation.py -v`

Expected: FAIL.

Both CONSTRUCTs still look for `sw:currentSampleRun`. Task 1 stopped writing it. So their sample branches match nothing and the RDF carries no sample at all.

- [ ] **Step 3: Update `endpoint_description.rq`**

The sample branch near line 255 currently reads:

```sparql
    GRAPH sw:current { ?endpoint sw:currentSampleRun ?sampleRun }
```

It becomes:

```sparql
    GRAPH sw:current {
      ?sampleptr sw:sampleRunFor ?endpoint ;
                 sw:sampleRunMetric ?sampleMetric ;
                 sw:sampleRunIs ?sampleRun .
    }
```

Then, inside the hop into `?sampleRun`, join `sw:sampledBy` to `?sampleMetric` instead of to a fixed IRI. Same change Task 3 made to the SELECT.

This is a CONSTRUCT, so unlike Task 3 it does not need `?sampleMetric` projected.

The behaviour change is intended: this query describes one endpoint, and it now emits every metric's sample instead of only the classes sample. That is what keeps the RDF agreeing with the HTML once the HTML shows more than one.

The header comment at line 75 says the sample branch reads `sw:currentSampleRun`. Update it.

- [ ] **Step 4: Update `index_description.rq`**

Make the same change to its sample branch. Its header comment at line 24 describes the lookup, so update that too.

**Time this one.** `index_description.rq` is the query with a measured 28x cost regression in its history, and a mutation guard because of it. It is where an innocent-looking extra triple pattern has already cost the most in this codebase.

The new pointer adds two patterns where there was one. Time the query before and after, the way `web/tools/time_read_queries.py` does, against the committed store. Put both figures in the report.

If it got materially slower, say so rather than absorbing it.

- [ ] **Step 5: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected: all green. That is 342 plus whatever Tasks 1 to 4 added.

If `test_reader_golden.py` fails, read why before regenerating anything.

A golden file records what the readers produced at a known-good moment. This plan does legitimately change that output, so the golden does need recapturing, with `web/tools/capture_reader_golden.py`.

But recapture it in this order:
1. read the diff line by line
2. confirm every changed line is one this plan intended
3. then recapture, and put the diff in the report

A golden regenerated without reading it stops being evidence of anything.

- [ ] **Step 6: Commit**

```bash
git add web/queries/endpoint_description.rq web/queries/index_description.rq \
        web/tests/test_negotiation.py
git commit -m "Publish every metric's sample in the RDF, not only classes"
```

If you recaptured the golden, commit it on its own, with a message saying what changed in it and why. That keeps the recapture reviewable by itself.

---

## Verification of the whole plan

- [ ] `cd web && source .venv/bin/activate && python -m pytest -q` is green.
- [ ] `grep -rn 'currentSampleRun' web/ --include='*.py' --include='*.rq' | grep -v .venv` finds only comments explaining that the predicate was replaced. Nothing reads or writes it.
- [ ] `grep -rn 'metric:classes' web/queries/ web/*.py | grep -v .venv` finds only `CLASSES_METRIC` and its documented default. No query names it.
- [ ] Both samples survive a round trip. Load both fixtures into a scratch store, serve `/endpoint` for that endpoint as HTML and in all four RDF forms, and check each one shows both samples.
- [ ] No em-dash was introduced. The check cannot contain the character it looks for, so spell it: `git diff main | grep "$(printf '\u2014')"` is empty.

## What this plan does NOT do

- **It adds no metric and probes nothing.** The prober is untouched and `metrics.toml` still declares one sampling metric. The properties sample this plan makes visible exists only in a test fixture.
- **It does not show a second sample on the page.** Task 3 makes `endpoint_content` answer for any metric. What the endpoint page should display when there are two samples is a design question about the page, not about the pointer, so it belongs to the next plan.
- **It does not migrate a deployed store.** `load_run.py` says the `.nq` files are the source of truth and the store is derived from them, so migrating means reloading.

  Note in the final report that the live store at `~/code/sparqlwatch-runs/store.db` holds zero content samples today, so for this deployment the migration is empty in practice.
