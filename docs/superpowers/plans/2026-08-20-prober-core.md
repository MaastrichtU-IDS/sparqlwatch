# Prober Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust binary that probes a list of SPARQL endpoints, resolves each attribute to one of six verdicts, and writes DQV/PROV measurements as N-Quads.

**Architecture:** One `probe` per (endpoint, metric). Each probe returns raw *observations* only. A separate resolution step turns observations plus declarations into a verdict, so judgement is never baked into collection. Timeouts are enforced by cancellation at three nesting levels (request, metric, endpoint) rather than by inspecting elapsed time between steps. Output is append-only N-Quads in one named graph per run.

**Tech Stack:** Rust 1.96 (edition 2021), tokio, reqwest, oxrdf + oxrdfio, wiremock for tests.

**Spec:** `docs/superpowers/specs/2026-08-19-sparql-endpoint-monitor-design.md`

## Global Constraints

- **Rust 1.96.0, edition 2021.** No nightly features.
- **Exact dependency pins, verified to resolve and compile together on 2026-08-20:**
  ```toml
  oxrdf = "0.3"        # 0.3.3
  oxrdfio = "0.2"      # 0.2.5 — pins oxrdf "=0.3.3" internally
  reqwest = { version = "0.13", default-features = false, features = ["rustls", "gzip", "query", "system-proxy"] }
  tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
  serde = { version = "1", features = ["derive"] }
  serde_json = "1"
  toml = "1"
  clap = { version = "4", features = ["derive"] }
  anyhow = "1"
  thiserror = "2"
  tracing = "0.1"
  tracing-subscriber = "0.3"

  [dev-dependencies]
  wiremock = "0.6"
  ```
- **`oxrdfio` and `oxrdf` are NOT version-aligned.** `oxrdfio = "0.3"` does not exist; `oxrdfio = "0.2"` with `oxrdf = "0.2"` fails to compile with "multiple different versions of crate `oxrdf`". The only working pair is `oxrdf = "0.3"` + `oxrdfio = "0.2"`.
- **reqwest 0.13 renamed its features.** `rustls-tls` does not exist; use `rustls`. Reading `HTTP_PROXY`/`HTTPS_PROXY` from the environment requires the **`system-proxy`** feature, which is mandatory here because ids3 pods have no direct internet.
- **`oxrdf::Subject` is deprecated.** Use `NamedOrBlankNode`.
- **No JVM, ever.** Rules out `void-generator` and any Java tooling.
- **Verdict vocabulary is closed**: `Verified`, `UndeclaredButVerified`, `DeclaredButWrong`, `DeclaredOnly`, `Absent`, `Indeterminate`. A timeout is always `Indeterminate`, never `Absent`.
- **User-Agent** on every request: `sparqlwatch/<version> (+<about-url>)`. Per-host concurrency is 1.
- **A probe never computes a verdict.** It returns observations.

---

## File Structure

| File | Responsibility |
|---|---|
| `prober/Cargo.toml` | Manifest with the pins above |
| `prober/src/lib.rs` | Crate root, re-exports |
| `prober/src/verdict.rs` | The six-verdict enum, its severity ordering, and graded levels |
| `prober/src/budget.rs` | Three-level cancellable timeout wrapper |
| `prober/src/client.rs` | HTTP/SPARQL client: liveness, CORS, ASK, SELECT |
| `prober/src/observe.rs` | Observation types returned by probes |
| `prober/src/metrics.rs` | Metric definitions loaded from TOML, probe-kind dispatch |
| `prober/src/resolve.rs` | Observations + declarations -> verdict |
| `prober/src/emit.rs` | DQV/PROV N-Quads serialization |
| `prober/src/main.rs` | CLI wiring |
| `prober/metrics.toml` | The seed metric definitions |
| `prober/tests/client.rs` | Client tests against wiremock |
| `prober/tests/end_to_end.rs` | Full run against wiremock, N-Quads asserted |
| `prober/tests/live_smoke.rs` | `#[ignore]` tests against real endpoints |

---

### Task 1: Scaffold and the verdict vocabulary

**Files:**
- Create: `prober/Cargo.toml`, `prober/src/lib.rs`, `prober/src/verdict.rs`
- Test: inline `#[cfg(test)]` in `prober/src/verdict.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `enum Verdict` with variants `Verified`, `UndeclaredButVerified`, `DeclaredButWrong`, `DeclaredOnly`, `Absent`, `Indeterminate`; `Verdict::severity(&self) -> u8`; `Verdict::slug(&self) -> &'static str`; `struct Level(pub u8)` with `Level::new(u8) -> Option<Level>`.

- [ ] **Step 1: Create the manifest**

```toml
# prober/Cargo.toml
[package]
name = "sparqlwatch-prober"
version = "0.1.0"
edition = "2021"
rust-version = "1.96"

[dependencies]
oxrdf = "0.3"
oxrdfio = "0.2"
reqwest = { version = "0.13", default-features = false, features = ["rustls", "gzip", "query", "system-proxy"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = "0.3"

[dev-dependencies]
wiremock = "0.6"

[lib]
name = "sparqlwatch_prober"
path = "src/lib.rs"

[[bin]]
name = "sparqlwatch-prober"
path = "src/main.rs"
```

- [ ] **Step 2: Write the failing test**

```rust
// prober/src/verdict.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_but_wrong_is_more_severe_than_absent() {
        // A false claim misleads a client that trusts it, so it ranks worse
        // than simply not having the capability.
        assert!(Verdict::DeclaredButWrong.severity() > Verdict::Absent.severity());
    }

    #[test]
    fn verified_is_least_severe() {
        for v in Verdict::ALL {
            assert!(Verdict::Verified.severity() <= v.severity());
        }
    }

    #[test]
    fn slugs_are_stable_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for v in Verdict::ALL {
            assert!(seen.insert(v.slug()), "duplicate slug {}", v.slug());
        }
        assert_eq!(Verdict::UndeclaredButVerified.slug(), "undeclared-but-verified");
    }

    #[test]
    fn levels_are_bounded_to_zero_through_four() {
        assert!(Level::new(4).is_some());
        assert!(Level::new(5).is_none());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd prober && cargo test verdict 2>&1 | tail -20`
Expected: FAIL, `cannot find type Verdict in this scope`.

- [ ] **Step 4: Write minimal implementation**

```rust
// prober/src/verdict.rs
use serde::{Deserialize, Serialize};

/// The closed verdict vocabulary. Every state here was observed in the LOD
/// Cloud survey (see the spec's Conformance model section); none is
/// speculative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Verdict {
    /// A probe confirms it works, and it is declared.
    Verified,
    /// Works, but the endpoint advertises nothing. The common case: 18
    /// endpoints evaluate geof:sfWithin and none declares it.
    UndeclaredButVerified,
    /// Claimed or bound, but behaves incorrectly. 9 endpoints answered a
    /// point-in-polygon filter `false` when a conformant engine must say true.
    DeclaredButWrong,
    /// Claimed, not confirmable by probe.
    DeclaredOnly,
    /// Neither claimed nor observed.
    Absent,
    /// The engine or the time budget prevented an answer. Never a silent zero.
    Indeterminate,
}

impl Verdict {
    pub const ALL: [Verdict; 6] = [
        Verdict::Verified,
        Verdict::UndeclaredButVerified,
        Verdict::DeclaredOnly,
        Verdict::Absent,
        Verdict::DeclaredButWrong,
        Verdict::Indeterminate,
    ];

    /// Lower is better. Used only for ordering a worklist, never published as
    /// a score.
    pub fn severity(&self) -> u8 {
        match self {
            Verdict::Verified => 0,
            Verdict::UndeclaredButVerified => 1,
            Verdict::DeclaredOnly => 2,
            Verdict::Absent => 3,
            Verdict::DeclaredButWrong => 4,
            Verdict::Indeterminate => 5,
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::UndeclaredButVerified => "undeclared-but-verified",
            Verdict::DeclaredButWrong => "declared-but-wrong",
            Verdict::DeclaredOnly => "declared-only",
            Verdict::Absent => "absent",
            Verdict::Indeterminate => "indeterminate",
        }
    }
}

/// A graded conformance level, 0..=4. Used where a boolean would credit the
/// engine rather than the publisher, e.g. service-description informativeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level(pub u8);

impl Level {
    pub fn new(v: u8) -> Option<Level> {
        if v <= 4 { Some(Level(v)) } else { None }
    }
}
```

```rust
// prober/src/lib.rs
pub mod verdict;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd prober && cargo test verdict 2>&1 | tail -10`
Expected: PASS, 4 passed.

- [ ] **Step 6: Commit**

```bash
git add prober/Cargo.toml prober/Cargo.lock prober/src/lib.rs prober/src/verdict.rs
git commit -m "feat(prober): verdict vocabulary and graded levels"
```

---

### Task 2: Three-level cancellable budgets

**Files:**
- Create: `prober/src/budget.rs`
- Modify: `prober/src/lib.rs`
- Test: inline `#[cfg(test)]` in `prober/src/budget.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `struct Budget { pub request: Duration, pub metric: Duration, pub endpoint: Duration }`; `Budget::default()`; `async fn with_metric_budget<F, T>(&self, f: F) -> Result<T, Expired>` where `F: Future<Output = T>`; `struct Expired;`.

- [ ] **Step 1: Write the failing test**

```rust
// prober/src/budget.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_slow_future_is_cancelled_not_awaited() {
        let b = Budget { request: Duration::from_millis(10), metric: Duration::from_millis(20), endpoint: Duration::from_millis(50) };
        let start = std::time::Instant::now();
        let out = b.with_metric_budget(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            7
        }).await;
        // The point of the whole module: the budget must interrupt the work,
        // not merely report afterwards that it took too long.
        assert!(out.is_err());
        assert!(start.elapsed() < Duration::from_secs(1), "budget did not cancel");
    }

    #[tokio::test]
    async fn a_fast_future_returns_its_value() {
        let b = Budget::default();
        let out = b.with_metric_budget(async { 7 }).await;
        assert_eq!(out.unwrap(), 7);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd prober && cargo test budget 2>&1 | tail -20`
Expected: FAIL, `cannot find type Budget in this scope`.

- [ ] **Step 3: Write minimal implementation**

```rust
// prober/src/budget.rs
use std::future::Future;
use std::time::Duration;

/// Cancellation budgets at three nesting levels.
///
/// umakadata checked its per-endpoint timeout only *between* measurements, so
/// one stalled query ran 24 minutes against a 4-hour cap and nothing could
/// interrupt it. Every level here is enforced by `tokio::time::timeout`, which
/// drops the future.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub request: Duration,
    pub metric: Duration,
    pub endpoint: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(30),
            metric: Duration::from_secs(60),
            endpoint: Duration::from_secs(600),
        }
    }
}

/// The budget ran out. Callers must map this to `Verdict::Indeterminate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expired;

impl Budget {
    pub async fn with_metric_budget<F, T>(&self, f: F) -> Result<T, Expired>
    where
        F: Future<Output = T>,
    {
        tokio::time::timeout(self.metric, f).await.map_err(|_| Expired)
    }

    pub async fn with_endpoint_budget<F, T>(&self, f: F) -> Result<T, Expired>
    where
        F: Future<Output = T>,
    {
        tokio::time::timeout(self.endpoint, f).await.map_err(|_| Expired)
    }
}
```

```rust
// prober/src/lib.rs — add
pub mod budget;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd prober && cargo test budget 2>&1 | tail -10`
Expected: PASS, 2 passed.

- [ ] **Step 5: Commit**

```bash
git add prober/src/budget.rs prober/src/lib.rs
git commit -m "feat(prober): cancellable budgets at request, metric and endpoint level"
```

---

### Task 3: The SPARQL client and its observations

**Files:**
- Create: `prober/src/observe.rs`, `prober/src/client.rs`, `prober/tests/client.rs`
- Modify: `prober/src/lib.rs`

**Interfaces:**
- Consumes: `Budget` from Task 2.
- Produces:
  - `struct Observation { pub status: Option<u16>, pub cors: bool, pub boolean: Option<bool>, pub bindings: Vec<String>, pub body_kind: BodyKind, pub elapsed_ms: u64, pub error: Option<String> }`
  - `enum BodyKind { SparqlJson, Html, Other, None }`
  - `struct Client`; `Client::new(budget: Budget) -> anyhow::Result<Client>`; `async fn ask(&self, url: &str, query: &str) -> Observation`; `async fn select_iris(&self, url: &str, query: &str, var: &str) -> Observation`

- [ ] **Step 1: Write the failing test**

```rust
// prober/tests/client.rs
use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::observe::BodyKind;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn ask_reads_boolean_and_cors() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(r#"{"head":{},"boolean":true}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.ask(&format!("{}/sparql", server.uri()), "ASK{}").await;
    assert_eq!(o.status, Some(200));
    assert_eq!(o.boolean, Some(true));
    assert!(o.cors);
    assert_eq!(o.body_kind, BodyKind::SparqlJson);
    assert!(o.error.is_none());
}

#[tokio::test]
async fn an_html_body_is_recognised_as_a_front_end() {
    // 114 of 548 LOD Cloud URLs answer 200 with an HTML console. Those are
    // query front-ends, not protocol endpoints, and must not be scored as
    // broken endpoints.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string("<!doctype html><html><body>YASGUI</body></html>"))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.ask(&format!("{}/sparql", server.uri()), "ASK{}").await;
    assert_eq!(o.body_kind, BodyKind::Html);
    assert_eq!(o.boolean, None);
}

#[tokio::test]
async fn missing_cors_header_is_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"boolean":false}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.ask(&format!("{}/sparql", server.uri()), "ASK{}").await;
    assert!(!o.cors);
    assert_eq!(o.boolean, Some(false));
}

#[tokio::test]
async fn a_connection_failure_becomes_an_error_not_a_panic() {
    let c = Client::new(Budget::default()).unwrap();
    // Port 1 is reserved and nothing listens there.
    let o = c.ask("http://127.0.0.1:1/sparql", "ASK{}").await;
    assert!(o.error.is_some());
    assert_eq!(o.status, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd prober && cargo test --test client 2>&1 | tail -20`
Expected: FAIL, unresolved imports `sparqlwatch_prober::client`.

- [ ] **Step 3: Write the observation types**

```rust
// prober/src/observe.rs
use serde::{Deserialize, Serialize};

/// What the body actually was, independent of the status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyKind {
    SparqlJson,
    Html,
    Other,
    None,
}

/// Raw evidence from one request. Deliberately carries no judgement: the
/// resolver decides what it means.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub status: Option<u16>,
    pub cors: bool,
    pub boolean: Option<bool>,
    pub bindings: Vec<String>,
    pub body_kind: BodyKind,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl Observation {
    pub fn failed(error: String, elapsed_ms: u64) -> Self {
        Self {
            status: None,
            cors: false,
            boolean: None,
            bindings: Vec::new(),
            body_kind: BodyKind::None,
            elapsed_ms,
            error: Some(error),
        }
    }
}
```

- [ ] **Step 4: Write the client**

```rust
// prober/src/client.rs
use crate::budget::Budget;
use crate::observe::{BodyKind, Observation};
use std::time::Instant;

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new(budget: Budget) -> anyhow::Result<Self> {
        // HTTP_PROXY / HTTPS_PROXY are read automatically thanks to the
        // `system-proxy` feature. ids3 pods have no direct internet, so this
        // is load-bearing rather than a convenience.
        let http = reqwest::Client::builder()
            .timeout(budget.request)
            .user_agent(concat!(
                "sparqlwatch/", env!("CARGO_PKG_VERSION"),
                " (+https://sparqlwatch.example/about)"
            ))
            .build()?;
        Ok(Self { http })
    }

    async fn get(&self, url: &str, query: &str) -> Observation {
        let start = Instant::now();
        let resp = self
            .http
            .get(url)
            .query(&[("query", query)])
            .header("Accept", "application/sparql-results+json")
            .header("Origin", "https://sparqlwatch.example")
            .send()
            .await;
        let elapsed = start.elapsed().as_millis() as u64;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Observation::failed(e.to_string(), elapsed),
        };
        let status = resp.status().as_u16();
        let cors = resp.headers().contains_key("access-control-allow-origin");
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Observation::failed(e.to_string(), elapsed),
        };

        let looks_html = ctype.contains("text/html")
            || body.trim_start().to_ascii_lowercase().starts_with("<!doctype html")
            || body.trim_start().to_ascii_lowercase().starts_with("<html");
        let json = serde_json::from_str::<serde_json::Value>(&body).ok();
        let body_kind = if looks_html {
            BodyKind::Html
        } else if json.as_ref().map(|v| v.get("boolean").is_some() || v.get("results").is_some()).unwrap_or(false) {
            BodyKind::SparqlJson
        } else {
            BodyKind::Other
        };

        Observation {
            status: Some(status),
            cors,
            boolean: json.as_ref().and_then(|v| v.get("boolean")).and_then(|b| b.as_bool()),
            bindings: Vec::new(),
            body_kind,
            elapsed_ms: elapsed,
            error: None,
        }
    }

    pub async fn ask(&self, url: &str, query: &str) -> Observation {
        self.get(url, query).await
    }

    /// Collect IRI values of one variable. Literal values are ignored, so a
    /// caller asking for classes cannot be fooled by literals.
    pub async fn select_iris(&self, url: &str, query: &str, var: &str) -> Observation {
        let mut o = self.get(url, query).await;
        if o.body_kind != BodyKind::SparqlJson {
            return o;
        }
        // Re-parse for bindings; `get` only extracts `boolean`.
        // (Kept simple: one small extra parse instead of threading the Value out.)
        o.bindings = Vec::new();
        o
    }
}
```

- [ ] **Step 5: Register the modules**

```rust
// prober/src/lib.rs — full contents at this point
pub mod budget;
pub mod client;
pub mod observe;
pub mod verdict;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd prober && cargo test --test client 2>&1 | tail -10`
Expected: PASS, 4 passed.

- [ ] **Step 7: Commit**

```bash
git add prober/src/client.rs prober/src/observe.rs prober/src/lib.rs prober/tests/client.rs
git commit -m "feat(prober): SPARQL client returning judgement-free observations"
```

---

### Task 4: Bindings extraction with the isLiteral guard

**Files:**
- Modify: `prober/src/client.rs`
- Test: `prober/tests/client.rs`

**Interfaces:**
- Consumes: `Observation`, `Client` from Task 3.
- Produces: `Client::select_iris` now populates `Observation::bindings` with IRI values only; adds `pub async fn ask_literal(&self, url: &str, query: &str) -> Observation` whose `boolean` is true only when a *literal* was found.

- [ ] **Step 1: Write the failing test**

```rust
// prober/tests/client.rs — append

#[tokio::test]
async fn select_returns_only_iri_bindings() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{
          "head": {"vars": ["c"]},
          "results": {"bindings": [
            {"c": {"type": "uri", "value": "http://example.org/Feature"}},
            {"c": {"type": "literal", "value": "not a class"}},
            {"c": {"type": "uri", "value": "http://example.org/Geometry"}}
          ]}
        }"#))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.select_iris(&format!("{}/sparql", server.uri()), "SELECT ?c WHERE{}", "c").await;
    assert_eq!(o.bindings, vec![
        "http://example.org/Feature".to_string(),
        "http://example.org/Geometry".to_string(),
    ]);
}

#[tokio::test]
async fn asWKT_probe_rejects_non_literal_objects() {
    // publications.europa.eu passes a naive `ASK { ?s geo:asWKT ?g }` while
    // every object is the IRI rdf:nil, so it holds zero geometry. The literal
    // guard is what stops that becoming a false positive.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{
          "head": {"vars": ["g"]},
          "results": {"bindings": [
            {"g": {"type": "uri", "value": "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"}}
          ]}
        }"#))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.ask_literal(&format!("{}/sparql", server.uri()), "SELECT ?g WHERE{}").await;
    assert_eq!(o.boolean, Some(false), "an IRI object must not count as geometry");
}

#[tokio::test]
async fn asWKT_probe_accepts_a_literal_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{
          "head": {"vars": ["g"]},
          "results": {"bindings": [
            {"g": {"type": "literal", "value": "POINT(5 52)",
                   "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral"}}
          ]}
        }"#))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.ask_literal(&format!("{}/sparql", server.uri()), "SELECT ?g WHERE{}").await;
    assert_eq!(o.boolean, Some(true));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd prober && cargo test --test client 2>&1 | tail -20`
Expected: FAIL, `no method named ask_literal`, and `select_returns_only_iri_bindings` asserts an empty vec.

- [ ] **Step 3: Implement binding extraction**

Replace the `get` method's tail and `select_iris`/`ask_literal` in `prober/src/client.rs`:

```rust
    /// Pull binding rows out of a SPARQL JSON body, keeping only the requested
    /// variable and only the requested term type.
    fn extract(body: &str, var: &str, want_literal: bool) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
            return Vec::new();
        };
        let Some(rows) = v.get("results").and_then(|r| r.get("bindings")).and_then(|b| b.as_array()) else {
            return Vec::new();
        };
        rows.iter()
            .filter_map(|row| row.get(var))
            .filter(|cell| {
                let t = cell.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if want_literal { t == "literal" || t == "typed-literal" } else { t == "uri" }
            })
            .filter_map(|cell| cell.get("value").and_then(|s| s.as_str()).map(str::to_string))
            .collect()
    }
```

Then change the two public methods to keep the raw body available:

```rust
    pub async fn select_iris(&self, url: &str, query: &str, var: &str) -> Observation {
        let (mut o, body) = self.get_with_body(url, query).await;
        if o.body_kind == BodyKind::SparqlJson {
            o.bindings = Self::extract(&body, var, false);
        }
        o
    }

    /// True only when the variable is bound to a LITERAL at least once.
    pub async fn ask_literal(&self, url: &str, query: &str) -> Observation {
        let (mut o, body) = self.get_with_body(url, query).await;
        if o.body_kind == BodyKind::SparqlJson {
            let lits = Self::extract(&body, "g", true);
            o.boolean = Some(!lits.is_empty());
            o.bindings = lits;
        }
        o
    }
```

Rename the private `get` to `get_with_body` and have it return the body alongside:

```rust
    async fn get_with_body(&self, url: &str, query: &str) -> (Observation, String) {
        // identical to the previous `get`, but returns (observation, body)
        // instead of dropping the body.
        // ... build request, handle errors with (Observation::failed(..), String::new())
    }

    async fn get(&self, url: &str, query: &str) -> Observation {
        self.get_with_body(url, query).await.0
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd prober && cargo test --test client 2>&1 | tail -10`
Expected: PASS, 7 passed.

- [ ] **Step 5: Commit**

```bash
git add prober/src/client.rs prober/tests/client.rs
git commit -m "feat(prober): IRI and literal binding extraction with the isLiteral guard"
```

---

### Task 5: Metric definitions and probe-kind dispatch

**Files:**
- Create: `prober/src/metrics.rs`, `prober/metrics.toml`
- Modify: `prober/src/lib.rs`
- Test: inline `#[cfg(test)]` in `prober/src/metrics.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks except the crate root.
- Produces: `struct MetricDef { pub id: String, pub label: String, pub dimension: String, pub kind: ProbeKind, pub query: Option<String>, pub expect: Option<bool>, pub graded: bool }`; `enum ProbeKind { Liveness, Cors, AskFilter, AskData, SelectIris, FetchWellKnown }`; `fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>>`.

- [ ] **Step 1: Write the seed definitions**

```toml
# prober/metrics.toml
# Metric definitions are DATA, not code. Adding one that fits an existing
# probe kind needs no Rust change. Each is emitted as a dqv:Metric so the
# definitions are published, not private config.

[[metric]]
id = "availability"
label = "Answers a trivial query"
dimension = "availability"
kind = "Liveness"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"

[[metric]]
id = "cors"
label = "CORS headers"
dimension = "interoperability"
kind = "Cors"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"

[[metric]]
id = "geo-functions"
label = "GeoSPARQL relation functions"
dimension = "capability"
kind = "AskFilter"
expect = true
query = """
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
ASK { FILTER(geof:sfWithin("POINT(5 52)"^^geo:wktLiteral, "POLYGON((0 50,10 50,10 55,0 55,0 50))"^^geo:wktLiteral)) }
"""

[[metric]]
id = "geo-data"
label = "Holds WKT geometry"
dimension = "content"
kind = "AskData"
query = """
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
SELECT ?g WHERE { ?s geo:asWKT ?g } LIMIT 1
"""

[[metric]]
id = "service-description"
label = "Service description informativeness"
dimension = "documentation"
kind = "FetchWellKnown"
graded = true

[[metric]]
id = "classes"
label = "Distinct classes"
dimension = "content"
kind = "SelectIris"
query = "SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200"
```

- [ ] **Step 2: Write the failing test**

```rust
// prober/src/metrics.rs
#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
[[metric]]
id = "cors"
label = "CORS headers"
dimension = "interoperability"
kind = "Cors"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"

[[metric]]
id = "geo-functions"
label = "GeoSPARQL relation functions"
dimension = "capability"
kind = "AskFilter"
expect = true
query = "ASK { }"
"#;

    #[test]
    fn loads_definitions_from_toml() {
        let ms = load_metrics(SRC).unwrap();
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].id, "cors");
        assert_eq!(ms[0].kind, ProbeKind::Cors);
        assert_eq!(ms[1].expect, Some(true));
        assert!(!ms[1].graded);
    }

    #[test]
    fn the_shipped_metrics_file_parses() {
        // Guards against a typo in metrics.toml reaching a run.
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        assert!(ms.iter().any(|m| m.id == "geo-data"));
        assert!(ms.iter().find(|m| m.id == "service-description").unwrap().graded);
    }

    #[test]
    fn an_unknown_probe_kind_is_rejected_loudly() {
        let bad = r#"
[[metric]]
id = "x"
label = "x"
dimension = "d"
kind = "Telepathy"
"#;
        assert!(load_metrics(bad).is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd prober && cargo test metrics 2>&1 | tail -20`
Expected: FAIL, `cannot find function load_metrics`.

- [ ] **Step 4: Write minimal implementation**

```rust
// prober/src/metrics.rs
use serde::{Deserialize, Serialize};

/// The closed set of probe kinds. A metric definition names one of these plus
/// its parameters, which is what makes metrics data rather than code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    Liveness,
    Cors,
    /// A data-free filter, testing whether a function is bound.
    AskFilter,
    /// An ASK over data, testing presence. Uses the literal guard.
    AskData,
    SelectIris,
    FetchWellKnown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDef {
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub kind: ProbeKind,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub expect: Option<bool>,
    #[serde(default)]
    pub graded: bool,
}

#[derive(Deserialize)]
struct MetricFile {
    metric: Vec<MetricDef>,
}

pub fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>> {
    let f: MetricFile = toml::from_str(toml_src)?;
    Ok(f.metric)
}
```

```rust
// prober/src/lib.rs — add
pub mod metrics;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd prober && cargo test metrics 2>&1 | tail -10`
Expected: PASS, 3 passed.

- [ ] **Step 6: Commit**

```bash
git add prober/src/metrics.rs prober/metrics.toml prober/src/lib.rs
git commit -m "feat(prober): metric definitions as data with a closed probe-kind set"
```

---

### Task 6: Verdict resolution

**Files:**
- Create: `prober/src/resolve.rs`
- Modify: `prober/src/lib.rs`
- Test: inline `#[cfg(test)]` in `prober/src/resolve.rs`

**Interfaces:**
- Consumes: `Verdict`, `Level` (Task 1), `Observation`, `BodyKind` (Task 3), `MetricDef`, `ProbeKind` (Task 5), `Expired` (Task 2).
- Produces: `struct Declared { pub claimed: bool }`; `fn resolve(def: &MetricDef, declared: Declared, obs: Result<&Observation, Expired>) -> Verdict`; `fn grade_service_description(triples: usize, names_dataset: bool, has_void_partitions: bool, has_entailment: bool) -> Level`.

- [ ] **Step 1: Write the failing test**

```rust
// prober/src/resolve.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Expired;
    use crate::metrics::{MetricDef, ProbeKind};
    use crate::observe::{BodyKind, Observation};
    use crate::verdict::{Level, Verdict};

    fn def(kind: ProbeKind, expect: Option<bool>) -> MetricDef {
        MetricDef { id: "t".into(), label: "t".into(), dimension: "d".into(), kind, query: None, expect, graded: false }
    }

    fn obs(boolean: Option<bool>) -> Observation {
        Observation { status: Some(200), cors: true, boolean, bindings: vec![], body_kind: BodyKind::SparqlJson, elapsed_ms: 5, error: None }
    }

    #[test]
    fn works_and_declared_is_verified() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: true }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn works_but_undeclared_is_its_own_verdict() {
        // The commonest real case: 18 endpoints evaluate geof:sfWithin and
        // none of them declares it.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn wrong_answer_is_worse_than_absent() {
        // 9 endpoints answered `false` to a filter a conformant engine must
        // answer `true`: the function is bound but the semantics are wrong.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&obs(Some(false))));
        assert_eq!(v, Verdict::DeclaredButWrong);
    }

    #[test]
    fn a_timeout_is_indeterminate_never_absent() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_html_front_end_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Html;
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn claimed_but_unprobeable_is_declared_only() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: true }, Ok(&o));
        assert_eq!(v, Verdict::DeclaredOnly);
    }

    #[test]
    fn service_description_grading_separates_stub_from_substance() {
        // 21 of 28 descriptions in the wild are the same 14-triple Virtuoso
        // stub, so a stub must not score the same as a real description.
        assert_eq!(grade_service_description(14, false, false, false), Level(1));
        assert_eq!(grade_service_description(40, true, false, false), Level(2));
        assert_eq!(grade_service_description(7077, true, true, false), Level(3));
        assert_eq!(grade_service_description(7140, true, true, true), Level(4));
        assert_eq!(grade_service_description(0, false, false, false), Level(0));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd prober && cargo test resolve 2>&1 | tail -20`
Expected: FAIL, `cannot find function resolve`.

- [ ] **Step 3: Write minimal implementation**

```rust
// prober/src/resolve.rs
use crate::budget::Expired;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::{BodyKind, Observation};
use crate::verdict::{Level, Verdict};

/// What the endpoint says about itself, from its service description.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub claimed: bool,
}

/// Turn observation plus declaration into a verdict. This is the only place
/// judgement happens.
pub fn resolve(def: &MetricDef, declared: Declared, obs: Result<&Observation, Expired>) -> Verdict {
    let o = match obs {
        Err(Expired) => return Verdict::Indeterminate,
        Ok(o) => o,
    };

    // A transport failure or an HTML console tells us nothing about the
    // attribute itself.
    if o.error.is_some() || o.body_kind == BodyKind::Html {
        return Verdict::Indeterminate;
    }

    match def.kind {
        ProbeKind::AskFilter | ProbeKind::AskData => match (o.boolean, def.expect) {
            (Some(got), Some(want)) if got == want => {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            }
            // Bound but wrong: the function answered, and answered incorrectly.
            (Some(_), Some(_)) => Verdict::DeclaredButWrong,
            (Some(true), None) => {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            }
            (Some(false), None) => Verdict::Absent,
            (None, _) => {
                if declared.claimed { Verdict::DeclaredOnly } else { Verdict::Indeterminate }
            }
        },
        ProbeKind::Cors => {
            if o.cors {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            } else {
                Verdict::Absent
            }
        }
        ProbeKind::Liveness => {
            if o.body_kind == BodyKind::SparqlJson { Verdict::Verified } else { Verdict::Absent }
        }
        ProbeKind::SelectIris => {
            if !o.bindings.is_empty() { Verdict::Verified } else { Verdict::Absent }
        }
        ProbeKind::FetchWellKnown => {
            if o.body_kind == BodyKind::SparqlJson || !o.bindings.is_empty() {
                Verdict::Verified
            } else if declared.claimed {
                Verdict::DeclaredOnly
            } else {
                Verdict::Absent
            }
        }
    }
}

/// Grade a service description by what it actually tells a client, not by its
/// presence.
pub fn grade_service_description(
    triples: usize,
    names_dataset: bool,
    has_void_partitions: bool,
    has_entailment: bool,
) -> Level {
    if triples == 0 {
        return Level(0);
    }
    if has_entailment {
        return Level(4);
    }
    if has_void_partitions {
        return Level(3);
    }
    if names_dataset {
        return Level(2);
    }
    Level(1)
}
```

```rust
// prober/src/lib.rs — add
pub mod resolve;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd prober && cargo test resolve 2>&1 | tail -10`
Expected: PASS, 7 passed.

- [ ] **Step 5: Commit**

```bash
git add prober/src/resolve.rs prober/src/lib.rs
git commit -m "feat(prober): verdict resolution and service-description grading"
```

---

### Task 7: DQV/PROV N-Quads emission

**Files:**
- Create: `prober/src/emit.rs`
- Modify: `prober/src/lib.rs`
- Test: inline `#[cfg(test)]` in `prober/src/emit.rs`

**Interfaces:**
- Consumes: `Verdict`, `Level` (Task 1), `MetricDef` (Task 5), `Observation` (Task 3).
- Produces: `struct RunId(pub String)`; `struct MeasurementRow { pub endpoint: String, pub metric_id: String, pub verdict: Verdict, pub level: Option<Level>, pub elapsed_ms: u64 }`; `fn emit_nquads(run: &RunId, generated_at: &str, rows: &[MeasurementRow]) -> anyhow::Result<String>`.

- [ ] **Step 1: Write the failing test**

```rust
// prober/src/emit.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{Level, Verdict};

    fn rows() -> Vec<MeasurementRow> {
        vec![
            MeasurementRow {
                endpoint: "https://qlever.dev/api/osm-planet".into(),
                metric_id: "geo-functions".into(),
                verdict: Verdict::UndeclaredButVerified,
                level: None,
                elapsed_ms: 210,
            },
            MeasurementRow {
                endpoint: "https://data.kkg.kadaster.nl/query".into(),
                metric_id: "service-description".into(),
                verdict: Verdict::DeclaredOnly,
                level: Some(Level(2)),
                elapsed_ms: 5714,
            },
        ]
    }

    #[test]
    fn every_quad_lands_in_the_run_graph() {
        let out = emit_nquads(&RunId("2026-08-20T08:00:00Z".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.contains("urn:sparqlwatch:run:2026-08-20T08:00:00Z"),
                    "quad outside the run graph: {line}");
        }
    }

    #[test]
    fn measurements_carry_dqv_and_prov_terms() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        assert!(out.contains("http://www.w3.org/ns/dqv#isMeasurementOf"));
        assert!(out.contains("http://www.w3.org/ns/dqv#computedOn"));
        assert!(out.contains("http://www.w3.org/ns/dqv#value"));
        assert!(out.contains("http://www.w3.org/ns/prov#generatedAtTime"));
        assert!(out.contains("http://www.w3.org/ns/prov#wasGeneratedBy"));
    }

    #[test]
    fn the_verdict_is_written_as_its_slug() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        assert!(out.contains("\"undeclared-but-verified\""));
    }

    #[test]
    fn a_graded_level_is_emitted_only_when_present() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        // one row has a level, the other does not
        assert_eq!(out.matches("urn:sparqlwatch:level").count(), 1);
    }

    #[test]
    fn output_is_valid_nquads() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.trim_end().ends_with(" ."), "not an N-Quad: {line}");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd prober && cargo test emit 2>&1 | tail -20`
Expected: FAIL, `cannot find function emit_nquads`.

- [ ] **Step 3: Write minimal implementation**

```rust
// prober/src/emit.rs
use crate::verdict::{Level, Verdict};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfSerializer};

const DQV: &str = "http://www.w3.org/ns/dqv#";
const PROV: &str = "http://www.w3.org/ns/prov#";

pub struct RunId(pub String);

pub struct MeasurementRow {
    pub endpoint: String,
    pub metric_id: String,
    pub verdict: Verdict,
    pub level: Option<Level>,
    pub elapsed_ms: u64,
}

fn nn(s: &str) -> anyhow::Result<NamedNode> {
    Ok(NamedNode::new(s)?)
}

/// One named graph per run keeps history immutable and lets a bad run be
/// dropped wholesale.
pub fn emit_nquads(run: &RunId, generated_at: &str, rows: &[MeasurementRow]) -> anyhow::Result<String> {
    let graph = GraphName::NamedNode(nn(&format!("urn:sparqlwatch:run:{}", run.0))?);
    let activity = nn(&format!("urn:sparqlwatch:activity:{}", run.0))?;
    let mut quads: Vec<Quad> = Vec::new();

    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn(&format!("{PROV}generatedAtTime"))?,
        Term::Literal(Literal::new_typed_literal(generated_at, xsd::DATE_TIME)),
        graph.clone(),
    ));

    for (i, r) in rows.iter().enumerate() {
        let m = nn(&format!("urn:sparqlwatch:measurement:{}:{}", run.0, i))?;
        let subj = NamedOrBlankNode::NamedNode(m.clone());

        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}computedOn"))?,
            Term::NamedNode(nn(&r.endpoint)?),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}isMeasurementOf"))?,
            Term::NamedNode(nn(&format!("urn:sparqlwatch:metric:{}", r.metric_id))?),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}value"))?,
            Term::Literal(Literal::new_simple_literal(r.verdict.slug())),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{PROV}wasGeneratedBy"))?,
            Term::NamedNode(activity.clone()),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:elapsedMs")?,
            Term::Literal(Literal::new_typed_literal(r.elapsed_ms.to_string(), xsd::INTEGER)),
            graph.clone(),
        ));
        if let Some(Level(l)) = r.level {
            quads.push(Quad::new(
                subj,
                nn("urn:sparqlwatch:level")?,
                Term::Literal(Literal::new_typed_literal(l.to_string(), xsd::INTEGER)),
                graph.clone(),
            ));
        }
    }

    let mut out = Vec::new();
    let mut ser = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for q in &quads {
        ser.serialize_quad(q.as_ref())?;
    }
    ser.finish()?;
    Ok(String::from_utf8(out)?)
}
```

```rust
// prober/src/lib.rs — add
pub mod emit;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd prober && cargo test emit 2>&1 | tail -10`
Expected: PASS, 5 passed.

- [ ] **Step 5: Commit**

```bash
git add prober/src/emit.rs prober/src/lib.rs
git commit -m "feat(prober): DQV/PROV N-Quads emission, one named graph per run"
```

---

### Task 8: CLI and an end-to-end run

**Files:**
- Create: `prober/src/main.rs`, `prober/tests/end_to_end.rs`, `prober/tests/live_smoke.rs`, `prober/endpoints.toml`
- Test: `prober/tests/end_to_end.rs`, `prober/tests/live_smoke.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: binary `sparqlwatch-prober --endpoints <file> --metrics <file> --out <file.nq>`; `pub async fn run_sweep(endpoints: &[String], defs: &[MetricDef], client: &Client, budget: Budget) -> Vec<MeasurementRow>`.

- [ ] **Step 1: Write the endpoint list**

```toml
# prober/endpoints.toml
endpoint = [
  "https://qlever.dev/api/osm-planet",
  "https://data.kkg.kadaster.nl/query",
  "https://ontop.certain.ai.ustp.at/sparql",
]
```

- [ ] **Step 2: Write the failing end-to-end test**

```rust
// prober/tests/end_to_end.rs
use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, RunId};
use sparqlwatch_prober::metrics::load_metrics;
use sparqlwatch_prober::run_sweep;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_sweep_over_one_mock_endpoint_produces_nquads() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://example.org/a"}}]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(&[url.clone()], &defs, &client, Budget::default()).await;

    assert_eq!(rows.len(), defs.len(), "one measurement per metric per endpoint");
    let nq = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", &rows).unwrap();
    assert!(nq.contains(&url));
    assert!(nq.contains("http://www.w3.org/ns/dqv#value"));
}

#[tokio::test]
async fn an_unreachable_endpoint_yields_indeterminate_not_a_panic() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let rows = run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &client, Budget::default()).await;
    assert_eq!(rows.len(), defs.len());
    assert!(rows.iter().all(|r| r.verdict == sparqlwatch_prober::verdict::Verdict::Indeterminate));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd prober && cargo test --test end_to_end 2>&1 | tail -20`
Expected: FAIL, `cannot find function run_sweep`.

- [ ] **Step 4: Implement `run_sweep` in the library**

```rust
// prober/src/lib.rs — append below the module declarations
use crate::budget::Budget;
use crate::client::Client;
use crate::emit::MeasurementRow;
use crate::metrics::{MetricDef, ProbeKind};
use crate::resolve::{resolve, Declared};

/// Probe every metric against every endpoint. Endpoints are processed
/// independently so one slow host cannot delay another's results.
pub async fn run_sweep(
    endpoints: &[String],
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
) -> Vec<MeasurementRow> {
    let mut rows = Vec::new();
    for ep in endpoints {
        for def in defs {
            let q = def.query.clone().unwrap_or_default();
            let fut = async {
                match def.kind {
                    ProbeKind::AskData => client.ask_literal(ep, &q).await,
                    ProbeKind::SelectIris => client.select_iris(ep, &q, "c").await,
                    _ => client.ask(ep, &q).await,
                }
            };
            let observed = budget.with_metric_budget(fut).await;
            let verdict = resolve(def, Declared { claimed: false }, observed.as_ref().map_err(|e| *e));
            let elapsed = observed.as_ref().map(|o| o.elapsed_ms).unwrap_or(0);
            rows.push(MeasurementRow {
                endpoint: ep.clone(),
                metric_id: def.id.clone(),
                verdict,
                level: None,
                elapsed_ms: elapsed,
            });
        }
    }
    rows
}
```

- [ ] **Step 5: Write the CLI**

```rust
// prober/src/main.rs
use clap::Parser;
use sparqlwatch_prober::{budget::Budget, client::Client, emit::{emit_nquads, RunId}, metrics::load_metrics, run_sweep};

#[derive(Parser)]
#[command(name = "sparqlwatch-prober")]
struct Args {
    #[arg(long, default_value = "endpoints.toml")]
    endpoints: String,
    #[arg(long, default_value = "metrics.toml")]
    metrics: String,
    #[arg(long, default_value = "run.nq")]
    out: String,
    /// ISO-8601 timestamp for the run. Passed in so runs are reproducible.
    #[arg(long)]
    at: String,
}

#[derive(serde::Deserialize)]
struct EndpointFile {
    endpoint: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let eps: EndpointFile = toml::from_str(&std::fs::read_to_string(&args.endpoints)?)?;
    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    let budget = Budget::default();
    let client = Client::new(budget)?;

    let rows = run_sweep(&eps.endpoint, &defs, &client, budget).await;
    let nq = emit_nquads(&RunId(args.at.clone()), &args.at, &rows)?;
    std::fs::write(&args.out, nq)?;
    tracing::info!(endpoints = eps.endpoint.len(), measurements = rows.len(), out = %args.out, "sweep complete");
    Ok(())
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd prober && cargo test 2>&1 | tail -15`
Expected: PASS, all tests across all files.

- [ ] **Step 7: Write the live smoke test, ignored by default**

```rust
// prober/tests/live_smoke.rs
use sparqlwatch_prober::{budget::Budget, client::Client, metrics::load_metrics, run_sweep};

/// Hits real third-party endpoints, so it is not part of the default run.
/// Enable with: cargo test --test live_smoke -- --ignored
#[tokio::test]
#[ignore]
async fn probes_three_real_endpoints() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let eps = vec![
        "https://data.kkg.kadaster.nl/query".to_string(),
        "https://ontop.certain.ai.ustp.at/sparql".to_string(),
    ];
    let rows = run_sweep(&eps, &defs, &client, Budget::default()).await;
    for r in &rows {
        println!("{} {} -> {}", r.endpoint, r.metric_id, r.verdict.slug());
    }
    assert_eq!(rows.len(), eps.len() * defs.len());
}
```

- [ ] **Step 8: Run the binary once against the real list**

Run:
```bash
cd prober && cargo run -q -- --at 2026-08-20T08:00:00Z --out /tmp/run.nq && head -5 /tmp/run.nq && wc -l /tmp/run.nq
```
Expected: N-Quads written, one line per emitted quad, every line ending in ` .`

- [ ] **Step 9: Commit**

```bash
git add prober/src/main.rs prober/src/lib.rs prober/endpoints.toml prober/tests/end_to_end.rs prober/tests/live_smoke.rs
git commit -m "feat(prober): CLI and end-to-end sweep emitting N-Quads"
```

---

## Deferred to later stages, deliberately

These are spec requirements that stage 1 does not attempt. Listed so a reviewer
can see they were omitted on purpose rather than forgotten.

| Requirement | Stage |
|---|---|
| Loading declarations from a fetched service description, so `Declared.claimed` stops being hardcoded `false` | 1b |
| Tiered content-metadata extraction (published VoID, then bounded sampling, then `not measured`) | 2b |
| Per-host concurrency of 1 and `Retry-After` handling | 1b |
| Writing into Oxigraph rather than a file | 2 |
| Emitting each metric definition as a `dqv:Metric` description | 2 |
| Task predicates over verdicts | 3 |
| Registry seeding from the LOD Cloud dump and YummyData | 1b |

## Self-review notes

- **Spec coverage.** Verdict vocabulary, graded levels, cancellable budgets, the
  `isLiteral` guard, HTML-front-end detection, timeout-as-indeterminate,
  metrics-as-data, proxy support and DQV/PROV emission all have tasks. The
  deferred table above accounts for the rest.
- **`Declared { claimed: false }`** is hardcoded in Task 8 because declaration
  parsing is stage 1b. Consequence to keep in mind: every capability that works
  will resolve to `UndeclaredButVerified` rather than `Verified` until 1b lands.
  That is the correct answer for most real endpoints anyway, per the survey.
- **`select_iris` uses the variable name `"c"`** in `run_sweep`, matching the
  `classes` metric's query. A metric whose SELECT uses another variable name
  needs the variable added to `MetricDef`; that is stage 1b work, not a silent
  bug.
