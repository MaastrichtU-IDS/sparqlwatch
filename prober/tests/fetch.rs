use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::observe::BodyKind;
use wiremock::matchers::{headers, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STUB: &str = r#"
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .
<http://example.org/sparql> a sd:Service ; sd:feature sd:UnionDefaultGraph .
"#;

#[tokio::test]
async fn a_turtle_body_is_classified_as_rdf_and_retained() {
    let server = MockServer::start().await;
    // NOTE: wiremock 0.6.5's `ResponseTemplate::set_body_string` unconditionally
    // sets Content-Type to "text/plain" in `generate_response`, overriding any
    // `insert_header("content-type", ...)` call regardless of ordering (verified
    // empirically: HeaderMap::insert always wins over the manually inserted
    // header once `self.mime` is non-empty). `set_body_raw` is the way to serve
    // a string body under an arbitrary Content-Type in this wiremock version.
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB.as_bytes().to_vec(), "text/turtle"))
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
    // NOTE: wiremock 0.6.5's single-value `header(key, value)` matcher does not
    // split the *expected* value on commas, but `HeaderExactMatcher::matches`
    // always splits the *actual* incoming header on commas before comparing
    // (see wiremock::matchers::HeaderExactMatcher). A one-argument `header(..)`
    // matcher against our single comma-joined Accept value therefore compares a
    // 1-element expected vec against a 3-element actual vec and can never
    // match (verified empirically). `headers(key, vec![...])`, matching each
    // comma-separated part, is the matcher this pinned version actually
    // supports for a multi-value header.
    Mock::given(method("GET")).and(path("/sparql"))
        .and(headers("accept", vec!["text/turtle", "application/rdf+xml;q=0.9", "application/ld+json;q=0.8"]))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw(STUB.as_bytes().to_vec(), "text/turtle"))
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
