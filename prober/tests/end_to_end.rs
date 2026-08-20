use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, RunId};
use sparqlwatch_prober::metrics::{load_metrics, MetricDef, ProbeKind};
use sparqlwatch_prober::run_sweep;
use sparqlwatch_prober::verdict::{Level, Verdict};
use oxrdf::{NamedNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeMap, BTreeSet};
use wiremock::http::Method;
use wiremock::matchers::{method, path, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A minimal, valid service description. Turtle, matching what a queryless
/// fetch actually receives from a real endpoint (see `tests/fetch.rs`). Also
/// declares `geof:sfWithin` as an `sd:extensionFunction`, the one metric in
/// `metrics.toml` that names a `declared_by` IRI, so a test that fetches this
/// stub and finds the probe working can assert `Verified` -- the direction
/// no other test here exercises.
const STUB_TTL: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/sparql> a sd:Service ;
    sd:feature sd:UnionDefaultGraph ;
    sd:extensionFunction <http://www.opengis.net/def/function/geosparql/sfWithin> .
"#;

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
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    assert_eq!(rows.len(), defs.len(), "one measurement per metric per endpoint");

    // Assert the verdicts themselves, not just the row count: an
    // implementation that resolved everything to `Absent` -- the exact failure
    // this project exists to prevent -- passed the old assertions.
    let got: BTreeMap<&str, Verdict> =
        rows.iter().map(|r| (r.metric_id.as_str(), r.verdict)).collect();
    let expected: BTreeMap<&str, Verdict> = BTreeMap::from([
        // A SPARQL JSON body proves it speaks the protocol.
        ("availability", Verdict::Verified),
        // The mock sets access-control-allow-origin.
        ("cors", Verdict::UndeclaredButVerified),
        // boolean:true against expect = true.
        ("geo-functions", Verdict::UndeclaredButVerified),
        // The mock binds ?s, not ?g, so no WKT literal is present and the 200
        // makes that a genuine absence rather than an unknown.
        ("geo-data", Verdict::Absent),
        // The mock's single `set_body_string` response is served as
        // `text/plain` regardless of the request (wiremock 0.6.5 always
        // overwrites Content-Type on `set_body_string`; see `tests/fetch.rs`),
        // so the queryless fetch's body_kind is `Other`, not `Rdf`. A 200 with
        // an unparsed body is `Indeterminate`, never `Absent`.
        ("service-description", Verdict::Indeterminate),
        // Likewise ?c is unbound, so zero classes, honestly measured.
        ("classes", Verdict::Absent),
    ]);
    assert_eq!(got, expected);

    let nq = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(nq.as_bytes())
        .map(|q| q.expect("the emitted sweep must parse as N-Quads"))
        .collect();
    let published: BTreeSet<String> = quads
        .iter()
        .filter(|q| q.predicate.as_str() == "http://www.w3.org/ns/dqv#value")
        .map(|q| match &q.object {
            Term::Literal(l) => l.value().to_string(),
            other => panic!("a verdict must be a literal, got {other}"),
        })
        .collect();
    assert_eq!(
        published,
        expected.values().map(|v| v.slug().to_string()).collect::<BTreeSet<String>>(),
        "every resolved verdict reaches the published graph, unchanged"
    );
    assert!(quads.iter().any(|q| q.object == Term::NamedNode(NamedNode::new(&url).unwrap())));
}

#[tokio::test]
async fn an_unreachable_endpoint_yields_indeterminate_not_a_panic() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let rows = run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &client, Budget::default()).await;
    assert_eq!(rows.len(), defs.len());
    assert!(rows.iter().all(|r| r.verdict == Verdict::Indeterminate));
}

/// Controller amendment: `run_sweep` must read the variable a metric's query
/// actually binds, not a hardcoded "g"/"c". A query binding `?thing` must be
/// extracted correctly when `var = Some("thing")`, or a real capability
/// silently reports as absent -- the exact false negative this survey exists
/// to prevent.
#[tokio::test]
async fn a_metric_binding_a_nonstandard_variable_is_extracted_via_its_declared_var() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["thing"]},"results":{"bindings":[{"thing":{"type":"literal","value":"hello"}}]}}"#))
        .mount(&server).await;

    let def = MetricDef {
        id: "custom-data".into(),
        label: "custom data probe".into(),
        dimension: "content".into(),
        kind: ProbeKind::AskData,
        query: Some("SELECT ?thing WHERE { ?s ?p ?thing } LIMIT 1".into()),
        expect: None,
        var: Some("thing".into()),
        declared_by: None,
        graded: false,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(&[url], &[def], &client, Budget::default()).await;

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].verdict,
        Verdict::UndeclaredButVerified,
        "a bound ?thing literal must be found via the metric's declared var, not silently reported absent"
    );
}

/// One fetch, not one per metric: six metrics must not mean six identical
/// queryless GETs in an operator's log. This supersedes an earlier test
/// (`a_probe_kind_with_no_implementation_issues_no_request`, removed) that
/// pinned `FetchWellKnown` issuing *no* request at all -- true only while it
/// had no probe. Now that it does, the invariant worth pinning is "exactly
/// one", not "zero", and no other kind in the closed set currently lacks a
/// probe to exercise the old assertion with.
#[tokio::test]
async fn the_sweep_fetches_the_description_once_per_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
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
        .filter(|r| r.method == Method::GET && r.url.query().is_none()).count();
    assert_eq!(queryless, 1, "expected exactly one queryless fetch per endpoint");
    assert_eq!(rows.len(), defs.len());
}

/// The first place a `Level` can appear in a row: a fetched, parseable
/// service description grades itself via `resolve_fetch`, and that grade
/// must reach the `service-description` row rather than being computed and
/// discarded.
#[tokio::test]
async fn a_fetched_description_puts_a_level_on_its_row() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let row = rows.iter().find(|r| r.metric_id == "service-description").unwrap();
    assert_eq!(row.verdict, Verdict::Verified);
    assert!(row.level.is_some(), "a graded metric must carry its level");
}

/// Fix round 1: the reviewer traced that if `lib.rs` were mis-wired to join
/// `Declared::from(&Declarations::empty(), def)` instead of the real fetched
/// `Declarations`, every other test in this file would still pass --
/// `STUB_TTL` originally declared nothing any metric's `declared_by` names,
/// the once-per-endpoint and level tests never look at `geo-functions`, and
/// the very first test's plain-JSON mock never serves a parseable
/// description at all. This test is the one that actually depends on the
/// join: `STUB_TTL` now declares `geof:sfWithin`, the probe answers `true`
/// (matching `expect = true`), and only a real join credits that as
/// `Verified` rather than `UndeclaredButVerified`.
#[tokio::test]
async fn a_declared_and_working_capability_resolves_to_verified_through_the_sweep() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let row = rows.iter().find(|r| r.metric_id == "geo-functions").unwrap();
    assert_eq!(
        row.verdict,
        Verdict::Verified,
        "declared in the fetched description and bound by the probe -- both halves of the join must run"
    );
}

/// A minimal, valid RDF/XML service description, declaring `geof:sfWithin`
/// exactly as `STUB_TTL` does in Turtle. `RDF_ACCEPT` in `client.rs` asks for
/// `application/rdf+xml` as well as Turtle, so an endpoint that actually
/// serves this format must not have its declarations silently dropped by an
/// assumed-Turtle reparse.
const STUB_RDFXML: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:sd="http://www.w3.org/ns/sparql-service-description#">
  <sd:Service rdf:about="http://example.org/sparql">
    <sd:extensionFunction rdf:resource="http://www.opengis.net/def/function/geosparql/sfWithin"/>
  </sd:Service>
</rdf:RDF>
"#;

/// Fix round 1: `parse_declarations(body, None)` always assumed Turtle, but
/// `fetch_rdf` asks for (and a real endpoint may serve) RDF/XML or JSON-LD
/// too. Reparsing an RDF/XML body as Turtle fails immediately and yields
/// `Declarations::empty()` -- silently indistinguishable from an endpoint
/// that published nothing at all, and specifically capable of turning an
/// honest declaration into `UndeclaredButVerified` instead of `Verified`.
/// `lib.rs` must thread the observed `Content-Type` into `parse_declarations`
/// so a non-Turtle description is parsed as itself.
#[tokio::test]
async fn a_description_served_as_rdf_xml_is_parsed_not_silently_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB_RDFXML.as_bytes().to_vec(), "application/rdf+xml"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let description = rows.iter().find(|r| r.metric_id == "service-description").unwrap();
    assert_eq!(description.verdict, Verdict::Verified, "the body did parse, as RDF/XML");
    assert_ne!(
        description.level,
        Some(Level(0)),
        "a description with a real triple in it must not grade the same as an empty one"
    );

    let geo = rows.iter().find(|r| r.metric_id == "geo-functions").unwrap();
    assert_eq!(
        geo.verdict,
        Verdict::Verified,
        "geof:sfWithin was declared in RDF/XML and answered true; a Turtle-only reparse would lose the declaration and report UndeclaredButVerified instead"
    );
}

/// The third budget level. `Budget::with_endpoint_budget` existed but had no
/// caller, so an endpoint could burn metric-budget × metrics, and unboundedly
/// more as metrics are added. When it expires, the metrics not reached must
/// still produce rows, so the one-row-per-(endpoint, metric) invariant holds
/// whatever the timing.
#[tokio::test]
async fn an_endpoint_budget_expiry_still_yields_one_row_per_metric() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_delay(std::time::Duration::from_millis(200))
            .set_body_string(r#"{"head":{},"boolean":true}"#))
        .mount(&server).await;

    let defs: Vec<MetricDef> = (0..3)
        .map(|i| MetricDef {
            id: format!("liveness-{i}"),
            label: "answers a trivial query".into(),
            dimension: "availability".into(),
            kind: ProbeKind::Liveness,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(60),
    };
    let client = Client::new(budget).unwrap();
    let url = format!("{}/sparql", server.uri());
    let started = std::time::Instant::now();
    let rows = run_sweep(std::slice::from_ref(&url), &defs, &client, budget).await;
    let took = started.elapsed();

    assert_eq!(rows.len(), defs.len(), "one row per (endpoint, metric) regardless of timing");
    assert!(took < std::time::Duration::from_millis(400), "endpoint budget did not cut the loop short: {took:?}");
    assert!(
        rows.iter().all(|r| r.verdict == Verdict::Indeterminate),
        "a metric the budget never reached is Indeterminate, never Absent"
    );
    assert!(
        rows.iter().all(|r| r.elapsed_ms.is_none()),
        "an unmeasured metric has no elapsed time, not a zero one"
    );
    for (row, def) in rows.iter().zip(&defs) {
        assert_eq!(row.metric_id, def.id, "rows stay aligned with the definitions");
        assert_eq!(row.endpoint, url);
    }
}

/// A one-triple service description: parseable, so it grades level 1, but it
/// names no dataset, carries no VoID partition and declares nothing that
/// reaches level 4.
const STUB_LEVEL_1: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/bare> a sd:Service .
"#;

/// The same shape plus an entailment regime, which the spec's ladder puts at
/// level 4. Deliberately does NOT declare extension functions or example
/// resources, so this fixture tests one level-4 criterion at a time.
const STUB_LEVEL_4: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/rich> a sd:Service ;
    sd:defaultEntailmentRegime <http://www.w3.org/ns/entailment/RDFS> .
"#;

/// Mount an endpoint that serves `description` to the queryless fetch and an
/// empty SPARQL result to everything else.
async fn endpoint_serving(description: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(description.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;
    server
}

/// Fix round 2: `a_fetched_description_puts_a_level_on_its_row` asserts only
/// `level.is_some()`, and the reviewer showed that replacing the wiring in
/// `lib.rs` with a constant `Some(Level(1))` left all 93 tests green. The unit
/// test `a_fetched_stub_grades_low_and_a_substantial_one_grades_high` pins the
/// computation, but nothing pinned that the emitted row carries the level
/// actually computed for *that* endpoint.
///
/// Two endpoints, one grading 1 and one grading 4, swept in one run. A
/// constant fails on whichever endpoint it does not happen to match, and
/// swapping the two expectations fails too, because each assertion is keyed
/// on the row's own endpoint.
#[tokio::test]
async fn each_endpoints_row_carries_the_level_computed_for_that_endpoint() {
    let bare = endpoint_serving(STUB_LEVEL_1).await;
    let rich = endpoint_serving(STUB_LEVEL_4).await;
    let bare_url = format!("{}/sparql", bare.uri());
    let rich_url = format!("{}/sparql", rich.uri());

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let rows = run_sweep(&[bare_url.clone(), rich_url.clone()], &defs, &client, Budget::default()).await;

    let level_at = |url: &str| {
        let row = rows.iter()
            .find(|r| r.endpoint == url && r.metric_id == "service-description")
            .expect("every endpoint gets a service-description row");
        assert_eq!(row.verdict, Verdict::Verified, "{url} served a parseable description");
        row.level
    };
    assert_eq!(level_at(&bare_url), Some(Level(1)), "a bare one-triple description grades 1");
    assert_eq!(level_at(&rich_url), Some(Level(4)), "an entailment regime grades 4");
}
