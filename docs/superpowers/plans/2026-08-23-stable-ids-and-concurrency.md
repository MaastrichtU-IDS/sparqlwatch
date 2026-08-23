# Stage 1c-b3: Stable Identifiers, Then Bounded Concurrency

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every published fact a subject derived from what it is about rather than from where it sat in a list, then sweep endpoints concurrently without losing the cancellation-safety, the politeness or the one-row-per-(endpoint, metric) invariant the sequential loop has today.

**Architecture:** Four changes in strict dependency order. A single subject-IRI helper keyed on (run, endpoint, metric), replacing three running-index `format!` sites. Then the per-endpoint accumulator moves into the per-endpoint unit of work, which is what lets that work move into a task **without** losing partial results when the endpoint budget drops the future. Then `run_sweep` drives up to `--concurrency` endpoints at once through a `JoinSet`, reassembling by input slot. Then a web-side fixture proving both subject schemes coexist in one store.

**Tech Stack:** Rust 1.96, edition 2021. tokio (`rt-multi-thread`, `macros`, `time`, `sync` are already declared at `Cargo.toml:11`; `JoinSet` and `Semaphore` need nothing new), reqwest 0.13, oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6.5, clap 4. Python 3.12 + pyoxigraph for Task 4.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## This plan was rewritten after review. What changed and why

The first draft was reviewed before execution and found to contain **4 Critical and
18 Important defects**. The review is at
`.superpowers/reviews/2026-08-23-plan-1c-b3.md` and is worth reading alongside this
plan. Four of its findings changed the design rather than the wording, and they are
stated here because an implementer who does not know them will reintroduce them:

1. **The `&mut` out-parameters in `probe_endpoint` are not a style accident. They are
   the mechanism that makes partial results survive cancellation.** `run_sweep` wraps
   the call in `budget.with_endpoint_budget`, which is `tokio::time::timeout`, which
   **drops** the future on expiry. A future that returns its results returns nothing
   when dropped. The first draft's "pure refactor" to a return value would have
   destroyed every measured row, every collected sample and the true
   `declarationsRead` for any endpoint whose 600s budget expired, which is the normal
   case for a black-holed host. `prober/tests/end_to_end.rs:577` fails on exactly
   that. The obstacle to spawning was never `&mut`; it was who **owns** the borrow.
   So Task 2 moves the accumulator's ownership into the unit of work and keeps the
   writes exactly where they are.
2. **A panicked endpoint task must still occupy its slot with facts.** Keeping the
   other 547 endpoints is right, but an endpoint contributing zero quads breaks the
   one-row-per-(endpoint, metric) invariant, and `web/queries/endpoint_measurements.rq`
   then keeps showing last night's verdicts as current with nothing saying this run
   failed. That is decisive defect 3 from the superseded 1c-b plan, restated.
3. **`--concurrency` and `--min-gap-ms` are not independent knobs.**
   `Politeness::acquire` sleeps the min gap **while holding the per-host lock**, so
   `k` endpoints on one host each wait up to `(k-1) x min_gap` extra, and all of it is
   spent inside the 60s metric budget. A `>= 1` check is not the constraint that
   matters; the relation `(c - 1) x gap + gap + request < metric` is, and it is the
   same relation `validate_min_gap` already exists to enforce.
4. **Deriving the subject is what decouples dispatch order from output order**, and the
   first draft did not use that. Tasks spawn in input order, `Semaphore` is FIFO, and
   a registry seeded from LOD Cloud plus YummyData is clustered by publisher, so the
   four in-flight endpoints are routinely four entries on one host, all serialising on
   one lock while 544 wait.

The rewrite was reviewed again, and one Critical plus nine Important findings survived
it. The decisive one changed the concurrency model:

5. **The unit of concurrency is a host, not an endpoint.** The first rewrite validated
   `--concurrency` against `(c - 1) x min_gap + gap + request < metric`, on the model
   that a competitor holds the per-host lock for its gap. It does not: `gated_hop`
   holds the guard until the request returns, so a competitor holds it for its gap plus
   its whole request, and on a throttled host for gap plus request plus the honoured
   `Retry-After` plus the retry. `main.rs:46-63` already states that hold as
   arithmetic, worst case `2 + 30 + 20 + 30 = 82`. At concurrency 4 that is up to 246
   seconds of lock-waiting inside a 60-second metric budget, so the validator would
   have certified 4 as safe when **no value above 1 is** on a shared host, and the
   symptom is silent: a cancelled metric budget reports `indeterminate` with no
   warning, converting exactly the slow-but-measurable endpoints a quality monitor
   exists to characterise. Round-robin dispatch does not bound this either, because
   endpoint durations range from 14s to 600s so the in-flight set drifts, and 548 URLs
   sit on far fewer than 548 hosts.

   So the invariant becomes structural rather than arithmetic: **one endpoint per host
   in flight**, achieved by grouping the endpoint list by `politeness::host_key` and
   giving each group one sequential task, with `--concurrency` bounding how many groups
   run at once. Then no endpoint ever waits on another endpoint's host lock, the metric
   budget sees exactly what it sees today, "concurrency only buys parallelism across
   different hosts" becomes true by construction rather than by hope, and no new budget
   relation is needed at all.

## Why this order, and why identifiers come first

Stage 1c-b4 writes output incrementally so a crash at endpoint 500 does not lose 499
endpoints' work. It cannot be built on today's identifiers: `emit.rs:236` builds a
measurement subject from `i`, the row's index in the whole run, so per-endpoint chunks
each restart at `:0` and different endpoints' measurements collapse onto one node.
That was found by the review of the superseded 1c-b plan and is recorded in its header.

Concurrency has the same dependency for a different reason. Today the row order is the
endpoint order because the loop is sequential, so `i` is stable across two runs of the
same list. Under concurrency the completion order varies, so an index-derived subject
would give the same probe a different IRI on every run, and a consumer diffing two runs
would see every measurement replaced.

## What "deterministic output" does and does not mean

The superseded plan's Done criterion was "two identical runs produce byte-identical
output". That is impossible: `emit.rs` publishes `elapsedMs`, a wall-clock measurement,
so no two runs of the same definitions ever produce the same bytes.

Two narrower properties replace it, and they have **different standing**:

- **Identity, which is a property of the graph and is what 1c-b4 relies on.** Every
  subject is a pure function of (run, endpoint, metric), so the same probe in the same
  run has the same IRI no matter when it ran, what else was swept, or how the endpoint
  list was ordered.
- **Order, which is a property of one emitted file and of nothing else.** Quads appear
  grouped by endpoint in input-list order, and within an endpoint by metric-definition
  order. This exists so that `git diff` of two run files is readable. N-Quads order
  carries no meaning, and `web/load_run.py` inserts into Oxigraph, which is
  order-blind, so no consumer depends on it. **Stage 1c-b4 is expected to break the
  order property**, because writing each endpoint's chunk as it completes is writing
  in completion order. Do not document order as a guarantee of the data.

Neither claims byte-identity. No comment, README sentence or test name may imply it.

## The spec requirement this stage does NOT meet

The spec's "Reliability and cost control" list requires:

> **Per-endpoint isolation.** One endpoint's failure or slowness must never delay
> another's results or block the run from finalizing. Results are written per endpoint
> as they complete.

This stage does not satisfy the second sentence. `run_sweep` returns after the slowest
task finishes and buffers the whole `Sweep`, so one endpoint burning its 600s budget
still delays the run's output by up to 600s. Concurrency shrinks the constant; it does
not change the shape. **Satisfying that sentence is stage 1c-b4's job**, and Task 5
must say so in the spec row rather than claiming the bullet. What this stage does
deliver against the first sentence is that a slow endpoint no longer delays other
endpoints' *probing*, which the sequential loop does today.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- CI runs with `-D warnings`. An unused variable or import is a build failure.
- **Never report a confident wrong answer.** The six-verdict vocabulary is closed
  (`verified`, `undeclared-but-verified`, `declared-but-wrong`, `declared-only`,
  `absent`, `indeterminate`) plus the non-verdict `NotMeasured` and `ContentSample`
  facts. This stage changes subjects and ordering; it must change **no verdict, no
  predicate and no object**.
- Run graphs are **immutable and append-only**. A published identifier can never be
  corrected, so an identifier defect is permanent in a way a rendering defect is not.
- All judgement stays in `resolve.rs`. Nothing here computes a verdict, including the
  panic path: it routes through `resolve(def, Declared { claimed: false }, Err(Expired))`.
- **The endpoint string goes into the subject verbatim.** No lowercasing, no
  trailing-slash normalisation, no post-redirect `final_url`. `dqv:computedOn`
  publishes the registry string (`emit.rs:258`), so a normalised subject would name
  the endpoint two ways in one node; and `registry::dedupe` rules deliberately that
  `http://x/sparql` and `http://X/sparql` are two entries
  (`registry.rs:130-145`, `a_near_duplicate_is_two_entries_not_one`), so lowercasing
  inside the encoder would merge two entries the registry keeps apart.
- **No em-dashes** anywhere: code, comments, tests, docs or commit messages.
- Every comment and doc sentence must be defensible by pointing at a line of code.
- Commit in logical steps, staging by name. Never `git add -A`.

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `prober/src/emit.rs` | modify | Gains `subject_iri` and `FactKind`, the one place a run-scoped subject is built, plus a duplicate-subject guard. Three sites call it: `:236`, `:319`, `:462`. Note there is a **fourth** fact loop, `declarations_read` at `:540`, whose subject is the endpoint IRI itself and is deliberately not run-scoped; it is not changing. |
| `prober/src/metrics.rs` | modify | Validates a metric `id` against `[a-z0-9][a-z0-9-]*` at load, joining the existing duplicate-id load error. |
| `prober/src/registry.rs` | modify | Drops an endpoint whose URL carries userinfo, with a warning, matching how `dedupe` drops duplicates. Its `dedupe` doc gains a sentence saying the subject scheme now depends on it. |
| `prober/src/lib.rs` | modify | The accumulator's owner moves; `probe_one_endpoint` is extracted with the expiry fill; `run_sweep` gains bounded concurrency and slot reassembly. Two false doc blocks corrected. |
| `prober/src/main.rs` | modify | `--concurrency` as `NonZeroUsize`, documented as a count of hosts. Non-zero exit when a task failed. A note beside `validate_min_gap` saying why no second budget relation is needed. |
| `prober/tests/end_to_end.rs` | modify | 32 `run_sweep` call sites take the new signature. |
| `prober/tests/live_smoke.rs` | modify | The 33rd call site. Its tests are `#[ignore]`d but still compile under `-D warnings`, so missing it leaves the build red. |
| `web/tests/fixtures/run-new-subjects.nq` | create | A run in the new subject scheme, so the web layer is not tested exclusively against a shape the prober can no longer produce. |
| `prober/README.md`, spec | modify | The flag, the two properties with their different standing, and what 1c-b4 still owes. |

---

### Task 1: One subject, derived from what it is about

**Files:**
- Modify: `prober/src/emit.rs` (add `subject_iri`, `FactKind`, the duplicate guard; call sites `:236`, `:319`, `:462`)
- Modify: `prober/src/metrics.rs` (validate `id` at load)
- Modify: `prober/src/registry.rs` (drop a userinfo URL; one doc sentence on `dedupe`)
- Test: the unit tests already in each of those files

**Interfaces:**
- Produces: `fn subject_iri(kind: FactKind, run: &RunId, endpoint: &str, metric_id: &str) -> anyhow::Result<NamedNode>` and `enum FactKind { Measurement, NotMeasured, ContentSample }`, both private to `emit.rs`.
- Consumes: `RunId` (`run.0`), `MeasurementRow`, `NotMeasured`, `ContentSample`.

**The shape**

```
urn:sparqlwatch:measurement:<run>:<percent-encoded endpoint>:<metric id>
urn:sparqlwatch:not-measured:<run>:<percent-encoded endpoint>:<metric id>
urn:sparqlwatch:content-sample:<run>:<percent-encoded endpoint>:<metric id>
```

The endpoint is percent-encoded keeping RFC 3986's unreserved set
(`ALPHA / DIGIT / "-" / "." / "_" / "~"`) and writing every other byte as `%XX` with
uppercase hex, over the UTF-8 bytes.

**Why this and not the two alternatives.** The deciding requirement is stage 1c-b4:
a subject must be computable from **one endpoint's own chunk**, with no knowledge of
the rest of the run. That rules out every global-rank scheme, including an opaque rank
over sorted (endpoint, metric), which would otherwise be shorter and would leak
nothing. It also rules out a non-cryptographic hash for a second reason: stage 5
accepts public submissions, so a submitter could deliberately craft a colliding URL
and merge their measurements onto someone else's subject, and `std` has no hash
documented as stable across releases to publish instead.

**The cost, which must be written down rather than glossed.** The endpoint URL becomes
a permanent, reversible part of every subject of every fact about it, in a graph this
project's own rules say can never be corrected. A URL carrying userinfo or an API key
in its query string would be published forever. `politeness.rs`'s `host_key` already
reasons that "userinfo is credentials, not identity" but nothing refuses such a URL,
which is why this task adds that refusal at registry load. An API key in a query
string remains a known, unmitigated exposure: record it in `prober/README.md`'s Known
limitations rather than leaving it undiscovered.

**Two failure policies, and why they are the same policy.** A metric id that reaches
emission with a colon in it, and a subject that appears twice, are both handled the way
`emit.rs:224` handles a junk endpoint: **skip the fact, warn, keep the rest of the
run**. Emission happens after all the probing, so returning `Err` would turn hours of
work into no output, and `main.rs:192-202` propagates an `Err` before
`std::fs::write`. So `subject_iri`'s `Result` is consumed at the call site with a
`continue`, never propagated with `?`.

For a duplicate the skip must drop the **whole subject**, not just the later row. The
junk-endpoint precedent does not say "keep one of two conflicting facts"; it says
publish nothing about what you cannot state correctly, and put the loss in the log.
Keeping the first of a `verified` and an `absent` observation would have the graph
assert `verified`, with full confidence, about a pair we also measured as `absent`:
a confident wrong answer produced by the guard that exists to prevent one. So: pre-scan
to count subjects, emit a subject that appears once, and for a subject that appears more
than once emit **nothing** while logging every dropped verdict at `warn`. An endpoint
with no measurement for a metric is a shape `web/queries/*.rq` already handles routinely.
A repeat that is byte-identical is a genuine no-op (RDF is a set) and may simply be
skipped.

**Injectivity, and the part of it the function cannot provide.** Right-splitting is
unambiguous even with a colon-bearing run id, because an encoded endpoint contains no
`:` and a validated metric id contains none either. Percent-encoding is injective on
strings, so `x:y` and `x%3Ay` stay distinct. But injectivity of the *function* says
nothing about the *input*: nothing guarantees that two entries of `rows` do not carry
the same (endpoint, metric). Under the old running index they got two subjects; under
this scheme they land on one node, and if their verdicts differ that node carries two
`dqv:value` literals, permanently. So this task adds a guard at emission, and
`subject_iri` checks the metric-id precondition it relies on rather than trusting a
check two modules away.

- [ ] **Step 1: Write the failing tests**

Locate the existing helpers first (`emit.rs`'s tests already build rows and a run id;
`metrics.rs` has the real loader entry point, which is `load_metrics`, not
`load_from_str`) and write against those exact names. Do not invent helpers.

```rust
#[test]
fn an_emitted_subject_is_the_one_the_helper_builds() {
    // Ties emit's output to the helper, so a parameter-order mistake in a
    // four-argument function cannot pass. Two rows, two endpoints, two metrics,
    // so endpoint and metric cannot be swapped without the assertion moving.
    let rows = vec![row("https://a.example/sparql", "cors"),
                    row("https://b.example/sparql", "classes")];
    let quads = emit_for(&rows);
    let expected = subject_iri(FactKind::Measurement, &run(), "https://a.example/sparql", "cors").unwrap();
    assert!(subjects_of(&quads).contains(&expected), "emit did not use subject_iri");
}

#[test]
fn a_subject_does_not_depend_on_where_its_row_sat() {
    // The property 1c-b4 needs: an endpoint's chunk can be emitted alone and
    // still name the node the full run would.
    let rows = vec![row("https://a.example/sparql", "cors"),
                    row("https://b.example/sparql", "classes")];
    let reversed: Vec<_> = rows.iter().cloned().rev().collect();
    let a = subjects_of(&emit_for(&rows));
    let b = subjects_of(&emit_for(&reversed));
    assert_eq!(sorted(a), sorted(b));
}

#[test]
fn the_encoding_is_exactly_this() {
    // Pins uppercase hex, the unreserved set, and UTF-8-byte-wise encoding.
    // Without an exact string this test cannot fail: the encoder's output is
    // valid IRI syntax by construction.
    let s = subject_iri(FactKind::Measurement, &RunId("R".into()), "http://a.example/p q\u{00e9}", "cors").unwrap();
    assert_eq!(s.as_str(),
        "urn:sparqlwatch:measurement:R:http%3A%2F%2Fa.example%2Fp%20q%C3%A9:cors");
}

#[test]
fn two_endpoints_differing_only_in_an_escape_get_different_subjects() {
    let one = subject_iri(FactKind::Measurement, &run(), "http://a.example/x:y", "cors").unwrap();
    let two = subject_iri(FactKind::Measurement, &run(), "http://a.example/x%3Ay", "cors").unwrap();
    assert_ne!(one, two);
}

#[test]
fn an_endpoint_cannot_shift_the_metric_field() {
    let a = subject_iri(FactKind::Measurement, &run(), "http://a.example/x", "cors").unwrap();
    let b = subject_iri(FactKind::Measurement, &run(), "http://a.example/x:cors", "cors").unwrap();
    assert_ne!(a, b);
    assert!(a.as_str().ends_with(":cors"));
}

#[test]
fn the_three_kinds_never_share_a_subject() {
    // A measurement and a not-measured fact about one pair must not land on one
    // node: it would both have and not have a verdict.
    let m = subject_iri(FactKind::Measurement, &run(), "http://a.example/x", "classes").unwrap();
    let n = subject_iri(FactKind::NotMeasured, &run(), "http://a.example/x", "classes").unwrap();
    let s = subject_iri(FactKind::ContentSample, &run(), "http://a.example/x", "classes").unwrap();
    assert_ne!(m, n); assert_ne!(m, s); assert_ne!(n, s);
}

#[test]
fn a_metric_id_that_would_make_a_subject_ambiguous_is_refused_by_the_helper() {
    // Enforced where it is relied on, not only where it is convenient.
    assert!(subject_iri(FactKind::Measurement, &run(), "http://a.example/x", "has:classes").is_err());
}

#[test]
fn a_conflicting_repeated_pair_publishes_nothing_about_that_pair() {
    // The collision the derived scheme makes possible and the old index hid.
    // ZERO values, not one: publishing the first of two contradictory
    // observations would assert `verified` about a pair also measured `absent`.
    let rows = vec![row_with_verdict("https://a.example/sparql", "cors", Verdict::Verified),
                    row_with_verdict("https://a.example/sparql", "cors", Verdict::Absent)];
    let quads = emit_for(&rows);
    let values: Vec<_> = quads.iter()
        .filter(|q| q.predicate.as_str().ends_with("value"))
        .collect();
    assert!(values.is_empty(), "published a verdict for a contradicted pair: {values:?}");
}

#[test]
fn a_metric_id_that_cannot_be_a_subject_costs_one_fact_not_the_run() {
    // The same policy as a junk endpoint: skip, warn, keep the run. A `?` here
    // would discard the whole sweep's output at main.rs:192-202.
    let rows = vec![row("https://a.example/sparql", "has:classes"),
                    row("https://b.example/sparql", "cors")];
    let out = emit_for(&rows);
    assert!(out.is_ok());
    // and b.example's measurement is present
}
```

**The central proof, and it must be written before any implementation.** The Global
Constraint is that no predicate and no object changed. Do not try to emit under both
schemes: that needs the old emission *walk*, not just the old format string, which means
duplicating `emit_nquads` inside the test where it will rot. The quantity being compared
does not depend on subjects at all, so freeze it:

```rust
// Written in Step 1, against the CURRENT code, before anything changes.
#[test]
fn only_the_subjects_changed() {
    // Baseline frozen from the pre-1c-b3 emitter on 2026-08-23: the sorted
    // (predicate, object) multiset and the quad count for the synthesized Sweep
    // below. Subjects are deliberately not part of it, because subjects are the
    // one thing this stage changes.
    const BASELINE_PAIRS: &[(&str, &str)] = &[ /* filled in from the current run */ ];
    const BASELINE_QUADS: usize = /* filled in */;
    // assert the new emission reproduces both exactly
}
```

In `metrics.rs`, using the real schema (`MetricDef` has `label`, `dimension` and
`kind`; `kind` values are PascalCase; see `prober/metrics.toml:5-11`) and asserting on
a message specific enough that a missing-field rejection cannot satisfy it:

```rust
#[test]
fn a_metric_id_with_a_colon_is_a_load_error() {
    let err = load_metrics(/* a definition identical to a working one except id = "has:classes" */).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("has:classes"), "did not name the offending id: {msg}");
    assert!(msg.contains("a-z"), "did not say what is allowed: {msg}");
}
```

In `registry.rs`, using that file's own test helpers (`v(&[...])` with `dedupe`, or
`load_endpoints` on TOML text) rather than a `load` that does not exist:

```rust
#[test]
fn an_endpoint_carrying_credentials_is_dropped_with_a_warning() {
    // Once the subject embeds the URL reversibly, admitting this publishes
    // credentials permanently. Dropped with a warning, matching how `dedupe`
    // handles a duplicate rather than failing the whole load.
    // => keeps only b.example
}

#[test]
fn an_at_sign_outside_the_authority_is_not_credentials() {
    // http://a.example/sparql?contact=x@y.example is a legitimate endpoint. A
    // `contains('@')` check would silently remove it from every sweep.
    // Userinfo is a property of the AUTHORITY, and politeness::host_key already
    // parses it correctly (lowercase, strip the scheme, take up to the first
    // `/`, `?` or `#`, then rfind('@')). Extract that step and share it rather
    // than writing a second parse.
}
```

- [ ] **Step 2: Run them and watch them fail**

Record what each printed. `subject_iri` and `FactKind` are undefined; the metrics test
loads a bad id; the registry test keeps the credentialed URL; the duplicate-pair test
sees two `dqv:value` objects.

- [ ] **Step 3: Implement**

```rust
/// Which kind of fact a subject names. The three kinds share a key, so they must
/// not share an IRI: a measurement and a not-measured fact about one pair would
/// otherwise land on a node that both has and has not a verdict.
enum FactKind { Measurement, NotMeasured, ContentSample }

impl FactKind {
    fn prefix(&self) -> &'static str {
        match self {
            FactKind::Measurement => "measurement",
            FactKind::NotMeasured => "not-measured",
            FactKind::ContentSample => "content-sample",
        }
    }
}

/// Percent-encode keeping RFC 3986's unreserved set, over UTF-8 bytes.
///
/// The output holds no `:`, which is half of what makes `subject_iri`
/// injective. No normalisation of any kind happens here: see this plan's
/// Global Constraints and `registry.rs:130-145` for why two endpoint strings
/// differing only in case are two subjects.
fn encode_unreserved(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' =>
                out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The subject of one published run-scoped fact, derived from what the fact is
/// about rather than from where its row sat.
///
/// Derived so that an endpoint's facts can be written on their own (stage
/// 1c-b4) and so two runs can be diffed. No consumer needs to parse it: a
/// measurement carries `dqv:computedOn` and `dqv:isMeasurementOf`, a sample
/// carries `sw:sampledFrom` and `sw:sampledBy`, and a not-measured fact carries
/// `sw:notMeasuredOn` and `sw:notMeasuredMetric` (`emit.rs:330` onward).
///
/// Rejects a metric id outside `[a-z0-9][a-z0-9-]*`. `load_metrics` refuses one
/// too, and the duplication is deliberate: this function's injectivity depends
/// on the invariant, so it checks it rather than trusting a check two modules
/// away. A validated `MetricId` newtype would make the check unnecessary and is
/// the better long-term shape; it is deferred because it touches every module
/// that names a metric.
fn subject_iri(kind: FactKind, run: &RunId, endpoint: &str, metric_id: &str)
    -> anyhow::Result<NamedNode> { /* ... */ }
```

At each of the three sites, drop the `enumerate()` **only if the loop uses `i` for
nothing else**: read each loop first. `-D warnings` catches a newly unused index.

The duplicate guard, as decided above: pre-scan the fact lists to count subjects, then
emit. A subject seen once emits normally; a subject seen more than once emits nothing
and logs every dropped verdict at `warn`. Say in the comment that `registry::dedupe` is
what makes this a belt-and-braces check rather than a routine path, and add the
reciprocal sentence to `dedupe`'s doc so the two cannot drift apart.

- [ ] **Step 4: Run the suite, then update the three hard-coded test IRIs**

`emit.rs:982`, `:1041` and `:1403` hard-code `urn:sparqlwatch:not-measured:r1:0` and
`urn:sparqlwatch:content-sample:r1:0`. Rebuild each expected subject with
`subject_iri` so those tests keep testing predicates rather than string shapes.

- [ ] **Step 5: Commit**

```bash
git add prober/src/emit.rs prober/src/metrics.rs prober/src/registry.rs
git commit -m "Derive a fact's subject from the fact, not from its row index"
```

---

### Task 2: Move the accumulator, not the writes

**Files:**
- Modify: `prober/src/lib.rs` (`probe_endpoint`'s parameters, new `probe_one_endpoint`, the two false doc blocks)
- Test: `prober/tests/end_to_end.rs`

**Interfaces:**
- Produces: `#[derive(Default)] struct EndpointSweep { rows: Vec<MeasurementRow>, declarations_read: bool, content_samples: Vec<ContentSample> }` and
  `async fn probe_one_endpoint(ep: &str, defs: &[MetricDef], client: &Client, budget: Budget) -> EndpointSweep`, which owns the accumulator, calls `probe_endpoint` through the endpoint budget, and applies the expiry fill.
- `probe_endpoint` keeps writing through a `&mut EndpointSweep`. **Its writes do not move.**

**The invariant this task must preserve, stated so it cannot be lost**

`budget.with_endpoint_budget` is `tokio::time::timeout`; on expiry the future is
dropped. Anything a dropped future was going to return is gone. So the accumulator
must be owned by the caller of the timeout, not by the future inside it:

```rust
async fn probe_one_endpoint(ep: &str, defs: &[MetricDef], client: &Client, budget: Budget)
    -> EndpointSweep
{
    let mut acc = EndpointSweep::default();
    let outcome = budget
        .with_endpoint_budget(probe_endpoint(ep, defs, client, budget, &mut acc))
        .await;
    if outcome.is_err() {
        // Unchanged from lib.rs:94-111: warn, then fill the metrics never
        // reached with verdicts from `resolve`, because judgement belongs in
        // one place and `resolve` already maps an expired budget to
        // Indeterminate.
    }
    acc
}
```

That is `'static`-friendly (the borrow is created and consumed inside the async block
Task 3 spawns) and it keeps every partial row, every collected sample and a truthful
`declarationsRead`.

- [ ] **Step 1: Write the failing test**

The two existing tests that bear on this are `end_to_end.rs:514`
(`an_endpoint_budget_expiry_still_yields_one_row_per_metric`) and `:577`
(`a_budget_expiry_after_the_fetch_still_publishes_declarations_read`). Only the second
is load-bearing here: in the first, all rows are `Indeterminate` either way, so it
passes while the loss happens. Neither covers the case that matters most, so write it:

```rust
#[tokio::test]
async fn a_partial_endpoint_keeps_the_verdicts_it_already_earned() {
    // Some metrics complete, then the endpoint budget expires. The completed
    // verdicts and their elapsedMs must survive, and so must any sample
    // already collected. Nothing in the suite covers this today, and it is
    // exactly what a return-value refactor silently destroys.
}
```

Build it from the mock the neighbouring expiry tests use: a server that answers the
first few requests promptly and then stalls past the endpoint budget, with a shortened
budget so the test is fast. Assert at least one non-`Indeterminate` verdict with a
`Some(elapsed_ms)` survives, and that `declarations_read` is `true`.

- [ ] **Step 2: Run it and watch it fail** (it describes behaviour nothing pins yet)

- [ ] **Step 3: Do the move**

Introduce `EndpointSweep`, give `probe_endpoint` a single `&mut EndpointSweep` in place
of its three out-parameters, and extract `probe_one_endpoint` with the expiry fill from
`lib.rs:94-111`. `run_sweep`'s loop body becomes a call to it. Nothing else changes and
no write moves.

- [ ] **Step 4: Correct the two doc blocks the change makes false**

`lib.rs:21-54` and `lib.rs:131-140` both exist to justify the old parameter shape:
"`read` starts `false` and is set inside `probe_endpoint` as soon as the fetch's
`Declarations` are known", "appending as it goes so a caller that cancels this future
can still see how far it got", "a cancelled future returns nothing". The mechanism is
still real, so the sentences need re-aiming at `EndpointSweep`, not deleting. Note
also that the 34-line block at `:21-54` is attached to `pub struct Sweep` at `:55`
while `pub async fn run_sweep` at `:69` has no doc comment at all; put each part where
it belongs while you are in there.

- [ ] **Step 5: Run the suite and commit**

Every test passes, including `:514` and `:577`. If any test other than the new one
needed editing, say why in your report: a refactor that forces a test change is a
behaviour change.

```bash
git add prober/src/lib.rs prober/tests/end_to_end.rs
git commit -m "Move the per-endpoint accumulator into the per-endpoint work"
```

---

### Task 3: Bounded concurrency, reassembled by slot

**Files:**
- Modify: `prober/src/lib.rs` (`run_sweep`, `Sweep::failed_endpoints`, plus a testable `assemble`)
- Modify: `prober/src/emit.rs` (`NotMeasuredReason::ProberFailed`, the two activity-level facts)
- Modify: `prober/src/main.rs` (`--concurrency`, exit status)
- Modify: `prober/tests/end_to_end.rs` (32 call sites), `prober/tests/live_smoke.rs` (the 33rd)
- Test: `prober/tests/end_to_end.rs`, plus a unit test for `assemble`

**Interfaces:**
- Produces: `pub async fn run_sweep(endpoints: &[String], defs: &[MetricDef], declined: &[MetricDef], client: &Arc<Client>, budget: Budget, concurrency: NonZeroUsize) -> Sweep`, with `Sweep` gaining `pub failed_endpoints: usize`. Also
  `fn assemble(slots: Vec<Option<EndpointSweep>>, endpoints: &[String], defs: &[MetricDef], declined: &[MetricDef]) -> Sweep`, a free function over already-normalised slots so it is unit-testable.
- The count goes **on `Sweep`**, not into a tuple: all 33 existing call sites destructure `Sweep { .. }`, so `-> (Sweep, usize)` would force every one of them to become `let (Sweep { .. }, _) = ...`. It also belongs there on the merits, as a fact about the run alongside the four fact lists.
- `&Arc<Client>` rather than `Arc<Client>` keeps each call-site edit to adding `&` and one argument.

**The 33 call sites.** `main.rs` is not the only caller: `prober/tests/end_to_end.rs`
calls `run_sweep` in 32 places and `prober/tests/live_smoke.rs` in one more. The
`live_smoke.rs` tests are `#[ignore]`d, so the suite reports "2 ignored" and the file is
easy to miss, but `cargo test` compiles ignored tests and CI runs `-D warnings`, so
missing it leaves the build red. Pass `NonZeroUsize::new(1).unwrap()` at all 33 so they
keep testing exactly what they test today. This is a mechanical edit, it is expected,
and it is not the kind of test change Task 2's rule is about.

**Why `NonZeroUsize`.** `Semaphore::new(0)` hangs rather than failing, and a
`concurrency.max(1)` inside the library would silently repair a caller's 0 while main
refuses it, which is the silent default `Politeness::unlimited()`'s doc refuses in its
own words. Making 0 inexpressible removes the hazard at the type level.

**The unit of concurrency is a host.** Group `endpoints` by `politeness::host_key`,
preserving each endpoint's input index as its slot. Each group becomes **one sequential
task** that probes its endpoints in input order. The global `Semaphore` bounds how many
*groups* run at once, so `--concurrency` means "how many hosts we talk to at once".

This is what makes the budget arithmetic safe, and it replaces the validator the
previous draft specified. `gated_hop` holds the per-host guard until the request
returns, so a competing endpoint on the same host holds it for its gap plus its whole
request, and on a throttled host for gap plus request plus the honoured `Retry-After`
plus the retry: `main.rs:46-63` states that worst case as `2 + 30 + 20 + 30 = 82`
seconds. Two endpoints of one host in flight would therefore spend up to 82 seconds
waiting for a lock **inside a 60-second metric budget**, silently, because a cancelled
metric budget does not warn. One endpoint per host in flight removes the term
altogether: no endpoint ever waits on another endpoint's host lock, the metric budget
sees exactly what it sees today, and the existing `validate_min_gap` relation remains
the only one needed. Say so in `main.rs` next to `validate_min_gap`, and say that
relaxing per-host serialisation would require the `hold` term rather than the gap.

It also closes the dispatch-order question by construction rather than by heuristic:
grouping by host is what spreads the work, so no round-robin is needed, and "concurrency
only buys parallelism across different hosts" becomes a property of the code.

**Why 4 is the default.** Politeness, not throughput: four hosts in flight with a 2s
per-host gap is roughly 2 requests per second in aggregate, a defensible load for a
service that probes strangers uninvited. **Do not repeat the first draft's schedule
claim.** `README.md:148-151` says its own per-endpoint estimate is "a rough estimate,
not a measurement", assumes all endpoints are distinct hosts, and is a floor: a
black-holed endpoint costs its full 600s budget, so the real bound is
`sum(per-endpoint cost) / concurrency`, worst case `548 * 600 / 4 = 22.8 hours`, and at
a plausible 10% dead `(493 * 14.1 + 55 * 600) / 4 = 9988s = 2.8 hours`. If the README
gives a duration, give it as a floor with the failure path priced in.

**A failed task fills its slot, and says why.** Record `(task_id, group)` at spawn via
`AbortHandle::id()`, because `JoinSet::join_next`'s `Err` arm carries only a `JoinError`
and `JoinError::id()`, so without that map the panic cannot even be logged with the
endpoint's name. Note a group task carries several endpoints, so a panic costs the whole
group; the map must name all of them.

On a `JoinError`, log at `error` and fill every slot in that group with facts. **Emit
`NotMeasured` with a new `NotMeasuredReason::ProberFailed`, one per (endpoint, metric in
`defs`)**, rather than `Indeterminate` measurement rows. Two reasons: there was no
observation at all, and `NotMeasured` is exactly the fact for that, whereas an
`Indeterminate` measurement asserts a measurement happened and was inconclusive; and a
run where the prober crashed on 30 endpoints would otherwise be indistinguishable in the
graph from one where 30 endpoints were slow, with the only record being an exit status
that is gone by the time anyone queries. The declined metrics keep their existing
`CostCeiling` facts, so no (endpoint, metric) gets two `NotMeasured` facts and the
duplicate-subject guard is not triggered. An empty slot is not an option:
`web/queries/endpoint_measurements.rq` would keep serving last night's verdicts as
current with nothing saying this run failed.

**These two additions move Task 1's frozen baseline, and that is expected.**
`only_the_subjects_changed` freezes the `(predicate, object)` multiset and the quad
count of a synthesized `Sweep` from the pre-1c-b3 emitter, so adding activity facts
makes it fail. Update the constants in the same commit, and add a comment saying the
baseline moved because this stage deliberately publishes two new run-level facts, naming
them. Do not weaken the test to tolerate additions: its whole value is that an
unintended predicate cannot slip in, and an intended one costs one line of explanation.

Also publish two run-level facts on the activity, beside the existing
`urn:sparqlwatch:maxCost`: `urn:sparqlwatch:concurrency` and
`urn:sparqlwatch:failedEndpoints`. The first lets a consumer comparing `elapsedMs`
across two runs tell a slower endpoint from a busier sweep; the second lets one tell a
complete run from an incomplete one without reading a log.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn output_order_is_input_order_not_completion_order() {
    // The first endpoint slow, the second fast, concurrency 2. Sequentially the
    // order is input order for the trivial reason that nothing overlapped; here
    // the second finishes first, so any append-as-completed implementation fails.
    // Assert on ALL FOUR fact lists, not just rows: declarations_read,
    // not_measured and content_samples are equally order-sensitive.
}

#[tokio::test]
async fn two_endpoints_on_different_hosts_interleave() {
    // Overlap as a STRUCTURAL fact, not a duration: each mock records the
    // arrival instant of every request, and the assertion is that some request
    // to B arrives between two requests to A. Impossible sequentially whatever
    // the machine speed, and it needs no timing threshold. (A cheap sweep is
    // seven requests per endpoint, so there are 14 arrivals to work with.)
}

#[tokio::test]
async fn two_endpoints_on_one_host_never_overlap() {
    // Now a structural property rather than a spacing one: they are in the same
    // host group, so they run in one sequential task and cannot overlap at all.
    // ONE MockServer with two paths. Two MockServers are two host_keys, because
    // host_key keeps a non-default port, so the two-server version proves
    // nothing.
    // There are 14 arrivals, not 2. Assert on every consecutive pair across
    // both endpoints: non-overlap AND the min gap, since spacing alone holds
    // for two requests that overlapped and released together.
    // Assert both endpoints produced a full row set, or a sweep that dropped
    // one endpoint passes.
    // Wrap in tests/common/mod.rs::without_deadlocking, not an ad-hoc timeout:
    // it is 20s and its docstring records the 4m47s hang it was built for.
}

#[tokio::test]
async fn concurrency_counts_hosts_not_endpoints() {
    // Four endpoints on two hosts at concurrency 4: at most two requests are
    // ever in flight, because two of the four permits can never be used by a
    // second endpoint of an already-running host. Assert from the arrival
    // records that no three requests overlap.
}

#[test]
fn a_failed_group_still_carries_facts_for_every_endpoint_in_it() {
    // Unit test on `assemble` over already-normalised slots, because
    // JoinError has no public constructor and a fold typed over it is not
    // unit-testable at all. Feed [Some, None, Some] and assert: both
    // survivors present in input order, failed_endpoints == 1, and the None
    // slot's endpoint carries one NotMeasured per metric with reason
    // prober-failed, plus its CostCeiling facts unchanged.
}
```

- [ ] **Step 2: Run them and watch them fail.** Record each failure.

- [ ] **Step 3: Implement**

`run_sweep` groups endpoints by `host_key` keeping input slots, spawns one task per
group into a `JoinSet` with an `Arc<Semaphore>` permit held for the whole group, records
the id map, drains `join_next` into `Vec<Option<EndpointSweep>>` indexed by slot, and
calls `assemble`.

The task body must **own** its data (`Vec<(usize, String)>` for the group,
`Arc<Vec<MetricDef>>` for the definitions rather than 548 `defs.to_vec()` clones,
`Arc::clone(&client)`), because `probe_one_endpoint` borrows all three and a spawned
future must be `'static`. The compiler catches this immediately; it is noted only to
save the minute.

`assemble` walks endpoints in input order and, per slot, appends that endpoint's rows,
its one `DeclarationsRead`, its samples, and its declined `NotMeasured` facts, exactly
as the sequential loop did. **All four lists, in that order.**

`main.rs` wraps its client in an `Arc` and exits non-zero when `sweep.failed_endpoints`
is above zero, **after** writing the output file so the run is preserved and the
operator still learns it is incomplete.

- [ ] **Step 4: Run the suite, then run it for real**

Full suite green, 32 call sites updated. Then a real sweep at the default and at
`--concurrency 1`, reporting both wall-clocks. **Do not assert the two outputs are
equal**: live verdicts flip between runs (a 30s request budget against
`qlever.dev/api/osm-planet` is the borderline case `run-with-samples.nq` records at
30003 ms) and a flipped verdict adds or removes a whole `ContentSample`, so neither the
quad count nor the subject set is stable. The equality proof lives in Task 1's
`only_the_subjects_changed`, in memory, where it is decidable. This step is a smoke
check with no equality claim attached.

- [ ] **Step 5: Commit**

```bash
git add prober/src/lib.rs prober/src/main.rs prober/tests/end_to_end.rs
git commit -m "Sweep endpoints concurrently, reassembled by input slot"
```

---

### Task 4: Prove the two subject schemes coexist

**Files:**
- Create: `web/tests/fixtures/run-new-subjects.nq`
- Modify: `web/tests/test_fixture.py` (the provenance list), and whichever query test file fits
- Test: `web/tests/`

After Task 1, every one of the nine committed fixtures carries a subject shape the
prober can no longer produce, so 100% of the web layer's coverage is against the old
scheme, and a production store will hold both shapes for as long as history is kept.
`web/tests/test_fixture.py` records that `run-with-samples.nq` is "a REAL sweep, copied
byte-for-byte", which will no longer be reproducible.

- [ ] **Step 1: Write the failing test**

Load an old-scheme run and a new-scheme run for the same endpoint into one store, and
assert the queries return both: the most-recent-run selection picks the newer one
regardless of subject shape, and the older one is still reachable. This is the only way
the "two schemes coexist" claim is tested rather than asserted, and
`web/queries/*.rq` match on predicates only, never on subject shape, so it should pass
once the fixture exists.

- [ ] **Step 2: Build the fixture**

**Derive it from `run-with-samples.nq`**, exactly as `run-two-sweeps.nq` was built:
rewrite the subjects into the new scheme and advance the run IRI and
`prov:generatedAtTime`. Do not capture it from a live sweep. The test is "two schemes for
the same endpoint coexist and the most-recent-run selection picks the newer", and a live
capture is about whatever the registry held at whatever time it ran, so it would not pair
with the existing fixture's endpoints and timestamps and the assertions would have nothing
to compare. It would also not be reproducible, which is why `test_fixture.py`'s
provenance list labels exactly one fixture a real capture and every other synthetic with
its construction written down. Document this one the same way.

- [ ] **Step 3: Run both suites and commit**

```bash
git add web/tests/fixtures/run-new-subjects.nq web/tests/test_fixture.py web/tests/<the test file>
git commit -m "Prove an old-scheme and a new-scheme run coexist in one store"
```

---

### Task 5: Say what it now guarantees, and what it does not

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: The README**

Document `--concurrency` as **how many hosts are talked to at once**, its default, and
why that default (politeness: roughly 2 requests per second in aggregate). Say that one
endpoint per host is in flight by construction, that this is what keeps per-host waiting
out of the metric budget, and that relaxing it would need the 82-second `hold` term
rather than the gap.

State the identity property as a property of the graph, and the order property as a
property of one emitted file that 1c-b4 is expected to break. State plainly that output
is not byte-identical between runs because `elapsedMs` is a wall-clock measurement.

Document the subject shape with the two clauses that stop the documentation being an
invitation: it must **never** be parsed (every fact carries its endpoint and metric as
triples), and a store will hold two subject shapes for as long as history is kept.
Document the two new activity-level facts (`concurrency`, `failedEndpoints`) and what a
consumer can conclude from them.

Add the userinfo refusal to the README's **registry section**, beside the dedupe rule,
because it changes what gets swept rather than being only a caveat. Add to Known
limitations that the endpoint URL is a permanent reversible part of every subject and
that an API key in a query string remains an unmitigated exposure.

Then correct the claim in `lib.rs` that endpoints "are processed independently so one
slow host cannot delay another's results", which was false before Task 3 and is now
true of probing but still false of the run's output. Check `README.md` for copies of it,
including the sequential-sweep sentence at `README.md:156`.

- [ ] **Step 2: The spec**

Add the 1c-b3 row following the convention the neighbouring rows use. Name what landed,
and say explicitly that the "Results are written per endpoint as they complete" half of
the per-endpoint-isolation bullet is **not** met and is stage 1c-b4's job. Do not claim
the bullet.

- [ ] **Step 3: Commit**

```bash
git add prober/README.md docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md
git commit -m "Document the identity and order properties, and what 1c-b4 still owes"
```

---

## Done when

- Every run-scoped subject is a pure function of (run, endpoint, metric), proven by
  reordering the input and getting the same IRIs, and by one exact expected IRI string
  that pins the encoding.
- A metric id that would make a subject ambiguous is refused by the loader **and** by
  `subject_iri`.
- A conflicting repeated (endpoint, metric) publishes **nothing** about that pair, and
  neither a bad metric id nor a duplicate costs the run its output.
- An endpoint URL carrying userinfo is dropped at registry load, and an `@` in a path or
  query string is not.
- Only subjects changed: proven against a `(predicate, object)` multiset and quad count
  frozen from the current emitter before any implementation.
- An endpoint whose budget expires mid-sweep keeps the verdicts and samples it already
  earned, and a truthful `declarationsRead`.
- A sweep at concurrency 4 emits all four fact lists in input order when the first
  endpoint is the slowest; two endpoints on different hosts interleave; two on one host
  never overlap; and four endpoints on two hosts never put three requests in flight.
- A panicked group still carries facts for every endpoint in it, as `NotMeasured` with
  reason `prober-failed`, distinguishable in the graph from a budget expiry, and the
  exit status says the run is incomplete.
- The activity publishes the concurrency and the failed-endpoint count.
- An old-scheme and a new-scheme run coexist in one store, tested.
- The README documents both properties with their different standing, and the spec row
  does not claim the isolation bullet.
