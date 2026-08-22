# Stage 2b-1: Publish What We Sampled

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the class IRIs the `classes` metric already fetches and currently throws away, labelled as the bounded sample they are.

**Architecture:** No new probe and no new request. `classes` already runs a `SELECT DISTINCT ?c ... LIMIT 200` and uses the bindings only to decide a verdict. This slice carries those bindings out of the sweep and emits them as a content sample with an explicit truncation flag, following the side-fact pattern `DeclarationsRead` and `NotMeasured` already established.

**Tech Stack:** Rust 1.96, edition 2021. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`, whose metadata tier 2 is "bounded sampling: distinct classes, properties per class, counts, each under a hard cancellable budget, **explicitly marked sampled and incomplete**". This delivers the classes half of that, and the marking is the part that matters most.

## Why this exists now, out of the spec's order

The owner picked "understand what is in it before writing a query" as the first
task for the UI's front door. That task cannot be answered by today's output: a
`MeasurementRow` carries a verdict, a level and an elapsed time, so the UI could
say "this endpoint has classes: verified" and not say **which** classes. For a
task whose entire purpose is knowing what is in an endpoint, that is a page that
answers the question with "yes".

The spec orders stage 2b after 1d because 2b's value scales with a seeded
registry. That dependency is about volume, not capability: three endpoints are
enough to build and verify against, and the UI work is blocked without this.

## Design decisions, with their reasoning

**D1. Our own predicates, not VoID.** A content sample could be published as
`void:classPartition` and would be more interoperable that way. It is not, for
three reasons, in increasing order of weight:

1. What we have is an **observation from a bounded sample**, not a description of
   a dataset. The thing behind a SPARQL endpoint may be several datasets, or a
   virtual graph over a relational store, which is literally what `ontop` in our
   own `endpoints.toml` is.
2. This project's architecture is to store observations and derive views as
   queries, which is stage 2's whole thesis. A VoID projection is better as a
   later query over honest raw facts than as a lossy choice made at write time.
3. **The domains confirm it.** I could not retrieve the VoID vocabulary from four
   sources while writing this plan. The pre-execution review of this plan did,
   and reports that **`void:classPartition` and `void:class` both carry
   `rdfs:domain void:Dataset`**. That is attributed rather than independently
   verified by me, so treat it as strong evidence rather than settled fact and
   re-check if you rely on it for anything beyond this decision.

   Taken at face value it means reusing either term would entail, under RDFS,
   that a `ContentSample` **is** a `void:Dataset`, which it is not. That is
   exactly the defect this project already shipped once: the not-measured fact
   reused `dqv:computedOn` and `dqv:isMeasurementOf`, whose domains entailed 548
   declined pairs were quality measurements that never happened, and nothing was
   wrong until a consumer ran inference.

   So D1 is not caution in the absence of evidence. The evidence points the same
   way.

**D2. Truncation is a published fact, not an implementation detail.** A query with
`LIMIT 200` that returns exactly 200 rows tells us nothing about whether a 201st
exists. Publishing those 200 as "the classes" is the content equivalent of a
confident wrong answer. Fewer than the limit means we saw them all, for that
query's graph scope. So the sample carries both the count and whether it hit the
cap.

**D3. Only the enumerating metric publishes a sample.** `has-classes` is
`LIMIT 1`: it exists to answer "is there anything here" cheaply, and publishing
the single arbitrary class it happens to bind would be worse than publishing
nothing.

**D4. The cap lives in one place, enforced at load.** The sample cannot say
"truncated" without knowing the limit, and the limit is in the query text. Two
places that must agree will drift, so the load step validates them against each
other and fails loudly on a mismatch, in keeping with how this crate already
treats an unknown probe kind, an unknown cost, a missing `var` and a duplicate id.

**D5. Nothing new is needed to distinguish "not sampled" from "no classes".**
`classes` is `expensive`, so a default sweep declines it and already publishes a
`NotMeasured` fact saying why. The UI reads that. This is the cost class and the
not-measured record paying off rather than new machinery.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- `resolve()` stays a pure function. `src/emit.rs` is a pure function of its
  inputs, with **no clock read** and no randomness. `--at` never comes from the
  clock.
- **The six-verdict vocabulary is closed.** A content sample is not a verdict and
  must not add one, exactly as `not measured` did not.
- **Do not reuse a predicate whose domain is a class we are not.** See D1. If you
  believe a foreign vocabulary term fits, verify its domain and range from the
  real vocabulary and say so in your report; do not reason from the name.
- The probe dispatch in `lib.rs` has no `_` arm and `ProbeKind::ALL` is complete
  by construction. Keep both.
- Every test offline against local `wiremock`. Only `live_smoke` touches real
  endpoints and stays `#[ignore]`d.
- No em-dashes anywhere. The repository is free of them.
- After any experiment, restore the source and run `touch src/*.rs tests/*.rs`
  before the final test run, and **verify a mutation actually applied** (print or
  diff it) before concluding anything from it. Both failure modes have cost this
  project real time.
- A test you have not watched fail is not evidence.

## Starting state

`main` at the stage 1c-b2 merge. 247 tests pass, 2 ignored, clippy clean with
`--all-targets -- -D warnings`. The binary reads `endpoints.toml` and
`metrics.toml` relative to the working directory, so run it from `prober/`.

Counts you will need, which I verified rather than estimated:

- **20 `MetricDef` struct literals** (12 in `src/metrics.rs`, 2 `src/resolve.rs`,
  2 `tests/cors_preflight.rs`, 4 `tests/end_to_end.rs`). `MetricDef` derives no
  `Default` and must not gain one, so a new field has to be added at each.
- **23 `emit_nquads` call sites.** An earlier draft of this plan said 26, from a
  grep that counted lines rather than calls. Count them yourself.
- **29 `run_sweep` call sites** (1 `src/main.rs`, 27 `tests/end_to_end.rs`, 1
  `tests/live_smoke.rs`). The earlier draft did not mention these at all, which
  was the review's Critical finding: it specified that `emit_nquads` gains a
  parameter without ever saying how a `ContentSample` reaches it, and `run_sweep`
  currently returns a three-element tuple that every one of those 29 sites
  destructures.
- `definitions_revision` destructures `MetricDef` with **no `..`**, so adding a
  field fails to compile until somebody decides whether it belongs in the
  published revision. It does: it changes what we publish. Put it in, and let the
  per-field revision test cover it.

Verify all four with grep before you start; line counts drift.

---

## Task 1: A declared sample limit, checked against the query

**Files:**
- Modify: `prober/src/metrics.rs`
- Modify: `prober/src/resolve.rs`, `prober/tests/cors_preflight.rs`, `prober/tests/end_to_end.rs` (struct literals)
- Modify: `prober/metrics.toml`
- Test: unit tests in `prober/src/metrics.rs`

**Interfaces:**
- Produces: `MetricDef.sample_limit: Option<usize>`, `#[serde(default)]`. `Some(n)`
  means "this metric enumerates, publish up to n and tell me if you hit n".
  `None` means it publishes no sample.
- `load_metrics` rejects, with an error naming the metric:
  - a `sample_limit` whose value does not equal the `LIMIT` in the metric's query
  - a `sample_limit` on a metric whose query has no `LIMIT` at all
  - a `sample_limit` on a kind that reads no bindings (only `SelectIris` and
    `AskData` read them; see the existing `var` check for the precedent)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_metric_without_a_sample_limit_publishes_no_sample() {
    let defs = load_metrics(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n"
    ).unwrap();
    assert_eq!(defs[0].sample_limit, None);
}

#[test]
fn a_sample_limit_must_match_the_querys_limit() {
    // Two places that must agree will drift. The loader is where that is caught,
    // and a mismatch is a broken definition file, not something to guess about.
    let src = |lim: &str, q_lim: &str| format!(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
         sample_limit={lim}\nquery=\"SELECT DISTINCT ?c WHERE {{ ?s a ?c }} LIMIT {q_lim}\"\n"
    );
    assert!(load_metrics(&src("200", "200")).is_ok());
    let err = load_metrics(&src("200", "50")).unwrap_err().to_string();
    assert!(err.contains("m"), "the error must name the metric: {err}");
    assert!(err.contains("200") && err.contains("50"), "and both numbers: {err}");
}

#[test]
fn a_sample_limit_without_a_query_limit_is_a_load_error() {
    // An unbounded enumeration is not something we send to a stranger's server.
    assert!(load_metrics(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
         sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?s a ?c }\"\n"
    ).is_err());
}

#[test]
fn a_sample_limit_on_a_kind_that_reads_no_bindings_is_a_load_error() {
    // Liveness and Cors never populate `bindings`, so a sample limit on one is a
    // promise the probe cannot keep. Same doctrine as the `var` check.
    assert!(load_metrics(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\n\
         sample_limit=200\nquery=\"ASK{} LIMIT 200\"\n"
    ).is_err());
}

#[test]
fn sample_limit_is_part_of_the_definitions_revision() {
    let with = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
                sample_limit=200\nquery=\"SELECT ?c WHERE { ?s a ?c } LIMIT 200\"\n";
    let without = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
                   query=\"SELECT ?c WHERE { ?s a ?c } LIMIT 200\"\n";
    assert_ne!(
        definitions_revision(&load_metrics(with).unwrap()),
        definitions_revision(&load_metrics(without).unwrap()),
        "it changes what we publish, so it changes the revision"
    );
}
```

- [ ] **Step 2: Run the whole suite to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml` (not `--lib`: adding a field
to `MetricDef` breaks struct literals in the integration tests, and `--lib` hides
them).

- [ ] **Step 3: Implement**

The `LIMIT` check reads a number out of the query text. Keep it deliberately
simple and say so: a case-insensitive search for the last `LIMIT` followed by an
integer. This is our own hand-written `metrics.toml`, not arbitrary SPARQL, and a
mismatch is a load error rather than a silent default, so a crude matcher that
fails loudly is the right amount of machinery. Do not add a SPARQL parser.

Set `sample_limit = 200` on `classes` in `metrics.toml`, matching its existing
`LIMIT 200`, and on nothing else. `has-classes` is `LIMIT 1` and publishes no
sample (D3).

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each alone and each verified applied: default `sample_limit` to
`Some(200)`; drop the query-LIMIT comparison; drop the no-LIMIT check; drop the
kind check; remove `sample_limit` from the revision's canonical string. Restore,
`touch`, re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/metrics.toml prober/tests
git commit -m "feat(prober): a metric declares how many it will sample, checked against its query"
```

---

## Task 2: Return a `Sweep`, so the next side-fact costs one file

This task changes **no behaviour**. It exists because the review found that the
earlier draft never said how a `ContentSample` gets out of the sweep, and because
the answer should not be a fourth tuple element.

`run_sweep` returns `(Vec<MeasurementRow>, Vec<DeclarationsRead>, Vec<NotMeasured>)`.
That tuple has already grown from one element to two to three, and **each growth
edited all 29 call sites**. A content sample is the fourth side-fact, and the spec
names more coming: properties per class, counts, published examples. Growing the
tuple again buys nothing and pays the same 29-site cost every time.

**Files:**
- Modify: `prober/src/lib.rs`
- Modify: `prober/src/main.rs`, `prober/tests/end_to_end.rs`, `prober/tests/live_smoke.rs`
- Test: no new tests. The suite passing unchanged **is** the test, because a pure
  refactor that changes a verdict is a failed refactor.

**Interfaces:**
```rust
pub struct Sweep {
    pub rows: Vec<MeasurementRow>,
    pub declarations_read: Vec<DeclarationsRead>,
    pub not_measured: Vec<NotMeasured>,
}
```
`run_sweep(...) -> Sweep`. Task 3 adds `content_samples` as a field, touching no
caller.

- [ ] **Step 1: Count the call sites yourself**

Run `grep -rn "run_sweep(" prober/src prober/tests | grep -v "pub async fn"`.
Expect 29. Expect the suite to be red until the last one is converted; that is
normal for this task and not a signal to change approach.

- [ ] **Step 2: Convert**

Most call sites destructure and ignore two of the three, in the shape
`let (rows, _read, _nm) = run_sweep(...)`. Those become `let sweep = run_sweep(...)`
plus `sweep.rows`, or a destructuring `let Sweep { rows, .. } = ...` where that
reads better. Prefer whichever leaves the test's intent clearest, and do not
mechanically rewrite an assertion while you are in there.

- [ ] **Step 3: Prove nothing moved**

The suite must pass with **exactly** the counts it started at: 247 passed, 0
failed, 2 ignored. Not "green", the same numbers. If a count changes you have
added or lost a test, which this task must not do.

Then run a real sweep from `prober/` and diff its output against a run from
before the refactor, ignoring `elapsedMs` (wall-clock, so it differs by design):

```
cargo run -q -- --at 2026-08-22T11:00:00Z --out <scratchpad>/after.nq
```

Report whether the quad sets are identical apart from timings. A pure refactor
that changes the published graph is not a pure refactor.

- [ ] **Step 4: Commit**

```bash
git add prober/src prober/tests
git commit -m "refactor(prober): run_sweep returns a Sweep, not a growing tuple"
```

---

## Task 3: Carry the sample out, and publish it truthfully

**Files:**
- Modify: `prober/src/lib.rs` (retain the bindings, build the samples)
- Modify: `prober/src/emit.rs` (the type and the quads)
- Modify: `prober/src/main.rs` (pass it through)
- Modify: `prober/tests/end_to_end.rs` (call sites and new tests)
- Test: unit tests in `prober/src/emit.rs`, integration tests in `prober/tests/end_to_end.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct ContentSample {
      pub endpoint: String,
      pub metric_id: String,
      /// The IRIs the probe actually bound, in the order the endpoint returned
      /// them. Not sorted: the order is evidence about the endpoint, and sorting
      /// would discard it for a tidiness nobody asked for.
      pub values: Vec<String>,
      /// True when `values.len()` reached the metric's declared `sample_limit`,
      /// so a further value may exist and we did not see it.
      pub truncated: bool,
  }
  ```
- `Sweep` gains `pub content_samples: Vec<ContentSample>`, which is why Task 2
  came first: adding a field touches **no** `run_sweep` caller.
- `emit_nquads` gains `content_samples: &[ContentSample]`. **23 call sites**, and
  `main.rs` is among them: update every one in this task, so the bin target never
  sits non-compiling.
- Emitted per sample, in the run graph, using **sparqlwatch-owned predicates
  only** (D1):
  - `<sample-iri> rdf:type urn:sparqlwatch:ContentSample`
  - `<sample-iri> urn:sparqlwatch:sampledFrom <endpoint>`
  - `<sample-iri> urn:sparqlwatch:sampledBy <metric>`
  - `<sample-iri> urn:sparqlwatch:sampleTruncated "true"^^xsd:boolean`
  - `<sample-iri> urn:sparqlwatch:sampleSize "N"^^xsd:integer`
  - `<sample-iri> urn:sparqlwatch:sampledValue <each IRI>`
  - `<sample-iri> prov:wasGeneratedBy <activity>`

  Its own IRI shape, not the measurement counter and not the not-measured
  counter, and it must not share a subject with either.

- [ ] **Step 1: Write the failing tests**

```rust
// emit.rs unit tests
#[test]
fn a_content_sample_says_how_many_and_whether_it_hit_the_cap() {
    let s = ContentSample {
        endpoint: "http://example.org/sparql".into(),
        metric_id: "classes".into(),
        values: vec!["http://example.org/A".into(), "http://example.org/B".into()],
        truncated: false,
    };
    let nq = emit_nquads(&RunId(AT.into()), AT, REV, &[], &[], &[], Cost::Cheap, &[s]).unwrap();
    assert!(nq.contains("urn:sparqlwatch:ContentSample"));
    assert_eq!(nq.matches("urn:sparqlwatch:sampledValue").count(), 2);
    assert!(nq.contains(r#""2"^^<http://www.w3.org/2001/XMLSchema#integer>"#));
    assert!(nq.contains(r#""false"^^<http://www.w3.org/2001/XMLSchema#boolean>"#));
}

#[test]
fn a_content_sample_uses_no_foreign_vocabulary() {
    // The not-measured fact reused dqv predicates whose domains entailed it was
    // a quality measurement, which was the one defect on this project that no
    // test could catch because nothing breaks until a consumer runs inference.
    // A sample is an observation from a bounded query, not a dataset
    // description, so it borrows nothing it has not earned.
    let nq = emit_nquads(/* one sample */).unwrap();
    let subjects: std::collections::HashSet<&str> = nq.lines()
        .filter(|l| l.contains("urn:sparqlwatch:ContentSample"))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    for line in nq.lines() {
        let Some(s) = line.split_whitespace().next() else { continue };
        if !subjects.contains(s) { continue }
        let p = line.split_whitespace().nth(1).unwrap_or("");
        assert!(
            p.starts_with("<urn:sparqlwatch:")
                || p.contains("22-rdf-syntax-ns#type")
                || p.contains("ns/prov#wasGeneratedBy"),
            "a sample carries only our own predicates, rdf:type and prov: {p}"
        );
    }
}

#[test]
fn a_sample_never_shares_a_subject_with_a_measurement_or_a_not_measured_fact() {
    // Three fact types in one graph. If any two share an IRI, a consumer
    // joining on it gets a node that is several things at once.
    // Build one of each and assert the three subject sets are pairwise disjoint,
    // collecting each by its own rdf:type rather than by a substring.
}
```

```rust
// end_to_end.rs
#[tokio::test]
async fn the_classes_metric_publishes_the_iris_it_bound() {
    // Assert the VALUES and their ORDER, not the count. A test that checks only
    // "three sampled values" is passed by an implementation that publishes three
    // placeholders, and this project has shipped exactly that kind of test
    // before.
    const A: &str = "http://example.org/Zebra";
    const B: &str = "http://example.org/Apple";
    const C: &str = "http://example.org/Mango";
    // Deliberately not alphabetical: the endpoint's order is evidence, and an
    // implementation that sorts must fail this.
    let sample = /* sweep a mock returning A, B, C in that order */;
    assert_eq!(sample.values, vec![A.to_string(), B.to_string(), C.to_string()],
               "the IRIs the endpoint returned, in its order, not ours");
    // And the verdict has not moved: this slice adds a fact.
    assert_eq!(verdict_of("classes"), Verdict::Verified);
}

#[tokio::test]
async fn a_sample_that_fills_the_limit_is_marked_truncated() {
    // sample_limit is 200 in the shipped file, so drive this with a definition
    // whose limit is small (3) and a mock returning exactly 3. Truncated must be
    // true: we cannot know whether a fourth exists.
}

#[tokio::test]
async fn a_sample_under_the_limit_is_not_marked_truncated() {
    // Limit 3, two values returned. We saw them all, for that query's graph
    // scope, so `truncated` is false and a consumer may treat the list as
    // complete.
}

#[tokio::test]
async fn a_declined_metric_publishes_no_sample_and_still_says_why() {
    // At the default cost ceiling `classes` is declined, so there is no sample
    // and the existing NotMeasured fact is what tells a reader that the absence
    // is a choice rather than an empty endpoint. This is D5: the distinction is
    // already published and needs no new machinery.
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement**

In `lib.rs`, where the observation is resolved, keep `o.bindings` when the
metric declares a `sample_limit`, and build one `ContentSample` per (endpoint,
metric that declared one and produced bindings). Truncated is
`values.len() >= limit`, and `>=` rather than `==` is deliberate: an endpoint that
ignores `LIMIT` and returns more than the cap has still given us a sample we
cannot call complete, and `==` would call it complete.

**Pin that choice, because the earlier draft argued it in prose and tested it
nowhere.** A test where the probe returns MORE values than the declared limit must
report `truncated: true`; changing `>=` to `==` must fail it. Endpoints ignoring
`LIMIT` is not hypothetical, and the failure mode is the one this slice exists to
prevent: a list a reader believes is complete.

Emit in `emit.rs`, purely from the inputs. Do not sort, filter or deduplicate the
values: `SELECT DISTINCT` already deduplicated, and reordering discards evidence
about the endpoint.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations, each verified applied:

- always set `truncated: false`, and always set it `true`
- **change `>=` to `==`** in the truncation test (the over-limit test must fail)
- publish the values sorted (the order assertion must fail)
- publish placeholder IRIs instead of the bound ones (the value assertion must
  fail; if it does not, the test is checking counts and needs tightening)
- build the sample IRI from the measurement counter (the disjointness test must
  fail)
- emit `void:classPartition` instead of `sampledValue` (the foreign-vocabulary
  test must fail)

Restore after each, `touch`, re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/tests
git commit -m "feat(prober): publish the classes we sampled, and whether we saw them all"
```

---

## Task 4: Documentation, and a real sample

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`
- Modify: `tools/render-run.mjs`

- [ ] **Step 1: Sample a real endpoint**

Run, from `prober/`:

```
cargo run -q -- --at 2026-08-22T12:00:00Z --max-cost expensive --out <scratchpad>/sampled.nq
```

Report the actual class IRIs sampled per endpoint, how many, and whether each was
truncated. `kadaster` returned 59 distinct classes in an earlier live check, so it
should not be truncated at a limit of 200; `qlever`'s osm-planet enumeration
exceeded the request budget in an earlier run, so expect `classes` there to be
`indeterminate` with no sample, and say so rather than treating it as a failure of
this slice.

- [ ] **Step 2: Show it in the viewer**

`tools/render-run.mjs` renders a run as a standalone page. Add the sampled
classes to the endpoint's row or a detail block, and make the truncation visible:
a list a reader believes is complete when it is not is the whole failure this
slice guards against. If the honest presentation is "59 classes sampled, not
truncated" plus the list, that is enough; this is a read-only viewer, not the web
tier.

Verify by rendering the run from Step 1 and reading the output. Say what you
looked at.

- [ ] **Step 3: Document**

In `prober/README.md`: what a content sample is, the exact predicates, that the
values are the IRIs the endpoint returned in its own order, and what
`sampleTruncated` means. State plainly that a sample is **not** a dataset
description and that we deliberately do not publish VoID, with D1's reasoning in
short form.

Say that samples appear only under `--max-cost expensive`, and that at the
default ceiling the `NotMeasured` fact is what distinguishes "we did not look"
from "there is nothing there".

In the spec: mark metadata tier 2 as partly delivered, naming what is and is not
built. Classes are sampled; properties per class and counts are not. Do not mark
the tier delivered.

- [ ] **Step 4: Commit**

```bash
git add prober/README.md docs/superpowers/specs tools/render-run.mjs
git commit -m "docs: what a content sample is, and what it deliberately is not"
```

---

## Done criteria

- Three tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A sweep with `--max-cost expensive` publishes real class IRIs for at least one
  endpoint, with a truthful truncation flag.
- A default sweep publishes no samples and still explains why, through the
  existing `NotMeasured` fact.
- A content sample carries only sparqlwatch-owned predicates plus `rdf:type` and
  `prov:wasGeneratedBy`, and shares a subject IRI with nothing else.
- No verdict changed. This slice adds a fact.
