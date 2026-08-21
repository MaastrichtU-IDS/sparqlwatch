# Stage 1c-a: Metrics Mean What They Say

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make four published metrics mean exactly what their labels claim, before a 548-endpoint registry sweep starts publishing them at scale.

**Architecture:** No new subsystems. Four independent corrections to existing modules: scope declarations to the service actually probed (`declare.rs`), state the declaration contract honestly (`resolve.rs` docs plus a test that pins the dangerous case), add an `OPTIONS` preflight probe kind alongside the existing simple-GET CORS probe (`client.rs`, `metrics.rs`, `resolve.rs`), and broaden the two graph-scoped metrics to look in named graphs as well as the default graph (`metrics.toml`).

**Tech Stack:** Rust 1.96, edition 2021. tokio, reqwest 0.13, oxrdf 0.3 + oxrdfio 0.2, wiremock 0.6.5. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## Why this plan exists, and why it is half of stage 1c

Stage 1c was scoped as "registry seeding prerequisites" and accumulated eight
items. Four of them make what we already publish more truthful; four of them
(cost classes, per-host politeness, concurrency, incremental writes) make it
safe and affordable to probe 548 endpoints. Those two groups share no code and
neither depends on the other, so they are two plans. This is the first: the
output has to mean what it says before volume makes it authoritative. The
throughput half follows as **1c-b**, and stage 1d (seeding) needs both.

## Global Constraints

- Rust 1.96, edition 2021, no nightly features. **No new dependencies.**
- `resolve()` in `src/resolve.rs` is the single home of judgement and stays a
  pure function. `src/client.rs` returns evidence and holds no opinion.
  `src/emit.rs` is a pure function of its inputs, with no clock read and no
  randomness.
- `--at` is never read from the clock.
- No composite score and no ranking, anywhere.
- **`absent` and `verified` are assertive verdicts.** Either may be published
  only when the evidence establishes it: the endpoint itself answered, with a
  status that speaks to the question, in a form we could parse. Everything else
  is `indeterminate`. When unsure which of the two a case is, it is
  `indeterminate`.
- Every test runs offline against a local `wiremock`. Only `live_smoke` touches
  real endpoints and stays `#[ignore]`d.
- Any new field on `MetricDef` must be added to the canonical string in
  `definitions_revision` **and** covered by
  `the_revision_is_a_pure_function_of_the_definitions`, which now varies every
  field. A published revision that does not change when the measurement changes
  is a silent lie.
- No em-dashes in prose, code comments or Markdown. Use commas, colons,
  parentheses, or separate sentences.
- After any experiment that temporarily edits source, restore it and run
  `touch src/*.rs tests/*.rs` before the final test run. A build artifact newer
  than the restored sources makes `cargo test` report the reverted code, which
  has already cost this project a full debugging cycle.

## Starting state

`main` at the stage 1b merge. 112 tests pass, 1 ignored, clippy clean with
`--all-targets -- -D warnings`.

Relevant existing shapes, verbatim, so no task has to guess them:

```rust
// src/metrics.rs
pub enum ProbeKind { Liveness, Cors, AskFilter, AskData, SelectIris, FetchWellKnown }

pub struct MetricDef {
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub kind: ProbeKind,
    pub query: Option<String>,
    pub expect: Option<bool>,
    pub var: Option<String>,
    pub declared_by: Option<String>,
    pub graded: bool,
}

// src/resolve.rs
pub struct Declared { pub claimed: bool }
impl Declared { pub fn from(defs: &Declarations, def: &MetricDef) -> Declared }

// src/declare.rs
pub struct Declarations {
    pub features: BTreeSet<String>,          // NOT Vec: these are sets, inserted into
    pub extension_functions: BTreeSet<String>,
    pub languages: BTreeSet<String>,
    pub triples: usize,
    pub names_dataset: bool,
    pub has_void_partitions: bool,
    pub has_entailment: bool,
    pub has_example_resources: bool,
}
pub fn parse_declarations(body: &str, content_type: Option<&str>) -> Declarations;

// src/observe.rs
pub struct Observation {
    pub status: Option<u16>,
    pub cors: bool,
    pub boolean: Option<bool>,
    pub bindings: Vec<String>,
    pub body_kind: BodyKind,
    pub elapsed_ms: Option<u64>,
    pub error: Option<String>,
    pub body: Option<String>,
    pub content_type: Option<String>,
}
pub enum BodyKind { SparqlJson, Html, Rdf, Other, None }
```

---

## Task 1: Scope declarations to the service actually probed

A fetched description is reduced to `Declarations` over **every** triple in the
graph. A document describing two services therefore credits the endpoint we
probed with the other service's declarations. The concrete harm: a Fuseki host
serving two datasets, one of which declares
`sd:extensionFunction geof:sfWithin`, makes the *other* dataset report
`verified` or `declared-only` for GeoSPARQL functions it does not have. That is
a confident wrong answer, which is the one thing this system must not do.

The fix is to find the service node whose `sd:endpoint` is the URL we probed and
read declarations from that node's subtree only. Descriptions that name no
`sd:endpoint` at all keep the current graph-wide behaviour: single-service
documents commonly omit it, and narrowing them to nothing would turn every one
of those into a false `undeclared`.

**Files:**
- Modify: `prober/src/declare.rs`
- Modify: `prober/src/lib.rs` (thread the endpoint URL into `parse_declarations`)
- Test: `prober/tests/declare.rs`

**Interfaces:**
- Consumes: `Declarations`, `parse_declarations` as they exist today.
- Produces: `parse_declarations(body: &str, content_type: Option<&str>, endpoint: &str) -> Declarations`.
  The third parameter is the endpoint URL being probed. Task 3 does not touch
  this signature; nothing else calls it.

- [ ] **Step 1: Write the failing test**

Add to `prober/tests/declare.rs`:

```rust
/// A description covering two services must not leak one service's
/// declarations onto the other. This is the shape a Fuseki host with two
/// datasets publishes, and reading it graph-wide credits whichever endpoint we
/// happen to be probing with the union of both.
#[test]
fn a_two_service_description_does_not_credit_the_wrong_endpoint() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/geo> a sd:Service ;
    sd:endpoint <http://example.org/geo/sparql> ;
    sd:extensionFunction geof:sfWithin .
<http://example.org/plain> a sd:Service ;
    sd:endpoint <http://example.org/plain/sparql> .
"#;
    let geo = parse_declarations(DOC, Some("text/turtle"), "http://example.org/geo/sparql");
    assert!(
        geo.declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "the geo service really does declare sfWithin"
    );

    let plain = parse_declarations(DOC, Some("text/turtle"), "http://example.org/plain/sparql");
    assert!(
        !plain.declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "the plain service declares nothing, and must not inherit its neighbour's function"
    );
}

/// The fallback that keeps single-service descriptions working. Most real
/// descriptions, including the 21 byte-identical Virtuoso stubs in the survey,
/// do not state `sd:endpoint` at all. Scoping those to nothing would turn every
/// one of them into a false `undeclared`.
#[test]
fn a_description_naming_no_endpoint_is_still_read_whole() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/svc> a sd:Service ;
    sd:extensionFunction geof:sfWithin .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/anything");
    assert!(
        d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "no sd:endpoint anywhere means we cannot scope, so read it whole"
    );
}

/// A URL that differs only by a trailing slash is the same endpoint. Registry
/// URLs and published `sd:endpoint` values disagree about it constantly.
#[test]
fn a_trailing_slash_does_not_defeat_the_scope_match() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix geof: <http://www.opengis.net/def/function/geosparql/> .
<http://example.org/geo> a sd:Service ;
    sd:endpoint <http://example.org/geo/sparql/> ;
    sd:extensionFunction geof:sfWithin .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/geo/sparql");
    assert!(
        d.declares("http://www.opengis.net/def/function/geosparql/sfWithin"),
        "the trailing slash is not a different service"
    );
}

/// The dataset a scoped service points at is part of that service's subtree, so
/// VoID partitions hanging off it must still be counted. Reading only triples
/// whose subject is the service node would silently drop them and regrade every
/// scoped description.
#[test]
fn a_scoped_services_dataset_is_still_reached_for_void_partitions() {
    const DOC: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
@prefix void: <http://rdfs.org/ns/void#> .
<http://example.org/svc> a sd:Service ;
    sd:endpoint <http://example.org/sparql> ;
    sd:defaultDataset <http://example.org/ds> .
<http://example.org/ds> a sd:Dataset ;
    void:classPartition [ void:class <http://example.org/C> ] .
"#;
    let d = parse_declarations(DOC, Some("text/turtle"), "http://example.org/sparql");
    assert!(d.has_void_partitions, "the dataset is inside the scoped service's subtree");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --test declare`
Expected: compile error, `parse_declarations` takes 2 arguments not 3. That is
the failure. Once the signature is widened, expect
`a_two_service_description_does_not_credit_the_wrong_endpoint` to fail on the
`plain` assertion, because today's graph-wide read credits it.

- [ ] **Step 3: Implement the scoping**

In `prober/src/declare.rs`, widen the signature and select the subject set
before extracting. Sketch, to be adapted to the existing extraction code rather
than pasted over it:

```rust
const SD_ENDPOINT: &str = "http://www.w3.org/ns/sparql-service-description#endpoint";
const SD_DEFAULT_DATASET: &str = "http://www.w3.org/ns/sparql-service-description#defaultDataset";
const SD_AVAILABLE_GRAPHS: &str = "http://www.w3.org/ns/sparql-service-description#availableGraphs";
const SD_NAMED_GRAPH: &str = "http://www.w3.org/ns/sparql-service-description#namedGraph";
const SD_GRAPH: &str = "http://www.w3.org/ns/sparql-service-description#graph";

/// Two endpoint URLs are the same service if they differ only by a trailing
/// slash. Registry URLs and published `sd:endpoint` values disagree about that
/// constantly, and treating them as different services would scope a real
/// description down to nothing.
fn same_endpoint(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The subjects whose triples describe the service at `endpoint`: the service
/// node itself, plus the dataset and graph nodes it points at, transitively.
/// Returns `None` when the document names no `sd:endpoint` at all, meaning we
/// cannot scope and must read the whole graph.
fn scope_subjects(triples: &[Triple], endpoint: &str) -> Option<HashSet<String>> {
    let mut any_endpoint_stated = false;
    let mut roots: HashSet<String> = HashSet::new();
    for t in triples {
        if t.predicate.as_str() == SD_ENDPOINT {
            any_endpoint_stated = true;
            if let Term::NamedNode(o) = &t.object {
                if same_endpoint(o.as_str(), endpoint) {
                    roots.insert(t.subject.to_string());
                }
            }
        }
    }
    if !any_endpoint_stated {
        return None;
    }
    // Walk the linking predicates so a dataset's VoID partitions stay in scope.
    // Bounded by the triple count, so a cyclic document cannot spin here.
    let linking = [SD_DEFAULT_DATASET, SD_AVAILABLE_GRAPHS, SD_NAMED_GRAPH, SD_GRAPH];
    loop {
        let before = roots.len();
        for t in triples {
            if linking.contains(&t.predicate.as_str()) && roots.contains(&t.subject.to_string()) {
                roots.insert(t.object.to_string());
            }
        }
        if roots.len() == before {
            break;
        }
    }
    Some(roots)
}
```

**This requires restructuring the parse, so read this before writing code.**
`parse_declarations` today is a SINGLE streaming pass: it iterates the parser's
quads and extracts each one inline as it arrives. Scoping cannot work that way,
because the `sd:endpoint` triple that decides the scope may arrive after the
declarations it governs. So:

1. Collect the parsed quads into a `Vec` first. Memory is bounded by the body
   cap (256 KiB), so this is safe, but say so in a comment or the next reader
   will assume it is unbounded.
2. Preserve the existing error semantics exactly: on a parse error, `break` and
   keep everything collected so far. `prober/tests/declare.rs` already pins
   this with a mid-parse-error fixture, and that test must keep passing.
3. Count `d.triples` during collection, over the whole document, not over the
   scoped subset (see the note below).
4. Compute the scope set from the collected quads.
5. Extract declarations in a second pass, skipping any quad whose subject is not
   in the scope set when a scope set exists. Blank-node subjects reached through
   a linking predicate are in scope by the same rule, which is what makes the
   `void:classPartition [ void:class ... ]` case work.

The parser yields `Quad`, not `Triple`, so the sketch's `&[Triple]` is
`&[Quad]`, and `t.predicate` / `t.object` / `t.subject` read the same. Import
`std::collections::HashSet` alongside the existing `BTreeSet`.

`triples` counts the whole document, not the scoped subtree: it feeds the
service-description grade, which is about how informative the published document
is, and an operator publishing one document for two services published all of
it. Add a comment saying so, because the asymmetry looks like a bug otherwise.

- [ ] **Step 4: Thread the endpoint through the one call site**

`prober/src/lib.rs` calls `parse_declarations(o.body..., o.content_type...)`.
Pass the endpoint URL already in scope there. No other call sites exist.

- [ ] **Step 5: Run the tests**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: all four new tests pass, and the pre-existing 112 still pass. If any
existing declaration test now fails, the fallback in Step 3 is wrong: existing
fixtures state no `sd:endpoint`, so they must take the read-it-whole path.

- [ ] **Step 6: Prove the scoping is load-bearing**

Temporarily make `scope_subjects` always return `None`. Expected:
`a_two_service_description_does_not_credit_the_wrong_endpoint` fails on the
`plain` assertion. Restore, then `touch src/*.rs tests/*.rs` before re-running.

- [ ] **Step 7: Commit**

```bash
git add prober/src/declare.rs prober/src/lib.rs prober/tests/declare.rs
git commit -m "fix(prober): read declarations from the service we probed, not the whole document"
```

---

## Task 2: State the declaration contract honestly

I had planned a tri-state `Declared` here, so that "we never read their
description" would differ from "their description declares nothing". Reading the
code changed my mind, and the reasoning belongs in the repo rather than in a
ledger.

`Declared { claimed: bool }` is `false` in both cases, and the question is what
verdict that produces. It cannot produce `declared-only` or `declared-but-wrong`,
since both assert a declaration exists. It produces `undeclared-but-verified`
when a probe confirms the capability. The spec fixes the verdict vocabulary at
six, so distinguishing the two cases would mean either a seventh verdict or an
extra field, and **the distinction is already published**: the
`service-description` row for the same endpoint in the same run says whether we
could read the description at all. A consumer joining the two rows already has
the answer.

So the honest fix is not more machinery, it is accurate wording plus a test that
pins the case that would actually hurt.

**Files:**
- Modify: `prober/src/resolve.rs` (doc comments on `Declared` and the verdict)
- Modify: `prober/README.md` (the verdict table's `undeclared-but-verified` row)
- Test: `prober/tests/end_to_end.rs`

**Interfaces:** unchanged. No signature moves in this task.

- [ ] **Step 1: Write the failing test**

Add to `prober/tests/end_to_end.rs`:

```rust
/// The one thing an unreadable description must never do is manufacture a
/// declaration. If the fetch fails, every declaration-backed metric has to fall
/// on the undeclared side: `declared-only` and `declared-but-wrong` both assert
/// that the endpoint claimed something, and we did not read any claim.
#[tokio::test]
async fn an_unreadable_description_never_manufactures_a_declaration() {
    let server = MockServer::start().await;

    // The description fetch fails outright.
    Mock::given(method("GET"))
        .and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    // Every query answers, so capabilities are genuinely confirmable.
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/sparql-results+json")
                .set_body_raw(r#"{"head":{},"boolean":true}"#, "application/sparql-results+json"),
        )
        .mount(&server)
        .await;

    let rows = sweep_against(&server).await;
    assert!(!rows.is_empty(), "the fixture must produce rows or it proves nothing");
    for r in &rows {
        assert_ne!(
            r.verdict, Verdict::DeclaredOnly,
            "metric {} claimed a declaration from a description we could not read",
            r.metric_id
        );
        assert_ne!(
            r.verdict, Verdict::DeclaredButWrong,
            "metric {} claimed a declaration from a description we could not read",
            r.metric_id
        );
    }
    // And the run still says, in the graph, why declarations are missing.
    let sd = rows.iter().find(|r| r.metric_id == "service-description")
        .expect("the service-description row is how a consumer learns the description was unreadable");
    assert_eq!(sd.verdict, Verdict::Indeterminate);
}
```

Use whatever mock-and-sweep helper the existing tests in this file already use
rather than inventing a second one; `query_param_is_missing` is the wiremock
matcher the fetch tests use to separate the queryless GET from the query
requests. If the existing helper does not expose `metric_id` on rows, assert
over `MeasurementRow` fields directly.

- [ ] **Step 2: Run it to verify it passes for the right reason**

Run: `cargo test --manifest-path prober/Cargo.toml --test end_to_end`

This test is expected to **pass immediately**: today's `claimed: false` already
prevents both verdicts. That is fine and it is the point. It is a regression pin
on a property that currently holds by accident of a boolean, so that a future
tri-state refactor cannot quietly break it. Prove it is load-bearing in Step 4
rather than assuming it.

- [ ] **Step 3: Correct the wording in both places**

`prober/src/resolve.rs`, on `Declared`: state that `claimed: false` means "no
declaration was seen", which covers both "the description declares nothing" and
"the description could not be read", and that the two are distinguished by the
`service-description` row rather than by this flag. Say why that is enough: the
verdict vocabulary is fixed at six by the spec, and the information is already
published per endpoint per run.

`prober/README.md`, the verdict table row for `undeclared-but-verified`:
currently "Works, but the endpoint advertises nothing". That overstates when we
never read the description. Reword to "Works; no declaration was seen (see the
`service-description` row for whether the description was readable)".

- [ ] **Step 4: Prove the test is load-bearing**

Temporarily change `Declared::from` to return `Declared { claimed: true }`
unconditionally. Expected: the new test fails, reporting a `declared-only` or
`declared-but-wrong` verdict for a metric whose declaration was never read.
Restore, then `touch src/*.rs tests/*.rs` before re-running.

- [ ] **Step 5: Commit**

```bash
git add prober/src/resolve.rs prober/README.md prober/tests/end_to_end.rs
git commit -m "docs(prober): say what an absent declaration actually means, and pin it"
```

---

## Task 3: An `OPTIONS` preflight probe, alongside the simple-GET one

`metrics.toml` states the problem itself: the `cors` metric observes an
`access-control-allow-origin` header on a **simple GET**, which is weaker than
what a browser editor does. A real cross-origin SPARQL query is preflighted, so
an endpoint that sets the header on GET but refuses `OPTIONS` passes this metric
and still fails in the embedded editor stage 3b ships. That is a metric whose
label is true and whose usefulness is a lie.

Add a second probe kind rather than replacing the first. Both facts are worth
publishing: the simple-GET header is what a `curl` user sees, the preflight is
what a browser sees, and an endpoint can genuinely have one and not the other.

**Files:**
- Modify: `prober/src/metrics.rs` (new `ProbeKind::CorsPreflight`)
- Modify: `prober/src/client.rs` (an `OPTIONS` request)
- Modify: `prober/src/observe.rs` (record the two preflight response headers)
- Modify: `prober/src/resolve.rs` (resolve the new kind)
- Modify: `prober/src/lib.rs` (dispatch the new kind)
- Modify: `prober/metrics.toml` (the new metric, and drop the stale deferral comment)
- Test: `prober/tests/cors_preflight.rs` (new file)

**Interfaces:**
- Consumes: `Client`, `Observation`, `resolve()` as they exist after Task 1.
- Produces:
  - `ProbeKind::CorsPreflight`
  - `Client::preflight(&self, url: &str) -> Observation`
  - `Observation.allow_origin: Option<String>`,
    `Observation.allow_methods: Option<String>` and
    `Observation.allow_headers: Option<String>`, all `None` for every probe that
    is not a preflight. Add them to every `Observation` construction site,
    including `Observation::failed`, or the crate will not compile.
    `allow_origin` carries the header's VALUE, because the existing
    `Observation.cors` bool records only presence and presence is not a grant
    (see the verdict table).

- [ ] **Step 1: Write the failing tests**

Create `prober/tests/cors_preflight.rs`. The verdict rules, which the tests
encode, are:

| Preflight response | Verdict | Why |
|---|---|---|
| 2xx, `access-control-allow-origin` present, `access-control-allow-methods` contains `GET` or is `*` | `Verified` | A browser would proceed |
| 2xx, `access-control-allow-origin` present, methods header absent | `Verified` | The methods header is optional for simple methods; the origin grant is the load-bearing part |
| 2xx, no `access-control-allow-origin` | `Absent` | The endpoint answered the preflight and granted nothing |
| 2xx, `access-control-allow-origin` present but neither `*` nor our own origin | `Absent` | A grant to somebody else is not a grant to us. Checking only for the header's presence would publish `verified` for an endpoint that allowlists one unrelated site |
| 405 or 501, **regardless of CORS headers** | `Absent` | Fetch requires a preflight to answer with an ok status, so a 405 fails the preflight whatever headers ride along. Do not make this row conditional on the headers being absent |
| any other non-2xx, or a transport error, or an expired budget | `Indeterminate` | Tells us about our request or the server's state, not about its CORS policy |

```rust
mod support; // if the other integration tests share a helper module; otherwise inline

use sparqlwatch_prober::observe::Observation;
use sparqlwatch_prober::verdict::Verdict;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_preflight_that_grants_the_origin_and_get_is_verified() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"),
        )
        .mount(&server)
        .await;
    let (verdict, _) = preflight_and_resolve(&server.uri()).await;
    assert_eq!(verdict, Verdict::Verified);
}

#[tokio::test]
async fn a_preflight_refused_with_405_is_absent_because_the_endpoint_answered() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    let (verdict, _) = preflight_and_resolve(&server.uri()).await;
    assert_eq!(
        verdict,
        Verdict::Absent,
        "a 405 to OPTIONS is the endpoint telling us preflight fails, which is what breaks a browser"
    );
}

#[tokio::test]
async fn a_preflight_answered_without_cors_headers_is_absent() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let (verdict, _) = preflight_and_resolve(&server.uri()).await;
    assert_eq!(verdict, Verdict::Absent);
}

#[tokio::test]
async fn a_throttled_or_broken_preflight_is_indeterminate_not_absent() {
    for status in [429u16, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("OPTIONS"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let (verdict, _) = preflight_and_resolve(&server.uri()).await;
        assert_eq!(
            verdict,
            Verdict::Indeterminate,
            "status {status} describes the server's state, not its CORS policy"
        );
    }
}

#[tokio::test]
async fn a_preflight_granting_only_post_does_not_claim_get_works() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "POST"),
        )
        .mount(&server)
        .await;
    let (verdict, _) = preflight_and_resolve(&server.uri()).await;
    assert_eq!(
        verdict,
        Verdict::Absent,
        "a methods header that excludes GET is a stated refusal of the request we would make"
    );
}

#[tokio::test]
async fn the_preflight_announces_an_origin_and_the_method_it_intends() {
    // Without these headers the request is not a preflight and a correct server
    // is entitled to ignore it, which would make every verdict above garbage.
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(wiremock::matchers::header_exists("origin"))
        .and(wiremock::matchers::header("access-control-request-method", "GET"))
        .respond_with(ResponseTemplate::new(204).insert_header("access-control-allow-origin", "*"))
        .mount(&server)
        .await;
    let (verdict, _) = preflight_and_resolve(&server.uri()).await;
    assert_eq!(
        verdict,
        Verdict::Verified,
        "the mock only matches a real preflight, so a non-preflight request fails this"
    );
}
```

Write `preflight_and_resolve` as a small local helper in this file that builds a
`Client` with the default `Budget`, calls `Client::preflight`, and feeds the
`Observation` to `resolve()` with the `CorsPreflight` metric definition. Return
`(Verdict, Observation)` so a test can assert on evidence when it needs to.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path prober/Cargo.toml --test cors_preflight`
Expected: compile failure, no `ProbeKind::CorsPreflight` and no
`Client::preflight`.

- [ ] **Step 3: Add the observation fields**

In `prober/src/observe.rs`, add to `Observation`:

```rust
    /// `access-control-allow-methods` from a preflight response. `None` for
    /// every probe that is not a preflight, and `None` when the server sent no
    /// such header, which are different facts only in context: a preflight that
    /// answered without it is a grant of the simple methods.
    pub allow_methods: Option<String>,
    /// `access-control-allow-headers` from a preflight response. Recorded as
    /// evidence for a later stage: an endpoint that grants the origin but not
    /// `content-type` still fails a POSTed query from a browser.
    pub allow_headers: Option<String>,
```

Set both to `None` at every existing construction site, `Observation::failed`
included.

- [ ] **Step 4: Add the client request**

In `prober/src/client.rs`:

```rust
    /// A CORS preflight, the request a browser sends before a cross-origin
    /// query. This is the only probe besides `cors` that announces an `Origin`,
    /// and unlike that one it also announces the method and headers the real
    /// request would carry: without `access-control-request-method` this is not
    /// a preflight at all, and a correct server may ignore it.
    pub async fn preflight(&self, url: &str) -> Observation {
        let start = Instant::now();
        let resp = self
            .http
            .request(reqwest::Method::OPTIONS, url)
            .header("Origin", ORIGIN)
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "content-type")
            .send()
            .await;
        let elapsed = start.elapsed().as_millis() as u64;
        // ... same error handling as the other probes: a transport failure is
        // `Observation::failed`, never a status.
    }
```

Record `status`, `cors` (whether `access-control-allow-origin` is present),
`allow_origin` (its value), `allow_methods`, `allow_headers`, and `elapsed_ms`.
Do not read or classify the body: a preflight has none worth reading, and asking
for one invites a `BodyKind` judgement this probe has no use for. Set
`body_kind` to `BodyKind::None`.

Reuse the existing `ORIGIN` module constant in `client.rs:8`, so both CORS
probes announce the same origin: two probes announcing different origins would
measure two different policies.

**Fix `ORIGIN` while you are here.** It is currently
`"https://sparqlwatch.example"`, a placeholder that does not resolve. We
announce it to every endpoint we probe, and an operator who maintains an origin
allowlist cannot allowlist a domain that does not exist. Change it to
`"https://sparqlwatch.dev.k8s.semanticscience.org"`, the decided public domain,
matching the `User-Agent` set a few lines below it. This is the same defect as
the placeholder `User-Agent` already corrected in commit `8c22c1e`, in the
other header we send strangers.

- [ ] **Step 5: Resolve the new kind**

In `prober/src/resolve.rs`, add a `ProbeKind::CorsPreflight` arm implementing
the table from Step 1. Put the method check in a named helper so the intent is
readable and testable:

```rust
/// Whether an `access-control-allow-methods` value permits the GET we would
/// send. An absent header is a grant: the header is optional, and a preflight
/// that answered without it has not refused anything. An empty header is not a
/// grant, because the server stated a list and GET is not in it.
fn allows_get(allow_methods: Option<&str>) -> bool {
    match allow_methods {
        None => true,
        Some(v) => v
            .split(',')
            .map(|m| m.trim())
            .any(|m| m == "*" || m.eq_ignore_ascii_case("GET")),
    }
}
```

Add unit tests for `allows_get` covering: `None`, `"*"`, `"GET"`, `"get"`,
`"GET, POST"`, `"POST"`, `""`, and `"POSTGET"` (which must not match, and is the
case a naive `contains` gets wrong).

Add the matching helper for the origin, with its own unit tests:

```rust
/// Whether an `access-control-allow-origin` value grants OUR origin. A wildcard
/// grants everyone; an exact echo of our own origin grants us. Anything else is
/// a grant to somebody else, and reporting it as ours would publish `verified`
/// for an endpoint that would refuse us in a browser.
fn grants_our_origin(allow_origin: Option<&str>) -> bool {
    match allow_origin {
        None => false,
        Some(v) => {
            let v = v.trim();
            v == "*" || v.eq_ignore_ascii_case(ORIGIN)
        }
    }
}
```

Cover: `None`, `"*"`, our exact origin, our origin with different case,
`"https://example.com"` (must be false), and `""` (must be false). `ORIGIN`
lives in `client.rs`; either re-export it or mirror it as a shared constant
rather than retyping the literal in `resolve.rs`, so the two cannot drift.

- [ ] **Step 6: Dispatch it and define the metric**

`prober/src/lib.rs`: route `ProbeKind::CorsPreflight` to `client.preflight(ep)`.
It takes no `query`, like `FetchWellKnown`.

`prober/metrics.toml`: add the metric, and rewrite the stale comment above
`cors` so it no longer says the preflight probe is deferred:

```toml
# Two CORS facts, deliberately separate. `cors` is what a curl user sees: an
# access-control-allow-origin header on a simple GET. `cors-preflight` is what a
# browser sees, and it is the one that decides whether the embedded editor can
# talk to this endpoint at all. An endpoint can genuinely have one and not the
# other, so neither subsumes the other.
[[metric]]
id = "cors-preflight"
label = "Answers a CORS preflight for a cross-origin GET"
dimension = "interoperability"
kind = "CorsPreflight"
```

Note that adding a metric changes `metricDefinitionRevision`, which is correct
and expected: the definition list really did change.

- [ ] **Step 7: Run everything**

Run: `cargo test --manifest-path prober/Cargo.toml`
Run: `cargo clippy --manifest-path prober/Cargo.toml --all-targets -- -D warnings`
Expected: all pass. Existing end-to-end tests that assert a row count or a
measurement total will need updating for the extra metric; update the expected
numbers, do not delete the assertions.

- [ ] **Step 8: Commit**

```bash
git add prober/src prober/metrics.toml prober/tests/cors_preflight.rs
git commit -m "feat(prober): probe the CORS preflight a browser actually sends"
```

---

## Task 4: Look in named graphs, not only the default graph

`metrics.toml` states this problem too: `geo-data` and `classes` query only the
default graph. On an engine where the default graph is not the union of the
named graphs, an endpoint holding everything in named graphs answers empty, and
we publish `absent`. That is a confident wrong answer about an endpoint full of
exactly the content we said it lacks, and it is the single most likely false
`absent` in the whole metric set once seeding starts.

Broaden both queries to a UNION over the default graph and named graphs, and
rename the labels to match. There is no published history to preserve: no
production sweep has run, so changing what these two metrics mean costs nothing
today and gets much more expensive after 1d.

**Files:**
- Modify: `prober/metrics.toml`
- Test: `prober/tests/end_to_end.rs`

**Interfaces:** none. This task changes data, not code, which is the point of
metrics being data.

- [ ] **Step 1: Write the failing test**

The query text is now load-bearing, so pin it. Add to
`prober/tests/end_to_end.rs`:

```rust
/// The default-graph-only versions of these two queries publish `absent` for an
/// endpoint that holds all its data in named graphs, which is a confident wrong
/// answer and the most likely false `absent` in the metric set. Pin that both
/// queries reach named graphs, by reading the shipped definitions rather than a
/// fixture, so editing the file cannot silently narrow them again.
#[test]
fn the_content_metrics_reach_named_graphs() {
    let defs = sparqlwatch_prober::metrics::load_metrics(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/metrics.toml")).unwrap(),
    )
    .unwrap();
    for id in ["geo-data", "classes"] {
        let d = defs.iter().find(|d| d.id == id).expect("metric must exist");
        let q = d.query.as_deref().unwrap_or("");
        assert!(
            q.contains("GRAPH"),
            "{id} must look in named graphs too, or it publishes absent for graph-partitioned endpoints"
        );
        assert!(
            q.to_uppercase().contains("UNION"),
            "{id} must still look in the default graph as well, not only in named graphs"
        );
        assert!(
            !d.label.to_lowercase().contains("default graph"),
            "{id}'s label still claims default-graph-only scope"
        );
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path prober/Cargo.toml --test end_to_end the_content_metrics_reach_named_graphs`
Expected: FAIL, the shipped queries contain no `GRAPH`.

- [ ] **Step 3: Broaden the two queries**

In `prober/metrics.toml`, replace the two metrics and the comment above them:

```toml
# Both queries look in the default graph AND in named graphs. Default-graph-only
# versions published `absent` for an endpoint holding everything in named
# graphs, which is a confident wrong answer about an endpoint full of exactly
# the content we said it lacked. The UNION costs more on a large endpoint, which
# is what the cost class in stage 1c-b is for.
[[metric]]
id = "geo-data"
label = "Holds WKT geometry"
dimension = "content"
kind = "AskData"
var = "g"
query = """
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
SELECT ?g WHERE { { ?s geo:asWKT ?g } UNION { GRAPH ?anyg { ?s geo:asWKT ?g } } } LIMIT 1
"""

[[metric]]
id = "classes"
label = "Distinct classes"
dimension = "content"
kind = "SelectIris"
var = "c"
query = """
SELECT DISTINCT ?c WHERE { { ?s a ?c } UNION { GRAPH ?anyg { ?s a ?c } } } LIMIT 200
"""
```

The graph variable is `?anyg`, not `?g`: `geo-data` binds `?g` as its result
variable, and reusing the name would join the geometry literal against the graph
name and return nothing. That would be a silent false `absent`, the exact bug
this task exists to remove. Keep the names distinct and say so in the file.

- [ ] **Step 4: Run the tests**

Run: `cargo test --manifest-path prober/Cargo.toml`
Expected: the new test passes and the suite stays green. The revision hash
changes, so any test asserting a literal revision string must be updated to the
new value, not deleted.

- [ ] **Step 5: Verify against a real endpoint, not only a mock**

Run: `cargo run --manifest-path prober/Cargo.toml -- --at 2026-08-21T12:00:00Z --out /tmp/run-1ca.nq`

Then read the `geo-data` and `classes` rows for all three endpoints. Expected:
no row moves from a positive verdict to `absent` or `indeterminate`. A UNION
that times out where the simple query succeeded is a real finding worth
reporting rather than hiding: record it in your report with the endpoint and the
elapsed time. Do not change the query to make the number look better.

- [ ] **Step 6: Commit**

```bash
git add prober/metrics.toml prober/tests/end_to_end.rs
git commit -m "fix(prober): look in named graphs, so a partitioned endpoint is not reported empty"
```

---

## Done criteria

- All four tasks committed, suite green, clippy clean with `--all-targets -- -D warnings`.
- A real sweep runs and no endpoint's verdict moved from positive to negative
  except where the report explains why.
- `prober/README.md`'s "Known limitations" no longer lists declaration scoping,
  the simple-GET-only CORS caveat, or the default-graph-only caveat, because all
  three are now fixed. The truncation limitation stays, and so does anything
  1c-b owns.
- `prober/metrics.toml` contains no comment claiming a probe is deferred when it
  now exists.
