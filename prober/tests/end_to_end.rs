use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, RunId};
use sparqlwatch_prober::metrics::{load_metrics, MetricDef, ProbeKind};
use sparqlwatch_prober::run_sweep;
use sparqlwatch_prober::verdict::Verdict;
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

/// A binding-reading probe kind with no declared `var` is a definition error,
/// not something to paper over by guessing a variable name. It must be
/// skipped and recorded as `Indeterminate`, never a confident wrong answer.
#[tokio::test]
async fn a_binding_probe_with_no_declared_var_is_indeterminate_not_guessed() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(r#"{"head":{"vars":["thing"]},"results":{"bindings":[{"thing":{"type":"literal","value":"hello"}}]}}"#))
        .mount(&server).await;

    let def = MetricDef {
        id: "undeclared-var".into(),
        label: "missing var".into(),
        dimension: "content".into(),
        kind: ProbeKind::AskData,
        query: Some("SELECT ?thing WHERE { ?s ?p ?thing } LIMIT 1".into()),
        expect: None,
        var: None,
        graded: false,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let rows = run_sweep(&[url], &[def], &client, Budget::default()).await;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, Verdict::Indeterminate);
}
