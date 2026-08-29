# Metric-agnostic sample pointers implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the web tier read a content sample from ANY sampling metric, by keying the recency pointer on (endpoint, metric) instead of hardcoding `sw:metric:classes`.

**Architecture:** `sw:currentSampleRun`, one triple per endpoint, is replaced by a derived-IRI pointer resource carrying `sw:sampleRunFor`, `sw:sampleRunMetric` and `sw:sampleRunIs`. The write side in `web/load_run.py` maintains one such resource per (endpoint, metric) pair a run sampled; the four read queries hop through it with the metric either bound by substitution or left free. Nothing in the prober changes.

**Tech Stack:** Python 3.12 (`web/.venv`), pyoxigraph 0.5.9, pytest, FastAPI, SPARQL 1.1.

**Spec:** `docs/superpowers/specs/2026-08-29-content-profiles-design.md`, Ruling 3 and the section "The recency pointer has to become per metric". Its parent is `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md` section 1b.

## Why this is its own plan

The spec's other half, deriving property profiles in the prober, cannot be read by anything until this lands: a `properties` sample is written correctly by the prober today and is invisible to the web tier. This plan delivers working, testable software on its own, and the proof is an existing fixture, `web/tests/fixtures/run-properties-sample.nq`, which exists precisely to pin the current pin and which this plan makes visible.

## Global Constraints

- **Never use an em-dash** in any output: code, comments, docstrings, test names, commit messages, template copy. This is absolute and applies to every file this plan touches.
- **Never `git add -A`.** Stage files by name.
- Commit messages end with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- **The governing rule:** never report a confident wrong answer. A missing sample and a sample that found nothing are different answers and must stay distinguishable.
- **`urn:sparqlwatch:current` is derived and reconstructible from the run graphs alone.** Nothing in this plan may put a triple in it that no run graph supports, and `rebuild_current` must reproduce exactly what incremental loading produces.
- **`Store.update()` in pyoxigraph 0.5.9 does not accept `substitutions`.** Values reach an update as prefix declarations with an empty local name (`endpoint:`, `run:`). Adding a value means adding a prefix. `Store.query()` does accept SEP-0007 `substitutions`, and only projected variables can be substituted.
- **An update body carries no `PREFIX` prologue of its own.** pyoxigraph accepts a prologue only before the first operation, so bodies are combined by `_update_text`, which prepends one.
- **The test-fixture convention is one named pytest fixture per store scenario, and there is no generic loader.** `web/tests/conftest.py` declares a module-level `RUN_<NAME> = FIXTURES / "run-<name>.nq"` path constant and a `@pytest.fixture def store_<name>(tmp_path)` returning `_loaded_store(tmp_path, "store-<name>", RUN_<NAME>, ...)`. Every store a test needs is built that way, through `load_run()` and never through `Store.load()`, because a raw load leaves no derived `current` graph and so produces a store shape the deployment never has. A new scenario means a new constant and a new fixture, both added to `conftest.py`. Do not invent a `load_fixture` helper; an earlier draft of this plan did and every test in it was unrunnable.
- Run `python -m pytest -q` from `web/` with `.venv` activated. The suite is 342 tests before this plan and must stay green throughout.

## File structure

| File | Responsibility after this plan |
|---|---|
| `web/load_run.py` | Owns the pointer's identity (`_sample_pointer_iri`), its predicates, its per-pair maintenance, its drift detection and its rebuild. The only writer. |
| `web/queries/endpoint_content.rq` | One endpoint's sample for a GIVEN metric, substituted. No metric hardcoded. |
| `web/queries/endpoint_description.rq` | Same hop, for the RDF representation of one endpoint. |
| `web/queries/index_description.rq` | Same hop, for the index's RDF representation. |
| `web/app.py` | Passes the metric to `endpoint_content.rq`; its prose about `sw:currentSampleRun` is updated. |
| `web/tests/fixtures/run-two-metrics-sampled.nq` | NEW. Two sampling metrics in one run, so a pair-keyed pointer has something to be right about. |
| `web/tests/fixtures/run-properties-later.nq` | NEW. A later run sampling only `properties`, so the two pointers must name different runs. |


---

### Task 1: The pointer's identity and the write side

Replace the one-per-endpoint `sw:currentSampleRun` triple with one pointer resource per (endpoint, metric) pair, and make `load_run` maintain it.

**Files:**
- Modify: `web/load_run.py`. Removed: `CURRENT_SAMPLE_RUN`, `_SAMPLED_ENDPOINTS`. Added: the three `SAMPLE_RUN_*` predicates, `_sample_pointer_iri`, `_SAMPLED_PAIRS`, `_run_sampled_pairs`, `_pair_run`. Rewritten: `_POINTERS`, `_REPLACE_SAMPLED`, the sample half of `load_run`'s per-run loop, and the docstring's QUAD SHAPE and TWO POINTERS sections. `_endpoint_run` is left alone: the measured half still uses it unchanged.
- Create: `web/tests/fixtures/run-two-metrics-sampled.nq`
- Create: `web/tests/fixtures/run-properties-later.nq`
- Modify: `web/tests/conftest.py` (two path constants and three fixtures, per the convention in Global Constraints)
- Test: `web/tests/test_load_run.py`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces, for Tasks 2 to 4:
  - `SAMPLE_RUN_FOR = "urn:sparqlwatch:sampleRunFor"`
  - `SAMPLE_RUN_METRIC = "urn:sparqlwatch:sampleRunMetric"`
  - `SAMPLE_RUN_IS = "urn:sparqlwatch:sampleRunIs"`
  - `def _sample_pointer_iri(endpoint: str, metric: str) -> str`
  - `_run_sampled_pairs(store, run) -> set[tuple[str, str]]` replacing `_run_endpoints(store, run, _SAMPLED_ENDPOINTS)`
  - `_newest_per_endpoint` returns `(measured, sampled)` where `sampled` is now `dict[tuple[str, str], tuple[str, str]]` keyed by (endpoint, metric)

- [ ] **Step 1: Write the two fixtures**

`web/tests/fixtures/run-two-metrics-sampled.nq`. One run, one endpoint, two sampling metrics. Modelled on `run-properties-sample.nq`, which already uses `sw:metric:properties`, so the vocabulary is not new.

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

`web/tests/fixtures/run-properties-later.nq`. A STRICTLY LATER run that samples ONLY `properties` for the same endpoint. This is the fixture that makes the whole plan necessary: after loading both, the classes pointer must still name the earlier run while the properties pointer names this one. Copy the block above, change every `2026-01-01T00:00:00Z` to `2026-02-01T00:00:00Z`, delete the six `:classes` lines, and change the `sampledValue` to `<http://xmlns.com/foaf/0.1/mbox>`.

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

Add to `web/tests/test_load_run.py`, taking the fixtures declared above. `ENDPOINT` below is `http://example.org/sparql`, the endpoint the two new run files describe; use whatever spelling that file's own tests already use for a fixture endpoint if one is established.

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

`load_run_module` in the second test is however `test_load_run.py` already reaches the module's private names; check its imports and follow them rather than adding a second style. If it imports names directly, import `_sample_pointer_iri` directly.

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -k "pointer or metrics_sampled or currentsamplerun" -v`

Expected: FAIL. The first three with an empty result set or `ImportError` on `_sample_pointer_iri`; the fourth with `1 left`, because the current code writes exactly that triple.

- [ ] **Step 5: Add the pointer's identity and predicates**

In `web/load_run.py`, beside `CURRENT_RUN`, replace the `CURRENT_SAMPLE_RUN` constant with the three new predicates and the derivation. Delete `CURRENT_SAMPLE_RUN` entirely rather than leaving it unused.

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

In `load_run`, the sample half of the per-run loop currently reads `sampled = _run_endpoints(store, run, _SAMPLED_ENDPOINTS)` and keys `live` on `(endpoint, CURRENT_SAMPLE_RUN)`. It becomes:

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

Note the consequence and do not paper over it: the sample pointer no longer moves inside the same `store.update()` as the endpoint's measurements. It cannot, because a per-pair update needs a per-pair prefix binding. `_REPLACE_MEASURED` keeps its own single-call guarantee, so an endpoint's measurements are still never half-updated; what is no longer atomic is measurements-and-samples together. That is the same tradeoff `rebuild_current` already documents and accepts for its two units, and the drift detector in Task 2 is what finds a partial result.

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

`?which` is the metric IRI for a sample pointer and `sw:currentRun` for the run pointer, so the dict `_pointers` builds is keyed `(endpoint, CURRENT_RUN)` for one and `(endpoint, metric)` for the other with no further change to that function. A metric can never be spelled `urn:sparqlwatch:currentRun`, because `metrics.rs` restricts a metric id to `[a-z0-9][a-z0-9-]*` and prefixes it with `urn:sparqlwatch:metric:`, so the two key spaces cannot collide.

- [ ] **Step 9: Update the module docstring**

Two passages state the old shape and would now be wrong. In QUAD SHAPE, replace the `E sw:currentSampleRun <run>` line with:

```
  <ptr> sw:sampleRunFor E       one pointer resource per (E, metric) pair,
        sw:sampleRunMetric M    naming the newest run that published an
        sw:sampleRunIs <run>    M sample of E. See _sample_pointer_iri.
```

In TWO POINTERS, keep the existing argument and extend it, because the argument is what generalises:

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

Expected: the four new tests PASS. Other tests in this file that assert on `sw:currentSampleRun` will FAIL, and that is correct: they pin the shape being replaced. Update each to the new shape, preserving what it was checking. Do not delete a test to make the suite green; if a test's subject no longer exists, say so in its comment and re-point it at the equivalent fact.

- [ ] **Step 11: Run the whole suite**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected: Tasks 2 to 4 own the read side, so failures confined to `test_endpoint_content.py`, `test_reader_golden.py`, `test_negotiation.py` and `test_page.py` are expected here. Record which fail, and their count, in the task report. A failure anywhere else is in scope for this task.

- [ ] **Step 12: Commit**

```bash
git add web/load_run.py web/tests/test_load_run.py \
        web/tests/fixtures/run-two-metrics-sampled.nq \
        web/tests/fixtures/run-properties-later.nq
git commit -m "Key the sample pointer on the metric, not just the endpoint"
```

---

### Task 2: Drift detection and rebuild for the new shape

`load_run` reports a `current` graph that has drifted from its run graphs, and `rebuild_current` repairs one. Both know the old pointer shape and must learn the new one, keyed per pair, or a drifted sample pointer becomes undetectable and a rebuild silently produces a different graph from incremental loading.

**Files:**
- Modify: `web/load_run.py` (`_DRIFTED_SAMPLE_POINTERS`, `_newest_per_endpoint`, `rebuild_current`, and the verification block near line 910)
- Test: `web/tests/test_load_run.py`

**Interfaces:**
- Consumes from Task 1: `SAMPLE_RUN_FOR`, `SAMPLE_RUN_METRIC`, `SAMPLE_RUN_IS`, `_sample_pointer_iri`, `_run_sampled_pairs`, `_REPLACE_SAMPLED`, `_pair_run`.
- Produces: `_newest_per_endpoint` returning `sampled` keyed `(endpoint, metric)`; `LoadResult.drifted` entries naming the metric.

- [ ] **Step 1: Write the failing test**

Drift is not exposed as a public `detect_drift`. It is computed by the private
`_drifted(store)`, which unions the two drift queries and returns a sorted list
of ENDPOINT strings, and it reaches a caller through `LoadResult.drifted` and the
operator through `_drift_advice`. Do not add a public entry point for this
plan's convenience; call what exists.

The discriminating case is a run that sampled ONLY a non-classes metric. Under
the old shape no sample pointer was written for it at all, so there was nothing
to drift and nothing to detect. `run-properties-sample.nq` is exactly that run
and is already declared as `store_properties_sample`.

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

Add the helper to `web/tests/test_load_run.py` beside them, unless the file
already has an equivalent, in which case use that one:

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

Confirm the run IRI in the first test against the fixture file rather than
trusting the spelling above, and confirm `NamedNode` and `_drifted` are imported
in that module; add them to its existing import lines if not.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -k "drift_naming or reproduces" -v`

Expected: FAIL. The drift test returns `[]`, because after Task 1 a properties pointer exists but the detector's `FILTER NOT EXISTS` still asks whether the run sampled `sw:metric:classes`, which it never did, so the pointer looks satisfied. The rebuild test fails because `rebuild_current` still writes `sw:currentSampleRun` while loading now writes pointer resources, so the two graphs differ on every sample pointer.

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

The `FILTER NOT EXISTS` now joins on `?metric` rather than on a literal IRI, which is what makes it ask the right question: not "did this run sample classes" but "did this run sample the metric this pointer claims it did".

`_drifted` reads `row["endpoint"].value` from both queries and unions them, so adding a projected `?metric` column does not change its contract and it needs no edit. Leave it alone. The metric is projected because the `FILTER NOT EXISTS` has to JOIN on it, which is the substantive fix; having it available for a future, more specific message is a side benefit and not a reason to widen `LoadResult.drifted` now. Repair is per endpoint (rebuild the graph), so reporting the endpoint once is the right granularity even when two of its pointers drifted.

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

`_tie_message` takes an endpoint and two runs. A tie is now per pair, so its message should name the metric as well, or two different metrics tying on the same endpoint produce the same message twice and a reader cannot tell them apart. Extend the signature; it has one other caller and both are in this module.

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

The existing loop iterates `sorted(set(measured) | set(sampled))` and branches inside. That union no longer type-checks, because the two dicts are keyed differently. Two loops is also clearer about what it does, and the comment about the two units naming different runs moves to where it is now true.

- [ ] **Step 6: Generalise the post-load verification**

The block near line 910 asserts that every endpoint the run sampled has a pointer naming the expected run, and raises naming `sw:currentSampleRun` when it does not. Re-point it at the pair-keyed dict and at the new predicates, keeping both messages: the "no pointer at all" case and the "pointer names the wrong run" case are different failures and both messages should name the metric.

- [ ] **Step 7: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_load_run.py -v`

Expected: PASS, all of them. This file is the write side's whole test surface, so it must be green before the read side is touched.

- [ ] **Step 8: Commit**

```bash
git add web/load_run.py web/tests/test_load_run.py web/tests/conftest.py
git commit -m "Detect and repair a drifted sample pointer per metric"
```

---

### Task 3: `endpoint_content.rq` reads any metric's sample

The HTML path. `endpoint_content` currently answers "what classes did the newest sampling run find", with the metric hardcoded in the query. It becomes "what did the newest run that sampled THIS METRIC find", with the metric a parameter.

**Files:**
- Modify: `web/queries/endpoint_content.rq`
- Modify: `web/endpoint_content.py` (`endpoint_content`, `EndpointContent`)
- Modify: `web/app.py` (the one call site, and the prose naming `sw:currentSampleRun`)
- Test: `web/tests/test_endpoint_content.py`

**Interfaces:**
- Consumes from Task 1: the three pointer predicates.
- Produces: `endpoint_content(store, endpoint, metric="urn:sparqlwatch:metric:classes") -> EndpointContent`, and `EndpointContent.metric` carrying which metric the answer is about.

The default keeps every existing caller working unchanged and keeps this task's diff small; Task 4's callers pass the metric explicitly. A default is defensible here and not a hedge, because `classes` is the only sampling metric the shipped `metrics.toml` declares, so it is the answer to "the sample" until a second one ships.

- [ ] **Step 1: Write the failing test**

Add to `web/tests/test_endpoint_content.py`, using the fixtures the file already
declares plus the two Task 1 added. Check the attribute name for the sampled
values on `EndpointContent` before writing `content.values` and use whatever the
dataclass already calls it; do not rename it in this task.

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

In `web/queries/endpoint_content.rq`, the pointer hop becomes:

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

and inside the hop, the hardcoded filter becomes a join on the same variable:

```sparql
      ?sample sw:sampledFrom ?endpoint ;
              sw:sampleSize ?size ;
              sw:sampleTruncated ?truncated ;
              sw:sampledBy ?metric .
```

Delete the `FILTER (?sampledBy = sw:metric:classes)` and the `?sampledBy` variable with it. The comment above it explains why `sw:sampledBy` was a bound variable plus a FILTER rather than a bound object, and that reasoning still applies to `?metric`, which IS bound by substitution: keep the comment, re-pointed at `?metric`.

`?metric` must be added to the SELECT projection, because pyoxigraph only substitutes projected variables. That is the same reason `?endpoint` is projected and the header already says so; extend that sentence rather than adding a second one.

Update the header's "Most recent IS NOT COMPUTED HERE" paragraph: it names `sw:currentSampleRun` and describes a per-endpoint pointer. The measured figures in it are still valid and must not be touched, because they measure the hop shape and not the pointer's key.

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

Add `metric: str` to `EndpointContent` and set it on both return paths. Every existing construction of `EndpointContent` in the codebase and its tests needs the new field; find them all rather than relying on a default, because a default would let a caller publish a sample without saying what it is a sample of.

Update the tie message to name the metric, for the same reason Task 2 updated `_tie_message`.

- [ ] **Step 5: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_endpoint_content.py -v`

Expected: PASS. Existing tests in this file that assert the classes sample keep passing through the default.

- [ ] **Step 6: Update app.py's prose**

`web/app.py:359` explains the reader hop through `sw:currentSampleRun` in a comment or in page copy. Re-point it. If the text is user-facing, it must stay accurate about what the site does and not acquire jargon: prefer naming the fact ("the newest sweep that sampled this") over the predicate.

- [ ] **Step 7: Run the whole suite**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected: `test_load_run.py` and `test_endpoint_content.py` green. `test_reader_golden.py`, `test_negotiation.py` and `test_page.py` may still fail on the RDF path, which is Task 4. Record the count.

- [ ] **Step 8: Commit**

```bash
git add web/queries/endpoint_content.rq web/endpoint_content.py web/app.py \
        web/tests/test_endpoint_content.py
git commit -m "Read a content sample by metric instead of assuming classes"
```

---

### Task 4: The RDF representations read it too

`endpoint_description.rq` and `index_description.rq` are CONSTRUCTs serialised straight out of the store, and both hop through the old pointer. Until they are updated the HTML and the RDF for one endpoint disagree, which is exactly what `test_negotiation.py` exists to catch.

**Files:**
- Modify: `web/queries/endpoint_description.rq` (the sample branch, near line 255)
- Modify: `web/queries/index_description.rq` (the sample branch, near line 24)
- Test: `web/tests/test_negotiation.py`, `web/tests/test_reader_golden.py`

**Interfaces:**
- Consumes from Task 1: the three pointer predicates. From Task 3: the convention that a sample is identified by (endpoint, metric).

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

`_serve` above is a stand-in, NOT an API to add. Read `test_negotiation.py` first: it already has a way to obtain both representations of one endpoint and compare them, and this case belongs inside that comparison rather than beside it if it can express it. Use that helper and that file's own request convention verbatim. An earlier draft of this plan invented both a `load_fixture` fixture and a `client` fixture that do not exist, and every test using them was unrunnable; do not repeat it by inventing `_serve`.

The second assertion is the substantive one and is a genuine design question this task must settle: the pointer resource lives in the derived `current` graph, and the published RDF is about the endpoint. A CONSTRUCT that emitted `sw:sampleRunIs` would publish our internal bookkeeping as though it described somebody's server. It must not. Confirm the same is true of the old shape before assuming this is a regression: if `sw:currentSampleRun` was already excluded from both CONSTRUCTs, preserve that; if it leaked, say so in the report because that is a pre-existing defect this task can close.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && source .venv/bin/activate && python -m pytest tests/test_negotiation.py -v`

Expected: FAIL. Both CONSTRUCTs still match `sw:currentSampleRun`, which Task 1 stopped writing, so their sample branches bind nothing and the RDF carries no sample at all.

- [ ] **Step 3: Update `endpoint_description.rq`**

The sample branch near line 255 reads:

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

and the hop into `?sampleRun` joins its `sw:sampledBy` to `?sampleMetric` rather than to a literal IRI, exactly as Task 3 did for the SELECT.

This query is a CONSTRUCT, so unlike Task 3 it does not need `?sampleMetric` projected. It is a description of one endpoint and now emits every metric's sample rather than only the classes sample. That is the intended change and it is what makes the RDF agree with the HTML once the HTML shows more than one. The header comment at line 75 states the sample branch reads `sw:currentSampleRun`; update it.

- [ ] **Step 4: Update `index_description.rq`**

Same change to its sample branch. Its header comment at line 24 describes the hop and must be updated with it.

Check the query's cost commentary before and after: `index_description.rq` is the one with the measured 28x regression in its history and a mutation guard, so it is the query in this codebase where an innocuous-looking extra triple pattern has already cost the most. Time it against the committed store the way `web/tools/time_read_queries.py` does, and put the before and after figures in the task report. If the pointer's two extra patterns cost materially more than the one they replace, say so rather than absorbing it.

- [ ] **Step 5: Run the tests**

Run: `cd web && source .venv/bin/activate && python -m pytest -q`

Expected: all green, 342 plus the tests added across Tasks 1 to 4.

If `test_reader_golden.py` fails, read why before regenerating anything. A golden file records what the readers produced at a known-good moment; this plan legitimately changes that output, so the golden must be recaptured with `web/tools/capture_reader_golden.py`. Recapture it only after the diff has been read line by line and every changed line is one this plan intended, and put that diff in the report. A golden regenerated without reading it stops being evidence of anything.

- [ ] **Step 6: Commit**

```bash
git add web/queries/endpoint_description.rq web/queries/index_description.rq \
        web/tests/test_negotiation.py
git commit -m "Publish every metric's sample in the RDF, not only classes"
```

If the golden was recaptured, commit it separately with a message saying what changed in it and why, so the recapture is reviewable on its own.

---

## Verification of the whole plan

- [ ] `cd web && source .venv/bin/activate && python -m pytest -q` is green.
- [ ] `grep -rn 'currentSampleRun' web/ --include='*.py' --include='*.rq' | grep -v .venv` returns only historical commentary that says the predicate was replaced, and nothing that reads or writes it.
- [ ] `grep -rn 'metric:classes' web/queries/ web/*.py | grep -v .venv` returns only `CLASSES_METRIC`'s definition and its documented default. No query names it.
- [ ] The two-metric fixture round-trips: load both fixtures into a scratch store, serve `/endpoint` for the endpoint in HTML and in all four RDF forms, and confirm each shows both samples.
- [ ] No em-dash was introduced. The check cannot contain the character it looks for, so spell it: `git diff main | grep "$(printf '\u2014')"` is empty.

## What this plan does NOT do

- It adds no metric and probes nothing. The prober is untouched, and `metrics.toml` still declares one sampling metric. The properties sample this plan makes visible exists only in a test fixture.
- It does not render a second sample in the HTML template. Task 3 makes `endpoint_content` answer for any metric; deciding what the endpoint page shows when there are two is the next plan's, because it is a design question about the page and not about the pointer.
- It does not migrate a deployed store. `load_run.py` states the `.nq` files are the source of truth and the store is derived, so the migration is a reload. Note in the final report that the live store at `~/code/sparqlwatch-runs/store.db` holds zero content samples, so for this deployment the migration is empty in practice.
