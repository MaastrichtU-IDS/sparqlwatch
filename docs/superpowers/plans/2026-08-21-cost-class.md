# Stage 1c-b1: A Cost Class, and Saying "Not Measured"

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a metric declare what it costs the endpoint it points at, so an expensive probe is opt-in, and record `not measured` when we decline to run one.

**Architecture:** Three additions, no new subsystems. A `Cost` enum and field on `MetricDef`, filtered inside the library where tests can reach it. A `notMeasured` fact per (endpoint, metric) we declined, carrying its reason, so a consumer can tell "we chose not to" from "we tried and could not". A split of the class metric into a cheap existence probe and an expensive enumeration, driven by a measurement.

**Tech Stack:** Rust 1.96, edition 2021. tokio, reqwest 0.13, oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6.5. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

**Supersedes part of:** `docs/superpowers/plans/2026-08-21-safe-at-scale.md`, whose
own superseded-note records why stage 1c-b was split into four slices.

## Why this plan exists, and the measurement behind it

Stage 1d seeds the registry with 548 endpoint URLs. Before that, a metric has to
be able to say what it costs, because one of the metrics we ship cannot be run
everywhere. Measured against `qlever.dev/api/osm-planet` (planet-scale OSM) on
2026-08-21:

| query | result |
| --- | --- |
| `SELECT DISTINCT ?c WHERE { {?s a ?c} UNION {GRAPH ?anyg {?s a ?c}} } LIMIT 200` | timeout at 45s |
| same, default graph only | 36.9s cold, then over 40s |
| `SELECT ?c WHERE { {?s a ?c} UNION {GRAPH ?anyg {?s a ?c}} } LIMIT 1` | **200 in 0.166s** |

The cost is `DISTINCT` scanning every type, not the named-graph UNION. Dropping
`DISTINCT` and taking `LIMIT 1` turns a timeout into 166ms, roughly 270x. So
"this endpoint holds typed resources" and "here are up to 200 of its classes" are
two different questions at two different prices, and this plan separates them.

The spec requires the outcome as well as the mechanism: metadata tier 3 is
`` `not measured`, never a zero ``, and the risk table's cost-class row says
expensive metrics "record `not measured` rather than a misleading zero". An
earlier draft of this work planned to omit the row entirely, which contradicts
both, and could not be disambiguated by a consumer because metric definitions are
not published yet, so a missing row is indistinguishable from a metric that never
existed in that revision.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- `resolve()` in `src/resolve.rs` is the single home of judgement and stays a pure
  function. `src/client.rs` returns evidence and holds no opinion. `src/emit.rs`
  is a pure function of its inputs, with **no clock read** and no randomness.
- `--at` is never read from the clock.
- Any new field on `MetricDef` must be added to the canonical string in
  `definitions_revision` **and** covered by
  `the_revision_is_a_pure_function_of_the_definitions`. Note: that test does
  **not** currently vary every field, whatever an earlier plan claimed; check
  what it varies before you rely on it.
- **The six-verdict vocabulary is closed.** `not measured` is not a seventh
  verdict: it is the absence of a measurement, recorded as its own fact. Do not
  add a `Verdict` variant.
- The probe dispatch in `lib.rs` has **no `_` arm** and `ProbeKind::ALL` is
  complete by construction. Keep both.
- Every test runs offline against a local `wiremock`. Only `live_smoke` touches
  real endpoints and stays `#[ignore]`d.
- No em-dashes in prose, code comments or Markdown. The crate is free of them.
- After any experiment that temporarily edits source, restore it and run
  `touch src/*.rs tests/*.rs` before the final test run. A build artifact newer
  than the restored sources makes `cargo test` report the reverted code.
- **A test you have not watched fail is not evidence.** Prove every test by the
  mutation its task names, and if a mutation does not fail the test, say so
  rather than moving on.

## Starting state

`main` at `923ca54`. 190 tests pass, 2 ignored, clippy clean with
`--all-targets -- -D warnings`.

```rust
// src/lib.rs
pub async fn run_sweep(endpoints: &[String], defs: &[MetricDef], client: &Client, budget: Budget)
    -> (Vec<MeasurementRow>, Vec<DeclarationsRead>);

// src/metrics.rs
pub struct MetricDef { id, label, dimension, kind, query, expect, var, declared_by, graded }
pub fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>>;
pub fn definitions_revision(defs: &[MetricDef]) -> String;

// src/emit.rs
pub fn emit_nquads(run: &RunId, generated_at: &str, metric_revision: &str,
                   rows: &[MeasurementRow], declarations_read: &[DeclarationsRead])
    -> anyhow::Result<String>;
```

---

## Task 1: The `Cost` enum, and a filter the tests can reach

**Files:**
- Modify: `prober/src/metrics.rs`
- Test: unit tests in `prober/src/metrics.rs`

**Interfaces:**
- Produces: `pub enum Cost { Cheap, Expensive }`, `#[serde(rename_all = "lowercase")]`,
  deriving `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`, plus
  `impl Default for Cost` returning `Cheap`.
- Produces: `MetricDef.cost: Cost` with `#[serde(default)]`.
- Produces: `pub fn within_cost(defs: &[MetricDef], ceiling: Cost) -> (Vec<MetricDef>, Vec<MetricDef>)`,
  returning (to run, declined) and **preserving input order in both**. This lives
  in `metrics.rs`, not in `main.rs`: a filter in `main.rs` cannot be reached from
  the integration tests, so the only thing a test could exercise would be a
  filter it performed itself.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_metric_without_a_cost_is_cheap() {
    let defs = load_metrics(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n"
    ).unwrap();
    assert_eq!(defs[0].cost, Cost::Cheap, "a definition silent about cost is cheap");
}

#[test]
fn an_unknown_cost_is_a_load_error_not_a_silent_default() {
    // Same doctrine as an unknown `kind`: the set is closed, and guessing
    // silently changes what a sweep costs somebody else's server.
    assert!(load_metrics(
        "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"free\"\n"
    ).is_err());
}

#[test]
fn within_cost_splits_and_keeps_order() {
    let defs = load_metrics(concat!(
        "[[metric]]\nid=\"a\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
        "[[metric]]\nid=\"b\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n",
        "[[metric]]\nid=\"c\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
    )).unwrap();

    let (run, declined) = within_cost(&defs, Cost::Cheap);
    assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    assert_eq!(declined.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["b"]);

    let (run, declined) = within_cost(&defs, Cost::Expensive);
    assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"],
               "the higher ceiling runs everything, still in file order");
    assert!(declined.is_empty());
}

#[test]
fn cost_is_part_of_the_definitions_revision() {
    // The revision exists so a measurement can be read against the definition
    // that produced it, and cost changes which metrics run at all.
    let one = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n";
    let two = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n";
    assert_ne!(
        definitions_revision(&load_metrics(one).unwrap()),
        definitions_revision(&load_metrics(two).unwrap())
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --lib`
Expected: compile errors, no `Cost`, no `cost`, no `within_cost`.

- [ ] **Step 3: Implement**

```rust
/// What a metric costs the endpoint we point it at. A closed set, like
/// `ProbeKind`: an unknown value is a load error, because guessing silently
/// changes what a sweep costs somebody else's server.
///
/// `Cheap` means the query can stop at its first match. `Expensive` means it
/// forces a scan. The line is not a guess: measured on qlever.dev's
/// planet-scale OSM endpoint, the same class query answers in 0.166s with
/// `LIMIT 1` and no `DISTINCT`, and times out past 45s with
/// `DISTINCT ... LIMIT 200`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Cost {
    #[default]
    Cheap,
    Expensive,
}
```

Add `#[serde(default)] pub cost: Cost` to `MetricDef` and to the canonical string
in `definitions_revision`.

- [ ] **Step 4: Check what the revision test actually varies**

Read `the_revision_is_a_pure_function_of_the_definitions` before extending it.
Report in your report which fields it varies today. If it does not vary every
field, extend it to vary `cost` at minimum, and say what remains unvaried rather
than silently leaving the gap.

- [ ] **Step 5: Prove the tests are load-bearing**

Mutations, each alone: make `Cost::default()` return `Expensive` (the
default-is-cheap test must fail); drop `cost` from the canonical string (the
revision test must fail); make `within_cost` return everything in the run half
(the split test must fail); make it reverse order (the order assertions must
fail). Restore, `touch`, re-run.

- [ ] **Step 6: Commit**

```bash
git add prober/src/metrics.rs
git commit -m "feat(prober): metrics declare what they cost the endpoint"
```

---

## Task 2: Record `not measured`, with its reason

The spec requires this outcome twice: metadata tier 3 is
`` `not measured`, never a zero ``, and the risk table says expensive metrics
"record `not measured` rather than a misleading zero".

It is **not** a seventh verdict. A verdict describes what we found out about a
capability; this describes a measurement that did not happen. Mixing them would
mean every consumer filtering verdicts has to know about a value that is not one.
So it is its own fact, and it carries its reason, because "you set the ceiling
low" and "we could not afford it here" are different things a consumer may want
to act on.

**Files:**
- Modify: `prober/src/emit.rs`
- Modify: `prober/src/lib.rs` (return the declined list from the sweep)
- Test: unit tests in `prober/src/emit.rs`, and `prober/tests/end_to_end.rs`

**Interfaces:**
- Produces: `pub struct NotMeasured { pub endpoint: String, pub metric_id: String, pub reason: NotMeasuredReason }`
  and `pub enum NotMeasuredReason { CostCeiling }`, an enum rather than a string so
  a second reason later cannot be spelled two ways.
- Produces: `emit_nquads` gains a `not_measured: &[NotMeasured]` parameter,
  emitting per entry, in the run graph:
  - `<measurement-iri> rdf:type sw:NotMeasured`
  - `<measurement-iri> dqv:computedOn <endpoint>`
  - `<measurement-iri> dqv:isMeasurementOf <metric>`
  - `<measurement-iri> sw:notMeasuredReason "cost-ceiling"`
  It carries **no** `dqv:value` and **no** `sw:level`, because nothing was
  measured. A consumer asking "what is the verdict" gets nothing, which is
  correct; a consumer asking "why is there no verdict" gets an answer.
- Produces: `run_sweep` gains a `declined: &[MetricDef]` parameter and returns
  `Vec<NotMeasured>` alongside its existing values, one entry per (endpoint,
  declined metric). It does **not** filter: `main.rs` calls `within_cost` and
  passes both halves, so the sweep never has to know what a ceiling is.

- [ ] **Step 1: Write the failing tests**

```rust
// emit.rs unit tests
#[test]
fn a_not_measured_fact_carries_no_verdict_and_no_level() {
    let nq = emit_nquads(&RunId(AT.into()), AT, REV, &[], &[], &[NotMeasured {
        endpoint: "http://example.org/sparql".into(),
        metric_id: "classes".into(),
        reason: NotMeasuredReason::CostCeiling,
    }]).unwrap();

    assert!(nq.contains("urn:sparqlwatch:NotMeasured"));
    assert!(nq.contains("cost-ceiling"));
    assert!(!nq.contains("dqv#value"),
            "nothing was measured, so there is no value to publish");
    assert!(!nq.contains("urn:sparqlwatch:level"));
}

#[test]
fn a_not_measured_fact_does_not_collide_with_a_measurement() {
    // Both are subjects in the same graph. If they share an IRI, a consumer
    // joining on the measurement IRI gets a node that both has and has not a
    // verdict.
    let rows = vec![row("http://example.org/sparql", "availability", Verdict::Verified)];
    let nm = vec![NotMeasured {
        endpoint: "http://example.org/sparql".into(),
        metric_id: "classes".into(),
        reason: NotMeasuredReason::CostCeiling,
    }];
    let nq = emit_nquads(&RunId(AT.into()), AT, REV, &rows, &[], &nm).unwrap();
    let subjects: Vec<&str> = nq.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|s| s.contains("measurement"))
        .collect();
    let unique: std::collections::HashSet<_> = subjects.iter().collect();
    assert_eq!(subjects.len() > 0, true);
    assert_eq!(
        unique.len(), 2,
        "one measurement subject and one not-measured subject, never shared"
    );
}
```

```rust
// end_to_end.rs
#[tokio::test]
async fn a_declined_metric_is_recorded_as_not_measured_not_as_indeterminate() {
    // Drive the real sweep with a definition list split by `within_cost`, so
    // this exercises the shipped filter rather than one the test performs.
    let (run, declined) = within_cost(&shipped_metrics(), Cost::Cheap);
    let out = sweep_against(&server, &run, &declined).await;

    assert!(out.rows.iter().all(|r| r.metric_id != "classes"),
            "a declined metric produces no measurement row");
    assert!(out.not_measured.iter().any(|n| n.metric_id == "classes"),
            "and is recorded as not measured instead");
    assert!(out.rows.iter().any(|r| r.metric_id == "has-classes"),
            "while its cheap counterpart still runs");
}

#[tokio::test]
async fn a_declined_metric_issues_no_request() {
    // The whole point of a cost ceiling. A row we do not publish is worthless if
    // we paid for it anyway.
    let (run, declined) = within_cost(&shipped_metrics(), Cost::Cheap);
    let server = MockServer::start().await;
    // ... mount a catch-all that records requests ...
    let _ = sweep_against(&server, &run, &declined).await;
    let bodies: Vec<String> = server.received_requests().await.unwrap()
        .iter().map(|r| r.url.to_string()).collect();
    assert!(!bodies.iter().any(|u| u.contains("DISTINCT")),
            "the expensive query was never sent");
}
```

- [ ] **Step 2: Run to verify they fail**

Expected: compile failure on the new parameter and types.

- [ ] **Step 3: Implement**

Give the not-measured subject its own IRI shape, distinct from a measurement's:
`urn:sparqlwatch:not-measured:{run}:{n}`. Do not reuse the measurement counter.

Note for a later slice: measurement IRIs are currently built from a running row
index (`emit.rs:108`), which is fine while the whole run is emitted at once and
is **not** fine once endpoints are written incrementally. Stage 1c-b3 replaces
them with stable identifiers. Do not attempt that here, and do not build the
not-measured IRI in a way that depends on the row counter.

- [ ] **Step 4: Prove the tests are load-bearing**

Mutations: emit a `dqv:value` on a not-measured fact (the no-verdict test must
fail); build the not-measured IRI from the same counter as measurements (the
collision test must fail); have `run_sweep` probe the declined metrics anyway
(the no-request test must fail). Restore, `touch`, re-run.

- [ ] **Step 5: Commit**

```bash
git add prober/src prober/tests
git commit -m "feat(prober): record not measured, with its reason, rather than a missing row"
```

---

## Task 3: Split the class metric, and wire the ceiling to the CLI

**Files:**
- Modify: `prober/metrics.toml`
- Modify: `prober/src/main.rs` (`--max-cost`, and the provenance quad)
- Modify: `prober/src/emit.rs` (the ceiling on the activity)
- Modify: `prober/tests/end_to_end.rs` (the existing collision test)
- Test: `prober/tests/end_to_end.rs`

- [ ] **Step 1: Write the failing test**

Extend `the_content_metrics_reach_named_graphs_without_colliding_variables` to
cover `has-classes` as well as `geo-data` and `classes`: it has the same
graph-variable collision hazard, and the same silent false `absent` if it is got
wrong.

Add an assertion that the shipped `metrics.toml` states a cost for **every**
metric explicitly, rather than relying on the default. A file that states its
costs can be read; one that omits them has to be cross-referenced against a
default in the code.

- [ ] **Step 2: Split the metric**

```toml
# Two class metrics, because one query cannot answer both questions at a price we
# can pay everywhere. Measured on qlever.dev/api/osm-planet: the DISTINCT
# enumeration times out past 45s, while the existence probe answers in 0.166s.
# So existence runs everywhere and enumeration is opt-in.
[[metric]]
id = "has-classes"
label = "Holds typed resources"
dimension = "content"
# `SelectIris`, NOT `AskData`. `AskData` routes through `Client::ask_literal`,
# which extracts with the literal guard on (`client.rs:472`), and `?c` in
# `?s a ?c` binds an IRI. Under `AskData` the guard would find no literal,
# report `boolean = false`, and publish `absent` for an endpoint full of typed
# resources: a silent false negative of exactly the kind this project exists to
# prevent.
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

State `cost` on every other metric too. `geo-data` is `cheap`: it is already
`LIMIT 1` with no `DISTINCT`.

- [ ] **Step 3: The flag and the provenance**

`--max-cost cheap|expensive`, default `cheap`. `main.rs` calls `within_cost` and
passes both halves to `run_sweep`.

Emit one quad on the run's activity recording the ceiling:
`<activity> urn:sparqlwatch:maxCost "cheap"`. `emit.rs` stays pure: pass the
ceiling in as a parameter, do not read it from anywhere global.

- [ ] **Step 4: Run a real sweep and compare**

Run: `cargo run --manifest-path prober/Cargo.toml -- --at 2026-08-21T13:00:00Z --out <scratchpad>/run-b1.nq`

Expected, against the three endpoints in `endpoints.toml`: `classes` no longer
appears as a measurement and appears as a `not measured` fact for each endpoint;
`has-classes` appears with a verdict; every other verdict is unchanged from the
previous stage. Report the full verdict table. Then run again with
`--max-cost expensive` and report what `classes` does, including how long it
takes, since one of the three endpoints is the one where it times out.

Note that `ontop.certain.ai.ustp.at` has been failing DNS resolution, so all its
rows read `indeterminate` regardless. Say so rather than reporting it as a result.

- [ ] **Step 5: Prove the test is load-bearing**

Mutation: change `has-classes` to `kind = "AskData"`, which is the mistake the
comment warns about, and confirm a test fails rather than the metric quietly
reporting `absent`. If no test catches it, that is a finding: report it and add
one. Restore, `touch`, re-run.

- [ ] **Step 6: Commit**

```bash
git add prober/src prober/metrics.toml prober/tests
git commit -m "feat(prober): an existence probe that is not a full scan, behind a cost ceiling"
```

---

## Task 4: Document it

**Files:**
- Modify: `prober/README.md`
- Modify: `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: README**

Document the cost class, what `--max-cost cheap` excludes, and the `not measured`
fact including its predicate and reason value, so somebody querying a run knows
what to look for. Record the class-query measurement, with its numbers, as the
reason `has-classes` and `classes` are two metrics: a reader who does not know
that will try to merge them back.

- [ ] **Step 2: Spec**

The spec's metric list names `classes`; add `has-classes` beside it. Mark the
cost-class risk-table row as delivered. Update the delivery sequence to show
stage 1c-b split into b1 through b4, with b1 done, and note that the earlier
single 1c-b plan is superseded and why (its own header records the detail).

- [ ] **Step 3: Commit**

```bash
git add prober/README.md docs/superpowers/specs
git commit -m "docs: the cost class, and what not measured means"
```

---

## Done criteria

- Four tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A default sweep records `classes` as `not measured` for every endpoint and
  publishes a verdict for `has-classes`, with no other verdict changed.
- No declined metric issues a request, proven by a test that inspects what the
  mock server received.
- `not measured` facts carry no `dqv:value` and no level, and never share an IRI
  with a measurement.
- `prober/metrics.toml` states a cost for every metric explicitly.
