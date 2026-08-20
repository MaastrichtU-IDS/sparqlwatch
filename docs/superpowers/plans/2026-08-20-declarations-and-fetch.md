# Declarations and the Fetch Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Verified` and graded levels reachable, by fetching what an endpoint publishes about itself and comparing it against what the probes observe.

**Architecture:** A new probe kind fetches RDF (no SPARQL query involved), the fetched graph is reduced to a small `Declarations` value, and that value replaces the hardcoded `Declared { claimed: false }` at the one place resolution consumes it. Judgement stays in `resolve()`; parsing stays out of it.

**Tech Stack:** Rust 1.96, tokio, reqwest, oxrdf + oxrdfio (already present — no new dependencies).

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## Why this slice, and why now

The spec's delivery sequence has a single stage 1b covering declarations, the fetch probe, registry seeding and per-host politeness. **This plan deliberately takes only the first two.** Reason: registry seeding at 548 endpoints depends on politeness, concurrency and incremental writes, none of which exist; declarations and fetching depend on nothing. Splitting along the dependency, not the label, keeps each plan independently shippable. The remainder becomes 1c (safe at scale) and 1d (registry seeding).

What stage 1 left unreachable, and this plan fixes:

- `Declared.claimed` is hardcoded `false` at `prober/src/lib.rs`, so **no capability can ever resolve to `Verified`** — every working one reports `undeclared-but-verified`.
- `ProbeKind::FetchWellKnown` issues no request at all and always reports `indeterminate`.
- `resolve::grade_service_description` is implemented and tested but has **no production caller**; nothing produces the booleans it takes, and `MeasurementRow.level` is always `None`, so no emitted quad ever carries a level.

## Global Constraints

- **Rust 1.96.0, edition 2021.** No nightly features.
- **No new dependencies.** Verified on 2026-08-20: `oxrdfio` 0.2.5 (already a dependency) parses Turtle and RDF/XML, and `RdfFormat::from_media_type("text/turtle")` resolves a format from a `Content-Type`. Do not add `oxttl`, `rio`, or `sophia`.
- Do not change dependency **versions**. `oxrdf = "0.3"` with `oxrdfio = "0.2"` is the only combination that compiles (`oxrdfio` 0.2.5 internally pins `oxrdf` `=0.3.3`). `reqwest`'s `["rustls", "gzip", "query", "system-proxy"]` features are load-bearing.
- **`Absent` may only be claimed when the evidence establishes absence.** A timeout, a transport error, an HTML console, an unparsed body, or any non-2xx response resolves to `Indeterminate`. This plan adds new evidence paths, so every one of them must respect that rule.
- **Judgement lives only in `resolve()`.** Parsing produces facts; `resolve()` decides. Do not add a verdict decision to the client, the parser, or the sweep.
- No composite score, no ranking, and never emit `severity()`.
- Emission stays a pure function of its inputs: no clock reads, no randomness.
- Test output pristine, snake_case test names, suite green under `RUSTFLAGS="-D warnings"` and `cargo clippy --all-targets -- -D warnings`.
- The live smoke test stays `#[ignore]`d; the default suite touches nothing beyond localhost.

## What the real world looks like

From the survey in `~/code/umaka-test` (548 LOD Cloud endpoint URLs). These numbers are why the tests below assert what they do:

- A service description is obtained by a **queryless GET on the endpoint itself** with an RDF `Accept` header. That is how 28 of 548 were harvested; there is no separate discovery step for it.
- **28 of 548** returned a parseable service description. **21 of those 28 are byte-identical 14-triple Virtuoso stubs** whose only `sd:feature` values are `UnionDefaultGraph` and `DereferencesURIs`. Hence grading rather than a boolean.
- **0 of 28** declare anything geospatial, while 18 endpoints evaluate `geof:sfWithin`. So wiring declarations will flip almost nothing to `Verified`, and `undeclared-but-verified` remaining the common answer is the **correct** outcome, not a bug. A test must pin that.
- **23 of 28** claim `sd:SPARQL10Query` only while demonstrably answering 1.1. Never gate a probe on a declaration.
- Only 4 of 28 descriptions are substantial, and they are substantial because of `void:` partitions, not `sd:`.

---

## File Structure

| File | Responsibility |
|---|---|
| `prober/src/observe.rs` | add `BodyKind::Rdf`; add the fetched-graph carrier |
| `prober/src/client.rs` | add `fetch_rdf` — a queryless GET that accepts RDF |
| `prober/src/declare.rs` | **new** — parse a fetched graph into `Declarations`; no I/O, no judgement |
| `prober/src/metrics.rs` | add `declared_by` to `MetricDef`; set it in `metrics.toml` |
| `prober/src/resolve.rs` | consume real `Declarations`; expose the grading inputs |
| `prober/src/lib.rs` | fetch once per endpoint, thread declarations and level through |
| `prober/src/emit.rs` | unchanged (already emits a level when present) |
| `prober/tests/declare.rs` | **new** — fixture-driven parsing tests |
| `prober/tests/fetch.rs` | **new** — wiremock tests for the fetch path |
| `prober/tests/fixtures/*.ttl` | **new** — a Virtuoso stub, a substantial description, a malformed one |

---

### Task 1: `BodyKind::Rdf` and a queryless RDF fetch

**Files:**
- Modify: `prober/src/observe.rs`, `prober/src/client.rs`
- Test: `prober/tests/fetch.rs` (create)

**Interfaces:**
- Consumes: `Budget`, `Observation`, `BodyKind` as they are.
- Produces: `BodyKind::Rdf`; `Observation.body: Option<String>` (the retained payload, capped); `Client::fetch_rdf(&self, url: &str) -> Observation`.

- [ ] **Step 1: Write the failing tests**

```rust
// prober/tests/fetch.rs
use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::observe::BodyKind;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STUB: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/sparql> a sd:Service ; sd:feature sd:UnionDefaultGraph .
"#;

#[tokio::test]
async fn a_turtle_body_is_classified_as_rdf_and_retained() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/turtle")
            .set_body_string(STUB))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    assert_eq!(o.status, Some(200));
    assert_eq!(o.body_kind, BodyKind::Rdf);
    assert!(o.body.as_deref().unwrap().contains("sd:Service"));
    assert!(o.error.is_none());
}

#[tokio::test]
async fn the_fetch_sends_no_query_parameter() {
    // The service description is obtained by a QUERYLESS GET. Sending
    // `?query=` is what the old FetchWellKnown path did wrong: a malformed
    // protocol request in every operator's log, learning nothing.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/turtle").set_body_string(STUB))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let _ = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].url.query().is_none(), "fetch must not send a query string, got {:?}", reqs[0].url.query());
}

#[tokio::test]
async fn the_fetch_asks_for_rdf_not_sparql_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .and(header("accept", "text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/turtle").set_body_string(STUB))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    assert_eq!(o.status, Some(200), "the Accept matcher did not match");
}

#[tokio::test]
async fn an_html_console_is_not_rdf() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string("<!doctype html><html><body>YASGUI</body></html>"))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    assert_eq!(o.body_kind, BodyKind::Html);
}

#[tokio::test]
async fn a_404_is_recorded_with_its_status_not_as_a_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    assert_eq!(o.status, Some(404));
    assert!(o.error.is_none(), "a 404 is an answer, not a transport failure");
    assert_ne!(o.body_kind, BodyKind::Rdf);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd prober && cargo test --test fetch 2>&1 | tail -20`
Expected: FAIL — `no method named fetch_rdf`, `no variant named Rdf`, `no field body`.

- [ ] **Step 3: Add the observation surface**

In `prober/src/observe.rs`: add `Rdf` to `BodyKind`, and add to `Observation`:

```rust
    /// The retained response body, truncated. Kept because "why did this
    /// endpoint score badly" is the first question a provider asks, and
    /// because the declaration parser reads it.
    pub body: Option<String>,
```

Cap the retained body at 256 KiB when populating it (a `const MAX_BODY: usize = 256 * 1024;` in `client.rs`, truncated on a char boundary). Update `Observation::failed` to set `body: None`, and every existing construction site accordingly — the existing tests must keep passing unchanged.

- [ ] **Step 4: Add `fetch_rdf`**

In `prober/src/client.rs`. It must:
- issue a **GET with no query string at all** (do not use `.query(...)`),
- send `Accept: text/turtle, application/rdf+xml;q=0.9, application/ld+json;q=0.8`,
- **not** send `Origin` (that belongs to the CORS probe alone),
- classify the body: HTML by the existing sniffing rules first; otherwise `Rdf` when the `Content-Type` resolves via `oxrdfio::RdfFormat::from_media_type` **and** the payload parses without error; otherwise `Other`,
- retain the body (capped) on any 2xx,
- return `Observation::failed(..)` only for a transport failure, exactly as `get_with_body` does.

Reuse the existing timing and error handling shape rather than inventing a parallel one.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd prober && cargo test 2>&1 | grep -E 'test result'`
Expected: all green, including the pre-existing 66.

- [ ] **Step 6: Commit**

```bash
git add prober/src/observe.rs prober/src/client.rs prober/tests/fetch.rs
git commit -m "feat(prober): fetch RDF with a queryless GET, and retain the body"
```

---

### Task 2: Parse a fetched graph into `Declarations`

**Files:**
- Create: `prober/src/declare.rs`, `prober/tests/declare.rs`, `prober/tests/fixtures/virtuoso-stub.ttl`, `prober/tests/fixtures/substantial.ttl`, `prober/tests/fixtures/malformed.ttl`
- Modify: `prober/src/lib.rs` (add `pub mod declare;`)

**Interfaces:**
- Consumes: nothing from Task 1 at compile time (it takes a `&str`).
- Produces:
  - `struct Declarations { pub features: BTreeSet<String>, pub extension_functions: BTreeSet<String>, pub languages: BTreeSet<String>, pub triples: usize, pub names_dataset: bool, pub has_void_partitions: bool, pub has_entailment: bool }`
  - `Declarations::empty() -> Self`
  - `Declarations::declares(&self, iri: &str) -> bool` — true if the IRI appears in features, extension functions or languages
  - `fn parse_declarations(body: &str, content_type: Option<&str>) -> Declarations`

- [ ] **Step 1: Write the fixtures**

`virtuoso-stub.ttl` — reproduce the shape found on 21 of 28 real endpoints: a `sd:Service` with `sd:endpoint`, `sd:feature sd:UnionDefaultGraph`, `sd:feature sd:DereferencesURIs`, `sd:resultFormat` several times, `sd:supportedLanguage sd:SPARQL10Query`, `sd:url`. Aim for 14 triples.

`substantial.ttl` — a description that earns a high grade: `sd:defaultDataset` naming a `sd:Dataset`, several `void:classPartition` and `void:propertyPartition`, and `sd:defaultEntailmentRegime`.

`malformed.ttl` — valid-looking prefixes then a syntax error partway through (an unterminated IRI is enough).

- [ ] **Step 2: Write the failing tests**

```rust
// prober/tests/declare.rs
use sparqlwatch_prober::declare::{parse_declarations, Declarations};

const SD: &str = "http://www.w3.org/ns/sparql-service-description#";

#[test]
fn a_virtuoso_stub_declares_only_the_two_stock_features() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), Some("text/turtle"));
    assert!(d.declares(&format!("{SD}UnionDefaultGraph")));
    assert!(d.declares(&format!("{SD}DereferencesURIs")));
    // The finding that matters: 21 of 28 real descriptions look exactly like
    // this, and not one of them declares anything geospatial.
    assert!(!d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"));
    assert!(d.extension_functions.is_empty(), "no real stub declares extension functions");
    assert!(!d.has_void_partitions);
    assert!(!d.has_entailment);
}

#[test]
fn a_substantial_description_reports_its_richer_signals() {
    let d = parse_declarations(include_str!("fixtures/substantial.ttl"), Some("text/turtle"));
    assert!(d.names_dataset);
    assert!(d.has_void_partitions);
    assert!(d.has_entailment);
    assert!(d.triples > 14);
}

#[test]
fn a_malformed_body_yields_empty_declarations_rather_than_panicking() {
    let d = parse_declarations(include_str!("fixtures/malformed.ttl"), Some("text/turtle"));
    assert_eq!(d.triples, 0);
    assert!(d.features.is_empty());
}

#[test]
fn an_unknown_content_type_still_parses_if_the_payload_is_turtle() {
    let d = parse_declarations(include_str!("fixtures/virtuoso-stub.ttl"), None);
    assert!(d.declares(&format!("{SD}UnionDefaultGraph")), "should fall back to Turtle");
}

#[test]
fn empty_declarations_declare_nothing() {
    let d = Declarations::empty();
    assert!(!d.declares(&format!("{SD}UnionDefaultGraph")));
    assert_eq!(d.triples, 0);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd prober && cargo test --test declare 2>&1 | tail -20`
Expected: FAIL — unresolved import `sparqlwatch_prober::declare`.

- [ ] **Step 4: Implement the parser**

`prober/src/declare.rs`. Requirements:
- Resolve the format with `RdfFormat::from_media_type(content_type)`, falling back to `RdfFormat::Turtle` when absent or unrecognised.
- Parse with `oxrdfio::RdfParser`. **A parse error must yield whatever was collected so far, never a panic and never an `Err`** — a partial graph is still evidence, and the caller has no better option than "what we could read".
- Collect: object IRIs of `sd:feature`, `sd:extensionFunction`, `sd:supportedLanguage`; count triples; set `names_dataset` when `sd:defaultDataset` or `sd:graph` appears; `has_void_partitions` when `void:classPartition` or `void:propertyPartition` appears; `has_entailment` when `sd:defaultEntailmentRegime` appears.
- No I/O, no judgement, no verdicts. This module decides nothing.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd prober && cargo test 2>&1 | grep -E 'test result'`

- [ ] **Step 6: Commit**

```bash
git add prober/src/declare.rs prober/tests/declare.rs prober/tests/fixtures prober/src/lib.rs
git commit -m "feat(prober): reduce a fetched graph to the declarations it makes"
```

---

### Task 3: Metrics name the declaration that would satisfy them

**Files:**
- Modify: `prober/src/metrics.rs`, `prober/metrics.toml`
- Test: inline `#[cfg(test)]` in `prober/src/metrics.rs`

**Interfaces:**
- Consumes: `MetricDef` as it is.
- Produces: `MetricDef.declared_by: Option<String>` — the IRI whose presence in an endpoint's declarations means the endpoint claims this capability.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_metric_can_name_the_declaration_that_would_satisfy_it() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo = ms.iter().find(|m| m.id == "geo-functions").unwrap();
        assert_eq!(
            geo.declared_by.as_deref(),
            Some("http://www.opengis.net/def/function/geosparql/sfWithin")
        );
        // Most metrics have no declaration that could speak for them.
        assert!(ms.iter().find(|m| m.id == "availability").unwrap().declared_by.is_none());
    }
```

- [ ] **Step 2: Run it and see it fail**

Run: `cd prober && cargo test --lib metrics 2>&1 | tail -12`
Expected: FAIL — no field `declared_by`.

- [ ] **Step 3: Implement**

Add to `MetricDef`, after `var`:

```rust
    /// The IRI whose presence in the endpoint's own declarations means it
    /// claims this capability. Absent for metrics no declaration can speak
    /// for (liveness, response time, CORS headers).
    #[serde(default)]
    pub declared_by: Option<String>,
```

In `metrics.toml`, set it on `geo-functions` only:
`declared_by = "http://www.opengis.net/def/function/geosparql/sfWithin"`, with a comment recording that **0 of 28 surveyed endpoints declare this** while 18 evaluate it, so this field is expected to change almost nothing — it exists so that the rare honest endpoint is credited.

- [ ] **Step 4: Run tests, then commit**

```bash
git add prober/src/metrics.rs prober/metrics.toml
git commit -m "feat(prober): a metric may name the declaration that would satisfy it"
```

---

### Task 4: `resolve()` consumes real declarations and grades

**Files:**
- Modify: `prober/src/resolve.rs`
- Test: inline `#[cfg(test)]` in `prober/src/resolve.rs`

**Interfaces:**
- Consumes: `Declarations` (Task 2), `MetricDef.declared_by` (Task 3).
- Produces:
  - `Declared::from(defs: &Declarations, def: &MetricDef) -> Declared` — `claimed` is true only when `def.declared_by` is `Some(iri)` and `defs.declares(iri)`.
  - `grade_from_declarations(defs: &Declarations) -> Level` — a thin adapter calling the existing `grade_service_description`.
  - `resolve_fetch(defs: &Declarations, obs: Result<&Observation, Expired>) -> (Verdict, Option<Level>)` — the `FetchWellKnown` resolution, now able to reach `Verified`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_declaration_the_endpoint_actually_makes_yields_verified() {
        let mut d = Declarations::empty();
        d.features.insert("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified, "declared and working is Verified");
    }

    #[test]
    fn working_but_undeclared_stays_undeclared_even_with_declarations_wired() {
        // This is the expected outcome for almost every real endpoint: 18
        // evaluate geof:sfWithin and none of them declares it. Wiring
        // declarations must NOT change these to Verified.
        let d = Declarations::empty();
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn a_metric_no_declaration_can_speak_for_is_never_claimed() {
        let mut d = Declarations::empty();
        d.features.insert("http://example.org/anything".into());
        let def = def(ProbeKind::Cors, None); // declared_by is None
        assert!(!Declared::from(&d, &def).claimed);
    }

    #[test]
    fn a_fetched_stub_grades_low_and_a_substantial_one_grades_high() {
        let stub = Declarations { triples: 14, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&stub), Level(1));
        let rich = Declarations { triples: 7077, names_dataset: true, has_void_partitions: true, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&rich), Level(3));
    }

    #[test]
    fn a_fetched_rdf_description_is_verified_with_a_level() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Verified);
        assert_eq!(level, Some(Level(1)));
    }

    #[test]
    fn a_404_on_the_description_is_absent_not_indeterminate() {
        // A 404 is a real answer: the endpoint served us, and there is no
        // description there. That is one of the few honest absences.
        let mut o = obs(None);
        o.status = Some(404);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn an_unparseable_description_body_is_indeterminate() {
        let mut o = obs(None);
        o.status = Some(200);
        o.body_kind = BodyKind::Other;
        let (v, _) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_expired_fetch_is_indeterminate_with_no_level() {
        let (v, level) = resolve_fetch(&Declarations::empty(), Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd prober && cargo test --lib resolve 2>&1 | tail -20`

- [ ] **Step 3: Implement**

- `Declared::from` as specified. Keep the existing `Declared { claimed }` shape so no other call site breaks.
- `grade_from_declarations` maps a `Declarations` onto the existing `grade_service_description(triples, names_dataset, has_void_partitions, has_entailment)`.
- `resolve_fetch`:
  - `Err(Expired)` → `(Indeterminate, None)`.
  - transport error or `Html` → `(Indeterminate, None)`.
  - `body_kind == Rdf` → `(Verified, Some(grade_from_declarations(defs)))`.
  - a 404 (or any 4xx that is not 401/403) → `(Absent, Some(Level(0)))`. A served 404 establishes that nothing is published there.
  - 401 or 403 → `(Indeterminate, None)`: we were refused, which says nothing about what exists.
  - anything else → `(Indeterminate, None)`.
- Replace the old `ProbeKind::FetchWellKnown` arm in `resolve()` so it delegates to `resolve_fetch`, and **delete the comment saying `Verified` is unreachable** — it now is reachable. Keep `resolve()` pure.

- [ ] **Step 4: Run tests, then commit**

```bash
git add prober/src/resolve.rs
git commit -m "feat(prober): resolve declarations and grade a fetched description"
```

---

### Task 5: Wire it into the sweep

**Files:**
- Modify: `prober/src/lib.rs`
- Test: `prober/tests/end_to_end.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `run_sweep` fetches once per endpoint before its metrics, threads `Declarations` into every `resolve` call, and carries a `Level` on the `FetchWellKnown` row.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn the_sweep_fetches_the_description_once_per_endpoint() {
    // One fetch, not one per metric: six metrics must not mean six identical
    // queryless GETs in an operator's log.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/turtle")
            .set_body_string(STUB_TTL))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let queryless = server.received_requests().await.unwrap().iter()
        .filter(|r| r.url.query().is_none()).count();
    assert_eq!(queryless, 1, "expected exactly one queryless fetch per endpoint");
    assert_eq!(rows.len(), defs.len());
}

#[tokio::test]
async fn a_fetched_description_puts_a_level_on_its_row() {
    // ... same two mocks ...
    let row = rows.iter().find(|r| r.metric_id == "service-description").unwrap();
    assert_eq!(row.verdict, Verdict::Verified);
    assert!(row.level.is_some(), "a graded metric must carry its level");
}
```

Use whatever wiremock matcher combination actually distinguishes the queryless request from the query one; if `query_param_is_missing` is unavailable in the pinned version, order two mocks so the more specific matches first, and say in your report which you used.

- [ ] **Step 2: Run them and see them fail**

- [ ] **Step 3: Implement**

In `probe_endpoint`: before the metric loop, call `client.fetch_rdf(ep)` **once**, wrapped in `budget.with_metric_budget`. Turn the outcome into a `Declarations` (empty on any failure) and keep the `Observation` for the `FetchWellKnown` row. Then:
- for each metric, build `Declared::from(&declarations, def)` instead of `Declared { claimed: false }`;
- for the `FetchWellKnown` metric, use `resolve_fetch`'s verdict and level for that row rather than probing again;
- keep every other row's shape unchanged, including `elapsed_ms: None` where nothing was measured.

Delete the `has_probe()` skip for `FetchWellKnown` — it has a probe now. Keep the mechanism for any future kind that does not.

- [ ] **Step 4: Run the whole suite, then the binary**

```bash
cd prober && RUSTFLAGS="-D warnings" cargo test && cargo clippy --all-targets -- -D warnings
cargo run -q -- --at 2026-08-20T15:00:00Z --out /tmp/declared.nq
grep -c 'urn:sparqlwatch:level' /tmp/declared.nq   # expect >= 1 now
```

Expected: at least one level quad, where stage 1 emitted none. Include the counts in your report.

- [ ] **Step 5: Commit**

```bash
git add prober/src/lib.rs prober/tests/end_to_end.rs
git commit -m "feat(prober): fetch once per endpoint and resolve against real declarations"
```

---

### Task 6: Truth-in-documentation

**Files:**
- Modify: `prober/README.md`, `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

- [ ] **Step 1: Update the README**

Record that a service description is fetched with a queryless GET; that `verified` is now reachable but will stay rare because almost no endpoint declares its capabilities; and that graded levels now appear on the `service-description` row.

- [ ] **Step 2: Correct the spec**

The spec's stage-0 findings still say uppercase `HTTP_PROXY` is ignored for `http://` URLs. That is **curl-specific** and does not bind this reqwest client (hyper-util reads both cases). Correct it in place, marking it as corrected rather than deleting the finding, so the reasoning stays visible.

Also update the spec's delivery sequence: stage 1b is being delivered in slices — this plan is declarations and fetching; per-host politeness, concurrency and incremental writes become 1c; registry seeding becomes 1d.

- [ ] **Step 3: Commit**

```bash
git add prober/README.md docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md
git commit -m "docs: declarations are wired, and the proxy finding was curl-specific"
```

---

## Deferred, deliberately

| Requirement | Stage |
|---|---|
| Per-host politeness, `Retry-After`, concurrency caps | 1c |
| Incremental per-endpoint output writing | 1c |
| A cost class on metric definitions | 1c (must exist before seeding) |
| Registry seeding from the LOD Cloud dump and YummyData | 1d |
| An `OPTIONS` preflight probe for the CORS metric | 1c |
| Probing `GRAPH ?g` as well as the default graph for content metrics | 1c |
| Writing into Oxigraph rather than a file | 2 |
| Emitting each metric definition as a `dqv:Metric` description | 2 |

## Self-review notes

- **Spec coverage.** This plan closes three things stage 1 left unreachable: `Declared.claimed` hardcoded, `FetchWellKnown` issuing no request, and `grade_service_description` having no caller. It also retains the response body, which is the first half of the spec's raw-evidence requirement; the rest (request headers, timing detail per measurement) stays deferred and is listed above.
- **The `404 → Absent` decision is the one judgement call worth challenging in review.** Everywhere else this system refuses to claim absence, but a served 404 genuinely establishes that nothing is published at that URL, and treating it as `Indeterminate` would mean no endpoint could ever be told it is missing a description. 401/403 stay `Indeterminate` because refusal is not absence.
- **The expected outcome of this plan is that almost nothing becomes `Verified`.** Two tests assert exactly that. If a reviewer sees many verdicts flip, that is evidence of a bug, not of success.
