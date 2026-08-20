use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, RunId};
use sparqlwatch_prober::metrics::{load_metrics, MetricDef, ProbeKind};
use sparqlwatch_prober::run_sweep;
use sparqlwatch_prober::verdict::Verdict;
use oxrdf::{NamedNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeMap, BTreeSet};
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
        // No probe is implemented for this kind yet.
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

/// `FetchWellKnown` has no implemented probe. Falling through to the generic
/// `ask` path issued `GET <endpoint>?query=` -- a malformed protocol request,
/// to every endpoint on every sweep, fetching nothing about `.well-known` and
/// putting noise in real operators' logs. The metric must stay visible in the
/// output as `Indeterminate`, but no request may leave the process.
#[tokio::test]
async fn a_probe_kind_with_no_implementation_issues_no_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"boolean":true}"#))
        .mount(&server).await;

    let def = MetricDef {
        id: "service-description".into(),
        label: "service description informativeness".into(),
        dimension: "documentation".into(),
        kind: ProbeKind::FetchWellKnown,
        query: None,
        expect: None,
        var: None,
        graded: true,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(&[url], &[def], &client, Budget::default()).await;

    assert_eq!(rows.len(), 1, "the gap must stay visible in the output");
    assert_eq!(rows[0].verdict, Verdict::Indeterminate);
    let seen = server.received_requests().await.unwrap();
    assert!(seen.is_empty(), "an unimplemented probe kind must not touch the network: {seen:?}");
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
