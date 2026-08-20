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
