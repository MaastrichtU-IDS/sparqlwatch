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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

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

    let nq =
        emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &_declarations_read)
            .unwrap();
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
    let (rows, _declarations_read) = run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &client, Budget::default()).await;
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
    let (rows, _declarations_read) = run_sweep(&[url], &[def], &client, Budget::default()).await;

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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let row = rows.iter().find(|r| r.metric_id == "service-description").unwrap();
    assert_eq!(row.verdict, Verdict::Verified);
    assert!(row.level.is_some(), "a graded metric must carry its level");
}

/// The level is keyed on the metric definition's `graded` flag, not on the
/// probe kind: `probe_endpoint` computes a level for every `FetchWellKnown`
/// fetch internally, but must only put it on the row when the metric that
/// asked for it is declared `graded = true` in `metrics.toml`. A
/// `FetchWellKnown` metric that is not graded must carry no level at all,
/// even though the same fetch resolves to `Verified` and a level was
/// available to attach.
#[tokio::test]
async fn a_non_graded_fetch_metric_carries_no_level() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;

    let def = MetricDef {
        id: "service-description-ungraded".into(),
        label: "ungraded fetch".into(),
        dimension: "capability".into(),
        kind: ProbeKind::FetchWellKnown,
        query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &[def], &client, Budget::default()).await;

    let row = &rows[0];
    assert_eq!(row.verdict, Verdict::Verified, "the fetch itself still succeeds");
    assert!(row.level.is_none(), "a non-graded metric must carry no level, got {:?}", row.level);
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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

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
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, budget).await;
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
    let (rows, _declarations_read) = run_sweep(&[bare_url.clone(), rich_url.clone()], &defs, &client, Budget::default()).await;

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

/// Scoping reads the endpoint URL, so the URL it reads has to be the one we
/// actually ended on. A registry entry pointing at `http://host/a` that
/// redirects to `/b` is ordinary, and the description then names `/b`.
///
/// The document describes TWO services on purpose. With only one, the
/// single-service fallback would read it whole and this test would pass
/// without the post-redirect URL ever being consulted. With two, a probe that
/// compares only the pre-redirect URL matches neither service, gets an empty
/// scope, and reports `UndeclaredButVerified` instead of `Verified`.
#[tokio::test]
async fn a_description_reached_through_a_redirect_is_still_scoped_to_us() {
    let server = MockServer::start().await;
    let description = format!(
        r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/mine> a sd:Service ;
    sd:endpoint <{}/b> ;
    sd:extensionFunction <http://www.opengis.net/def/function/geosparql/sfWithin> .
<http://example.org/theirs> a sd:Service ;
    sd:endpoint <http://elsewhere.example/sparql> .
"#,
        server.uri()
    );
    // Only the queryless description fetch is redirected; the query probes
    // are answered at `/a` directly, because a 301 drops the query string and
    // this test is about the fetch, not about redirect semantics for queries.
    Mock::given(method("GET")).and(path("/a")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/b"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/b"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(description.into_bytes(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/a"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/a", server.uri());
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let geo = rows.iter().find(|r| r.metric_id == "geo-functions").unwrap();
    assert_eq!(
        geo.verdict,
        Verdict::Verified,
        "the service that declared sfWithin is the one we probed, reached through a redirect"
    );
}

/// The other direction, and the one that motivates the whole change: a host
/// serving two datasets from one description must not credit the dataset we
/// probed with its neighbour's extension functions. Before scoping, this
/// reported `Verified` for a service whose description declares nothing.
#[tokio::test]
async fn a_neighbouring_datasets_declaration_is_not_credited_to_this_endpoint() {
    let server = MockServer::start().await;
    let description = format!(
        r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/geo> a sd:Service ;
    sd:endpoint <{uri}/geo/sparql> ;
    sd:extensionFunction <http://www.opengis.net/def/function/geosparql/sfWithin> .
<http://example.org/plain> a sd:Service ;
    sd:endpoint <{uri}/plain/sparql> .
"#,
        uri = server.uri()
    );
    Mock::given(method("GET")).and(path("/plain/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(description.into_bytes(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/plain/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[]},"boolean":true}"#))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/plain/sparql", server.uri());
    let (rows, _declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;

    let geo = rows.iter().find(|r| r.metric_id == "geo-functions").unwrap();
    assert_eq!(
        geo.verdict,
        Verdict::UndeclaredButVerified,
        "the endpoint evaluates sfWithin but never declared it; its neighbour did"
    );

    // And the grade is untouched by that, because the document as published is
    // what the grade is about.
    let description_row = rows.iter().find(|r| r.metric_id == "service-description").unwrap();
    assert_eq!(description_row.verdict, Verdict::Verified);
    assert!(
        description_row.level.is_some() && description_row.level != Some(Level(0)),
        "a real two-service description is not graded as if nothing were served, got {:?}",
        description_row.level
    );
}

// --- declarationsRead: the fact a consumer cannot infer from the verdicts
// alone. See `resolve::Declared` and `emit::DeclarationsRead` for why. ---

/// A working answer for every query probe: `boolean: true`, one bound `?s`.
/// Good enough to exercise `AskFilter`/`AskData`/`Liveness` positively without
/// caring which query text actually landed.
const WORKING_QUERY_RESPONSE: &str =
    r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://example.org/a"}}]},"boolean":true}"#;

/// Sweep one mock endpoint whose queryless description fetch serves `body`
/// under `content_type`, and whose query probes all get a working answer.
/// Returns the run's emitted N-Quads.
async fn sweep_with_description(body: &str, content_type: &str) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), content_type))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read).unwrap()
}

/// Sweep one mock endpoint where every request, queryless or not, gets
/// `status` with an empty body. The description was never readable.
async fn sweep_with_status(status: u16) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read).unwrap()
}

/// As `sweep_with_status`, except the query probes (everything but the
/// queryless description fetch) get `WORKING_QUERY_RESPONSE`, so a
/// declaration-backed metric's probe genuinely confirms the capability while
/// its description stays unreadable.
async fn sweep_with_status_and_working_queries(status: u16) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, declarations_read) = run_sweep(std::slice::from_ref(&url), &defs, &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read).unwrap()
}

/// Two endpoints: one mock server answering every request, and one address
/// nothing listens on. Same shape as
/// `an_unreachable_endpoint_yields_indeterminate_not_a_panic` above.
async fn sweep_two_endpoints_one_broken() -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let good = format!("{}/sparql", server.uri());
    let broken = "http://127.0.0.1:1/sparql".to_string();
    let (rows, declarations_read) = run_sweep(&[good, broken], &defs, &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read).unwrap()
}

fn quads_of(run: &str) -> Vec<Quad> {
    RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(run.as_bytes())
        .map(|q| q.expect("the run must parse as N-Quads"))
        .collect()
}

/// The `declarationsRead` fact for the (assumed single) endpoint in `run`.
/// `None` means no such quad was found at all, which is exactly the bug this
/// task exists to prevent: `every_endpoint_gets_exactly_one_declarations_read_fact`
/// checks the count directly for the multi-endpoint case.
fn declarations_read(run: &str) -> Option<bool> {
    quads_of(run)
        .iter()
        .find(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead")
        .map(|q| match &q.object {
            Term::Literal(l) => l.value() == "true",
            other => panic!("declarationsRead must be a literal, got {other}"),
        })
}

fn count_declarations_read_quads(run: &str) -> usize {
    quads_of(run).iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead").count()
}

/// The verdict published for `metric_id` in `run`. Reads the two rows a
/// measurement is spread across (`dqv:isMeasurementOf`, `dqv:value`) rather
/// than assuming a fixed quad order, since the emitted document's order is
/// not part of the contract.
fn verdict_of(run: &str, metric_id: &str) -> Verdict {
    let quads = quads_of(run);
    let metric_iri = format!("urn:sparqlwatch:metric:{metric_id}");
    let subject = quads
        .iter()
        .find(|q| {
            q.predicate.as_str() == "http://www.w3.org/ns/dqv#isMeasurementOf"
                && matches!(&q.object, Term::NamedNode(n) if n.as_str() == metric_iri)
        })
        .map(|q| q.subject.clone())
        .unwrap_or_else(|| panic!("no measurement of {metric_id} in this run"));
    let slug = quads
        .iter()
        .find(|q| q.subject == subject && q.predicate.as_str() == "http://www.w3.org/ns/dqv#value")
        .and_then(|q| match &q.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("measurement of {metric_id} has no dqv:value"));
    Verdict::ALL
        .into_iter()
        .find(|v| v.slug() == slug)
        .unwrap_or_else(|| panic!("{slug} is not a verdict slug"))
}

fn load_shipped_metrics() -> Vec<MetricDef> {
    load_metrics(include_str!("../metrics.toml")).unwrap()
}

/// The fact a consumer cannot currently infer. A description that parses
/// partially declares something AND fails to classify, so neither the
/// service-description verdict nor the presence of a declaration tells you
/// whether we read their declarations. Publish it directly.
#[tokio::test]
async fn a_partially_parsed_description_still_reports_its_declarations_as_read() {
    // Declares sfWithin, then breaks. `declare.rs` keeps what parsed; the body
    // does not classify as RDF, so service-description is indeterminate.
    const BROKEN: &str = concat!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n",
        "@prefix geof: <http://www.opengis.net/def/function/geosparql/> .\n",
        "<http://example.org/s> sd:extensionFunction geof:sfWithin .\n",
        "<http://example.org/s> sd:endpoint <<<< not turtle at all\n",
    );
    let run = sweep_with_description(BROKEN, "text/turtle").await;
    assert_eq!(
        declarations_read(&run),
        Some(true),
        "we did read declarations out of this body, whatever the grade says"
    );
    assert_eq!(
        verdict_of(&run, "service-description"),
        Verdict::Indeterminate,
        "the fixture must reproduce the mismatch or it proves nothing"
    );
}

#[tokio::test]
async fn an_unreadable_description_reports_declarations_as_not_read() {
    let run = sweep_with_status(500).await;
    assert_eq!(declarations_read(&run), Some(false));
}

#[tokio::test]
async fn every_endpoint_gets_exactly_one_declarations_read_fact() {
    // Including the ones whose fetch failed: a missing fact is indistinguishable
    // from a false one to a consumer writing a SPARQL query.
    let run = sweep_two_endpoints_one_broken().await;
    assert_eq!(count_declarations_read_quads(&run), 2);
}

/// The one thing an unreadable description must never do is manufacture a
/// declaration. Assert over the declaration-backed metrics specifically: for
/// those, an unread description must land on `undeclared-but-verified` when the
/// probe confirms the capability. Revision 1 asserted only that the verdict was
/// not `declared-only` or `declared-but-wrong`, which a `verified` slips past.
#[tokio::test]
async fn an_unreadable_description_never_manufactures_a_declaration() {
    let run = sweep_with_status_and_working_queries(500).await;
    let defs = load_shipped_metrics();
    let backed: Vec<&MetricDef> = defs.iter().filter(|d| d.declared_by.is_some()).collect();
    assert!(!backed.is_empty(), "there must be a declaration-backed metric or this proves nothing");
    for d in backed {
        assert_eq!(
            verdict_of(&run, &d.id),
            Verdict::UndeclaredButVerified,
            "metric {} claimed a declaration from a description we could not read",
            d.id
        );
    }
}
