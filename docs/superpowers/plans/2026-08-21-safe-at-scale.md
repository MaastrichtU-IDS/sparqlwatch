# Stage 1c-b: Safe At Scale

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a sweep of 548 strangers' endpoints something we can run without being a nuisance, without losing the whole run to one crash, and without one hostile host stalling every host behind it.

**Architecture:** Four additions to the existing sequential sweep, plus one metric-definition change driven by a measurement. A cost class on metric definitions so an expensive probe is opt-in. A politeness gate that serialises and spaces requests per host and honours `Retry-After`. Bounded concurrency across endpoints, with deterministic output regardless of completion order. Crash-safe incremental writing.

**Tech Stack:** Rust 1.96, edition 2021. tokio, reqwest 0.13, oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6.5. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## Why this plan exists

Stage 1c-a made the published metrics mean what they say. This half makes it
safe and affordable to publish them at registry scale. Stage 1d (seeding from the
LOD Cloud dump plus YummyData, 548 endpoint URLs) depends on all four items here:
today's sweep is sequential, has no politeness at all, and writes its output once
at the very end, so a crash at endpoint 500 loses everything and a single slow
host delays the rest.

## The measurement that shapes Task 1

Taken against `qlever.dev/api/osm-planet` (planet-scale OSM) on 2026-08-21:

| query | result |
| --- | --- |
| `SELECT DISTINCT ?c WHERE { {?s a ?c} UNION {GRAPH ?anyg {?s a ?c}} } LIMIT 200` | timeout at 45s |
| same, default graph only | 36.9s cold, then over 40s |
| `SELECT ?c WHERE { {?s a ?c} UNION {GRAPH ?anyg {?s a ?c}} } LIMIT 1` | **200 in 0.166s** |
| same, default graph only | 200 in 0.112s |

So the cost is `DISTINCT` scanning every type, not the named-graph UNION. Dropping
`DISTINCT` and taking `LIMIT 1` turns a timeout into 166ms, roughly 270x. That is
the difference between "this endpoint holds typed resources", which we can afford
everywhere, and "here are up to 200 of its classes", which we cannot. They are two
different metrics and this plan separates them.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- `resolve()` in `src/resolve.rs` is the single home of judgement and stays a
  pure function. `src/client.rs` returns evidence and holds no opinion.
  `src/emit.rs` is a pure function of its inputs, with **no clock read** and no
  randomness.
- **The politeness gate reads the clock, and that is fine.** It is scheduling,
  not measurement. `--at` is still never read from the clock, and no clock read
  may enter `emit.rs` or `resolve.rs`.
- Any new field on `MetricDef` must be added to the canonical string in
  `definitions_revision` **and** covered by
  `the_revision_is_a_pure_function_of_the_definitions`, which varies every field.
  A published revision that does not change when the measurement changes is a
  silent lie.
- `absent` and `verified` are assertive verdicts, publishable only when the
  evidence establishes them. Everything else is `indeterminate`. A metric we
  chose not to run is neither: see Task 1.
- The probe dispatch in `lib.rs` has **no `_` arm** and `ProbeKind::ALL` is
  complete by construction. Keep both properties.
- Every test runs offline against a local `wiremock`. Only `live_smoke` touches
  real endpoints and stays `#[ignore]`d.
- No em-dashes in prose, code comments or Markdown. The crate is currently free
  of them.
- After any experiment that temporarily edits source, restore it and run
  `touch src/*.rs tests/*.rs` before the final test run. A build artifact newer
  than the restored sources makes `cargo test` report the reverted code, which
  has already cost this project a debugging cycle.
- A test you have not watched fail is not evidence. Prove every test by the
  mutation its task names.

## Starting state

`main` at the stage 1c-a merge (`ae18e44`). 190 tests pass, 2 ignored, clippy
clean with `--all-targets -- -D warnings`.

Shapes you will touch, read from the source:

```rust
// src/lib.rs
pub async fn run_sweep(endpoints: &[String], defs: &[MetricDef], client: &Client, budget: Budget)
    -> (Vec<MeasurementRow>, Vec<DeclarationsRead>);

// src/budget.rs
pub struct Budget { pub request: Duration, pub metric: Duration, pub endpoint: Duration }

// src/registry.rs
pub fn load_endpoints(toml_text: &str) -> anyhow::Result<Vec<String>>;
pub fn dedupe(endpoints: &[String]) -> Vec<String>;

// src/metrics.rs
pub struct MetricDef { id, label, dimension, kind, query, expect, var, declared_by, graded }

// src/main.rs, the tail of it
let (rows, declarations_read) = run_sweep(&endpoints, &defs, &client, budget).await;
let nq = emit_nquads(&RunId(args.at.clone()), &args.at, &revision, &rows, &declarations_read)?;
std::fs::write(&args.out, nq)?;
```

---

## Task 1: A cost class, and split the class metric the measurement condemned

**Files:**
- Modify: `prober/src/metrics.rs` (the `Cost` enum, the field, the revision string)
- Modify: `prober/src/main.rs` (a `--max-cost` flag, filtering the definitions)
- Modify: `prober/src/emit.rs` (record the ceiling on the activity)
- Modify: `prober/metrics.toml` (the split, and cost on each metric)
- Test: `prober/tests/end_to_end.rs`, unit tests in `metrics.rs` and `emit.rs`

**Interfaces:**
- Produces: `pub enum Cost { Cheap, Expensive }` with `#[serde(rename_all = "lowercase")]`,
  and `MetricDef.cost: Cost` defaulting to `Cost::Cheap`.
- Produces: a `--max-cost cheap|expensive` CLI flag, default `cheap`.
- Produces: a quad on the run's PROV activity recording the ceiling the run used.

- [ ] **Step 1: Write the failing tests**

```rust
// metrics.rs unit tests
#[test]
fn a_metric_without_a_cost_is_cheap() {
    let defs = load_metrics("[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n").unwrap();
    assert_eq!(defs[0].cost, Cost::Cheap, "a definition that says nothing about cost is cheap");
}

#[test]
fn an_unknown_cost_is_a_load_error_not_a_silent_default() {
    // Same doctrine as an unknown `kind`: the set is closed, and guessing
    // silently changes what a sweep costs a stranger's server.
    assert!(load_metrics("[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"free\"\n").is_err());
}

#[test]
fn cost_is_part_of_the_definitions_revision() {
    // The revision exists so a measurement can be read against the definition
    // that produced it. Cost changes which metrics run, so it changes the
    // definition list.
    let cheap = load_metrics("[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n").unwrap();
    let exp = load_metrics("[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n").unwrap();
    assert_ne!(definitions_revision(&cheap), definitions_revision(&exp));
}
```

```rust
// end_to_end.rs
/// A metric excluded by the cost ceiling produces NO row, and the run records
/// the ceiling that excluded it. It must not produce an `indeterminate` row:
/// `indeterminate` means we tried and could not find out, and we did not try.
/// The distinction matters to a consumer deciding whether to re-probe.
#[tokio::test]
async fn a_metric_above_the_cost_ceiling_is_not_measured_and_not_reported_indeterminate() {
    let run = sweep_with_max_cost(Cost::Cheap).await;
    assert!(run.rows.iter().all(|r| r.metric_id != "classes"),
            "an expensive metric above the ceiling produces no row at all");
    assert!(run.rows.iter().any(|r| r.metric_id == "has-classes"),
            "and its cheap counterpart still runs");
    assert_eq!(run.cost_ceiling_quads(), 1,
               "exactly one quad records the ceiling, so a consumer can see why a metric is missing");
}

#[tokio::test]
async fn raising_the_ceiling_runs_the_expensive_metric() {
    let run = sweep_with_max_cost(Cost::Expensive).await;
    assert!(run.rows.iter().any(|r| r.metric_id == "classes"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: compile errors, no `Cost`, no `cost` field, no `--max-cost`.

- [ ] **Step 3: Implement the cost class**

```rust
/// What a metric costs the endpoint we point it at. A closed set, like
/// `ProbeKind`: an unknown value is a load error, because guessing silently
/// changes what a sweep costs somebody else's server.
///
/// `Cheap` means the query can stop at its first match. `Expensive` means it
/// forces a scan. Measured on qlever.dev/api/osm-planet: the same class query
/// takes 0.166s with `LIMIT 1` and no `DISTINCT`, and over 45s with
/// `DISTINCT ... LIMIT 200`. That is the line this enum draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cost { Cheap, Expensive }
```

Add `#[serde(default)] pub cost: Cost` to `MetricDef` with a `Default` impl
returning `Cheap`, add it to the canonical string in `definitions_revision`, and
extend `the_revision_is_a_pure_function_of_the_definitions` to vary it.

- [ ] **Step 4: The flag, the filter, and the provenance**

`--max-cost cheap|expensive`, default `cheap`. Filter `defs` before the sweep, so
an excluded metric issues no request and produces no row.

Emit one quad on the run's activity: `urn:sparqlwatch:maxCost "cheap"`. Without
it, a missing row is indistinguishable from a metric that did not exist in that
revision. `emit.rs` stays pure: pass the ceiling in as a parameter.

- [ ] **Step 5: Split the class metric**

```toml
# Two class metrics, because one query cannot answer both questions at a price
# we can pay everywhere. Measured on qlever.dev/api/osm-planet: the DISTINCT
# enumeration below times out past 45s, while the existence probe answers in
# 0.166s. So existence runs on every endpoint and enumeration is opt-in.
[[metric]]
id = "has-classes"
label = "Holds typed resources"
dimension = "content"
# `SelectIris`, NOT `AskData`. `AskData` routes through `Client::ask_literal`,
# which extracts with the literal guard on (`client.rs:472`), and `?c` in
# `?s a ?c` binds an IRI. Under `AskData` the guard would find no literal,
# report `boolean = false`, and publish `absent` for an endpoint full of typed
# resources: a silent false negative of exactly the kind this project exists to
# prevent. `SelectIris` extracts IRIs and resolves non-empty bindings to a
# confirmation.
kind = "SelectIris"
var = "c"
cost = "cheap"
query = """
SELECT ?c WHERE { { ?s a ?c } UNION { GRAPH ?anyg { ?s a ?c } } } LIMIT 1
"""

[[metric]]
id = "classes"
label = "Distinct classes"
dimension = "content"
kind = "SelectIris"
var = "c"
cost = "expensive"
query = """
SELECT DISTINCT ?c WHERE { { ?s a ?c } UNION { GRAPH ?anyg { ?s a ?c } } } LIMIT 200
"""
```

Mark `geo-data` and every other existing metric explicitly, rather than relying
on the default, so the file states its costs. `geo-data` is `cheap` (it is
already `LIMIT 1` with no `DISTINCT`).

The existing test `the_content_metrics_reach_named_graphs_without_colliding_variables`
covers `geo-data` and `classes`; extend it to `has-classes`, which has the same
collision hazard.

- [ ] **Step 6: Prove the tests are load-bearing**

Mutations, each alone: default `Cost` to `Expensive` (the default-is-cheap test
must fail); drop `cost` from the revision string (the revision test must fail);
make the filter a no-op (the ceiling test must fail); emit no ceiling quad (the
provenance assertion must fail). Restore, `touch`, re-run.

- [ ] **Step 7: Commit**

```bash
git add prober/src prober/metrics.toml prober/tests
git commit -m "feat(prober): a cost class, and an existence probe that is not a full scan"
```

---

## Task 2: Per-host politeness

Today nothing spaces our requests. A sweep over 548 endpoints will hit some hosts
many times in a row as fast as they answer, and several registry URLs share a
host. This is the task that decides whether operators experience us as a monitor
or as a nuisance, so its defaults err toward slow.

**Files:**
- Create: `prober/src/politeness.rs`
- Modify: `prober/src/client.rs` (consult the gate before every request)
- Modify: `prober/src/lib.rs`, `prober/src/main.rs` (construct and thread it)
- Test: `prober/tests/politeness.rs`

**Interfaces:**
- Produces: `pub struct Politeness` with
  `pub fn new(min_gap: Duration) -> Politeness` and
  `pub async fn acquire(&self, url: &str) -> HostGuard`, where the guard is held
  for the duration of one request and released on drop.
- Produces: `pub fn host_key(url: &str) -> String`, the politeness identity of a
  URL: lowercased host plus port, ignoring scheme, path and userinfo. Two URLs
  with the same `host_key` are the same server and must never be in flight
  together.
- Produces: `pub fn parse_retry_after(value: &str) -> Option<Duration>`, parsing
  the delta-seconds form and the IMF-fixdate form, and returning `None` for
  anything malformed including a negative delta.
- Produces: `pub enum Honour { Wait(Duration), TooLong }` and
  `pub fn honour(requested: Duration, cap: Duration) -> Honour`, so the
  cap decision is a pure function with its own tests, separate from the waiting.
  The tests below use exactly these three names; do not rename them.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_host_key_is_the_server_not_the_url() {
    for (a, b) in [
        ("http://example.org/sparql", "https://example.org/other"),
        ("http://Example.ORG/a", "http://example.org/b"),
        ("http://user:pw@example.org/a", "http://example.org/b"),
    ] {
        assert_eq!(host_key(a), host_key(b), "{a} and {b} are one server");
    }
    assert_ne!(host_key("http://a.example.org/x"), host_key("http://b.example.org/x"));
    // A port is part of the server: two engines commonly share a host.
    assert_ne!(host_key("http://example.org:7878/x"), host_key("http://example.org:7879/x"));
}

#[tokio::test]
async fn two_requests_to_one_host_are_spaced_by_the_minimum_gap() {
    let p = Politeness::new(Duration::from_millis(300));
    let t0 = Instant::now();
    { let _g = p.acquire("http://example.org/a").await; }
    { let _g = p.acquire("http://example.org/b").await; }
    assert!(t0.elapsed() >= Duration::from_millis(300),
            "the second request to the same host waited for the gap");
}

#[tokio::test]
async fn different_hosts_do_not_wait_for_each_other() {
    let p = Politeness::new(Duration::from_secs(5));
    let t0 = Instant::now();
    let (_a, _b) = tokio::join!(p.acquire("http://a.example.org/x"), p.acquire("http://b.example.org/x"));
    assert!(t0.elapsed() < Duration::from_secs(1),
            "a slow gap on one host must not serialise the whole sweep");
}

#[tokio::test]
async fn one_host_never_has_two_requests_in_flight() {
    // The gap alone does not guarantee this: two tasks could both pass the gap
    // check and proceed together. The guard has to exclude.
    let p = std::sync::Arc::new(Politeness::new(Duration::from_millis(0)));
    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (p, live, peak) = (p.clone(), live.clone(), peak.clone());
        set.spawn(async move {
            let _g = p.acquire("http://example.org/x").await;
            let n = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
    while set.join_next().await.is_some() {}
    assert_eq!(peak.load(Ordering::SeqCst), 1, "requests to one host must not overlap");
}

#[test]
fn retry_after_is_read_in_both_forms() {
    assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
    assert_eq!(parse_retry_after("  30 "), Some(Duration::from_secs(30)));
    assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT").is_some(), true);
    assert_eq!(parse_retry_after("not a delay"), None);
    assert_eq!(parse_retry_after("-5"), None, "a negative delay is malformed, not immediate");
}

#[test]
fn a_retry_after_beyond_the_cap_is_not_waited_out() {
    // A server asking us to come back in an hour has told us to go away for this
    // sweep. Waiting would hold a slot and blow the endpoint budget; the honest
    // outcome is to stop probing that host now.
    assert_eq!(honour(Duration::from_secs(3600), Duration::from_secs(120)), Honour::TooLong);
    assert_eq!(honour(Duration::from_secs(5), Duration::from_secs(120)), Honour::Wait(Duration::from_secs(5)));
}
```

The HTTP-date form needs no new dependency: parse it by hand against the three
formats RFC 9110 allows, or accept only the IMF-fixdate form and return `None`
for the others, stating that choice in a comment. Do not add a date crate.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --test politeness`
Expected: compile failure, no `politeness` module.

- [ ] **Step 3: Implement**

A `Mutex<HashMap<String, Arc<Mutex<Instant>>>>` keyed by `host_key` is enough: the
outer lock is held only long enough to clone the per-host handle, and the
per-host lock is what both excludes concurrent requests and carries the last-request
instant. Use `tokio::sync::Mutex`, since the guard is held across an await.

Default `min_gap`: **2 seconds**, with a `--min-gap-ms` flag to change it. State
in a comment why the default is deliberately slow: at seven metrics per endpoint
that is roughly 14 seconds per host, which is nothing to us and invisible to
them, and a monitor that is remembered as a nuisance loses the access it needs.

`Retry-After`: on a 429 or 503 carrying it, wait the requested delay if it is at
or under the cap (**120 seconds**, `--retry-after-cap-s`), then retry the request
once. Beyond the cap, abandon the host for this sweep: the remaining metrics for
that endpoint are `indeterminate`, which is honest, because we stopped asking.

- [ ] **Step 4: Wire it into the client**

Every outbound request acquires a guard first, including the description fetch
and the preflight. Hold it across the request and drop it after. The guard must
not be held while the caller processes the response.

- [ ] **Step 5: Prove the tests are load-bearing**

Mutations: make `acquire` return immediately (the gap and exclusion tests must
fail); key on the full URL instead of the host (the same-host test must fail);
key on host without port (the port test must fail); ignore the cap and always
wait (the cap test must fail). Restore, `touch`, re-run.

- [ ] **Step 6: Commit**

```bash
git add prober/src prober/tests/politeness.rs
git commit -m "feat(prober): space and serialise requests per host, and honour Retry-After"
```

---

## Task 3: Bounded concurrency, deterministic output

**Files:**
- Modify: `prober/src/lib.rs` (concurrent sweep)
- Modify: `prober/src/main.rs` (a `--concurrency` flag)
- Test: `prober/tests/end_to_end.rs`

**Interfaces:**
- `run_sweep` gains a `concurrency: usize` parameter. Its return type is
  unchanged, and **its output must be identical to the sequential order** whatever
  order the work completes in.

- [ ] **Step 1: Write the failing tests**

```rust
/// Output order must not depend on completion order, or a run stops being
/// reproducible and two identical sweeps produce different files.
#[tokio::test]
async fn output_order_is_the_registry_order_whatever_finishes_first() {
    // Three endpoints, the FIRST deliberately slowest, so completion order is
    // the reverse of registry order.
    let run = sweep_three_with_delays(&[400, 50, 5]).await;
    let seen: Vec<&str> = run.rows.iter().map(|r| r.endpoint.as_str()).collect();
    let mut expected = Vec::new();
    for ep in run.registry_order() { for _ in 0..run.metric_count() { expected.push(ep); } }
    assert_eq!(seen, expected, "rows are in registry order, then metric order");
}

#[tokio::test]
async fn concurrency_actually_overlaps_endpoints() {
    // Without this, a "concurrent" sweep that is secretly sequential passes
    // every other test in this file.
    let t0 = Instant::now();
    let _ = sweep_three_with_delays_and_concurrency(&[300, 300, 300], 3).await;
    assert!(t0.elapsed() < Duration::from_millis(700),
            "three 300ms endpoints at concurrency 3 must not take 900ms");
}

#[tokio::test]
async fn concurrency_one_is_the_old_sequential_behaviour() {
    let a = sweep_three_with_delays_and_concurrency(&[5, 5, 5], 1).await;
    let b = sweep_three_with_delays_and_concurrency(&[5, 5, 5], 4).await;
    assert_eq!(a.rows, b.rows, "concurrency changes timing, never output");
}
```

- [ ] **Step 2: Run to verify they fail**

Expected: compile failure on the new parameter, then the overlap test fails
because the sweep is sequential.

- [ ] **Step 3: Implement**

Spawn one task per endpoint, bounded by a `tokio::sync::Semaphore` of
`concurrency` permits (default **8**, `--concurrency`). Collect results into a
`Vec<Option<...>>` indexed by the endpoint's registry position, then flatten in
index order. That gives determinism structurally rather than by sorting after the
fact, so there is no comparator to get wrong.

Per-host serialisation is already Task 2's job, so two registry entries on one
host may both hold concurrency permits while the politeness gate keeps their
requests apart. Note that in a comment: the two limits are independent and both
are needed.

The endpoint budget must still apply per endpoint, not to the whole sweep.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations: collect results in completion order (the order test must fail); set
the semaphore to 1 permit (the overlap test must fail); await each task
immediately after spawning it (the overlap test must fail). Restore, `touch`,
re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/tests
git commit -m "feat(prober): sweep endpoints concurrently, in deterministic output order"
```

---

## Task 4: Crash-safe incremental writing

A sweep of 548 endpoints that dies at endpoint 500 currently loses all 500. The
output is written once, at the end, from memory.

**Files:**
- Modify: `prober/src/main.rs`
- Modify: `prober/src/emit.rs` (a per-endpoint emission entry point)
- Test: `prober/tests/incremental.rs`

**Interfaces:**
- Produces: `emit_endpoint_nquads(run, rows_for_one_endpoint, declarations_read_for_one) -> String`,
  the same pure function restricted to one endpoint's facts, so a completed
  endpoint can be appended the moment it finishes.

- [ ] **Step 1: Write the failing tests**

```rust
/// The whole point: work already done survives a crash.
#[tokio::test]
async fn a_partial_file_holds_the_endpoints_that_finished() {
    let dir = tempdir_in_scratch();
    let out = dir.join("run.nq");
    sweep_two_endpoints_then_abort(&out).await;
    let partial = std::fs::read_to_string(out.with_extension("nq.partial")).unwrap();
    assert!(partial.contains("endpoint-one"), "the endpoint that finished is on disk");
    assert!(!partial.contains("endpoint-two"), "the one that did not, is not");
}

/// And the final file is still canonical and byte-identical run to run.
#[tokio::test]
async fn the_final_file_is_byte_identical_across_two_identical_runs() {
    let a = run_sweep_to_string().await;
    let b = run_sweep_to_string().await;
    assert_eq!(a, b, "same --at and same definitions produce the same bytes");
}

#[tokio::test]
async fn the_partial_file_is_removed_on_success() {
    let dir = tempdir_in_scratch();
    let out = dir.join("run.nq");
    full_sweep(&out).await;
    assert!(out.exists());
    assert!(!out.with_extension("nq.partial").exists(),
            "a leftover partial file would look like a crashed run");
}
```

Use the session scratchpad for temporary files, never `/tmp` directly, and never
the repository.

- [ ] **Step 2: Run to verify they fail**

Expected: no partial file is written at all.

- [ ] **Step 3: Implement**

Append each endpoint's quads to `<out>.partial` as that endpoint completes,
flushing after each append so a kill leaves the file usable. On success, write
the canonical `<out>` from the complete sorted set and remove the partial.

Two properties to keep, and say so in comments: the partial file may be in
completion order, because it is a crash artifact rather than the published
output; the final file is in registry order, because that is what makes a run
reproducible.

If the output path's parent is not writable, fail before probing anything rather
than after 548 endpoints of work.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations: skip the partial write (the partial test must fail); leave the partial
in place on success (the cleanup test must fail); write the final file in
completion order (the byte-identity test must fail, given a fixture whose
completion order differs from registry order). Restore, `touch`, re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/tests/incremental.rs
git commit -m "feat(prober): write each endpoint as it finishes, so a crash keeps the work"
```

---

## Task 5: Documentation, and one honest measurement

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: Document what a sweep now costs a stranger**

In `prober/README.md`: the politeness defaults and the reasoning (2s per host,
`Retry-After` honoured to a 120s cap, then the host is abandoned for the sweep);
the concurrency default; the cost class and what `--max-cost cheap` excludes;
and the crash-safety contract, including that a `.partial` file left behind means
the run died.

Record the class-query measurement in the README too, not only in
`metrics.toml`: it is the reason `has-classes` and `classes` are two metrics, and
a reader who does not know that will try to merge them back.

- [ ] **Step 2: Estimate the real sweep, and say so**

With the defaults, compute what a 548-endpoint sweep costs in wall-clock and in
requests per host, and put the arithmetic in the README. Show the working so a
reader can check it: metrics per endpoint, requests per metric, the per-host gap,
the concurrency, and where the endpoint budget caps it.

State plainly that this number is arithmetic, not a measurement, and that no
548-endpoint sweep has been run. Stage 1d is what will produce the measured
figure.

- [ ] **Step 3: Update the spec's delivery sequence**

Mark 1c-b delivered, and record that 1d is now unblocked. Fold in the
class-metric split, since the spec's metric list names `classes`.

- [ ] **Step 4: Commit**

```bash
git add prober/README.md docs/superpowers/specs
git commit -m "docs(prober): what a polite sweep costs, and what we have not measured"
```

---

## Done criteria

- Five tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A real sweep over the three endpoints in `endpoints.toml` still produces the
  same verdicts as before this stage, except that `classes` is now absent under
  the default cost ceiling and `has-classes` is present.
- Two identical runs produce byte-identical output files.
- No `.partial` file remains after a successful run.
- `prober/README.md` states the politeness defaults, the cost ceiling, the
  crash-safety contract, and the sweep estimate labelled as arithmetic.
