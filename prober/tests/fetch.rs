use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::declare::parse_declarations;
use sparqlwatch_prober::observe::BodyKind;
use sparqlwatch_prober::resolve::resolve_fetch;
use sparqlwatch_prober::verdict::{Level, Verdict};
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
async fn the_fetch_sends_no_origin() {
    // `Origin` is announced by the CORS probe alone (`Client::cors`). A
    // description fetch that also sent it could have its evidence perturbed
    // by a server that filters on `Origin`, for metrics that have nothing to
    // do with CORS.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/turtle").set_body_string(STUB))
        .mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let _ = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(
        reqs[0].headers.get("origin").is_none(),
        "the description fetch must not send Origin, got {:?}",
        reqs[0].headers.get("origin")
    );
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

// ---------------------------------------------------------------------------
// Fix round 2: what the bytes are (client) and what they mean (resolver).
//
// Two false-positive families were measured against oxrdfio directly. First,
// `RdfFormat::from_media_type` accepts the generic `text/plain`,
// `application/json`, `application/xml` and `text/xml`, mapping them onto
// N-Triples, JSON-LD and RDF/XML: an empty throttle body, a JSON error page
// and a SPARQL-results XML document therefore all parsed as "RDF". Second, a
// zero-triple parse counted as a positive identification, so an empty
// `text/turtle` 200 did too. Third, `resolve_fetch` returned `Verified`
// on an RDF-ish body before it ever looked at the status.
//
// Each case below is one of those, driven end to end through the same
// composition `lib.rs` performs.

/// The exact pipeline `run_sweep` runs for the description probe: fetch,
/// reduce the body to declarations, resolve. Both fixes sit inside it, so the
/// cases below exercise the classification and the verdict together rather
/// than trusting either in isolation.
async fn fetch_and_resolve(status: u16, ctype: Option<&str>, body: &str) -> (Verdict, Option<Level>, BodyKind) {
    let server = MockServer::start().await;
    let mut template = ResponseTemplate::new(status);
    if let Some(ctype) = ctype {
        template = template.set_body_raw(body.as_bytes().to_vec(), ctype);
    }
    Mock::given(method("GET")).and(path("/sparql")).respond_with(template).mount(&server).await;

    let c = Client::new(Budget::default()).unwrap();
    let o = c.fetch_rdf(&format!("{}/sparql", server.uri())).await;
    let declarations = parse_declarations(o.body.as_deref().unwrap_or(""), o.content_type.as_deref());
    let (verdict, level) = resolve_fetch(&declarations, Ok(&o));
    (verdict, level, o.body_kind)
}

#[tokio::test]
async fn a_429_with_an_empty_text_plain_body_is_indeterminate() {
    // `text/plain` maps to N-Triples and an empty body parses clean, so this
    // used to publish `verified` at level 0 for a throttled endpoint: the
    // situation where we know least about what is published.
    let (verdict, level, kind) = fetch_and_resolve(429, Some("text/plain"), "").await;
    assert_eq!(kind, BodyKind::Other, "text/plain is not a positive RDF identification");
    assert_eq!(verdict, Verdict::Indeterminate);
    assert_eq!(level, None);
}

#[tokio::test]
async fn a_500_with_a_json_error_body_is_indeterminate() {
    // `application/json` maps to JSON-LD, under which `{}` parses to zero
    // triples without error.
    let (verdict, level, kind) = fetch_and_resolve(500, Some("application/json"), "{}").await;
    assert_eq!(kind, BodyKind::Other);
    assert_eq!(verdict, Verdict::Indeterminate);
    assert_eq!(level, None);
}

#[tokio::test]
async fn a_503_with_an_empty_body_is_indeterminate() {
    let (verdict, level, kind) = fetch_and_resolve(503, None, "").await;
    assert_eq!(kind, BodyKind::Other);
    assert_eq!(verdict, Verdict::Indeterminate);
    assert_eq!(level, None);
}

#[tokio::test]
async fn sparql_results_xml_served_as_application_xml_is_indeterminate() {
    // The worst of the family: `application/xml` maps to RDF/XML, under which
    // this document parses to three triples and would grade level 1, exactly
    // like a real Virtuoso stub description. The document is a minimal
    // SPARQL-results skeleton rather than a fully populated result set on
    // purpose: RDF/XML rejects unqualified attributes, so the `name="s"` on a
    // real `<variable>`/`<binding>` makes the parse error out, and it is the
    // attribute-free shape that actually produces the false positive. Only the
    // media-type allowlist stops it.
    const RESULTS_XML: &str = r#"<?xml version="1.0"?>
<sparql xmlns="http://www.w3.org/2005/sparql-results#">
  <head/>
  <results/>
</sparql>
"#;
    let (verdict, level, kind) = fetch_and_resolve(200, Some("application/xml"), RESULTS_XML).await;
    assert_eq!(kind, BodyKind::Other, "SPARQL results are not a service description");
    assert_eq!(verdict, Verdict::Indeterminate);
    assert_eq!(level, None);
}

#[tokio::test]
async fn an_empty_turtle_body_is_indeterminate_not_a_zero_graded_description() {
    // An RDF-specific media type, but zero triples. Parsing clean is not a
    // positive identification of RDF content: an empty 200 is equally
    // consistent with "no description here", which is not something we know.
    let (verdict, level, kind) = fetch_and_resolve(200, Some("text/turtle"), "").await;
    assert_eq!(kind, BodyKind::Other);
    assert_eq!(verdict, Verdict::Indeterminate);
    assert_eq!(level, None);
}

#[tokio::test]
async fn a_404_is_still_absent_at_level_zero() {
    // The gates must not swallow the two genuine absences. (Served as
    // `text/plain`: an HTML body is `Indeterminate` at any status, by the
    // separate and older rule that a query console tells us nothing.)
    let (verdict, level, _) = fetch_and_resolve(404, Some("text/plain"), "not found").await;
    assert_eq!(verdict, Verdict::Absent);
    assert_eq!(level, Some(Level(0)));
}

#[tokio::test]
async fn a_real_turtle_description_is_still_verified_with_a_level() {
    // Nor the ordinary success case.
    let (verdict, level, kind) = fetch_and_resolve(200, Some("text/turtle"), STUB).await;
    assert_eq!(kind, BodyKind::Rdf);
    assert_eq!(verdict, Verdict::Verified);
    assert!(level.is_some(), "a parsed description carries its grade");
}

#[tokio::test]
async fn a_genuine_rdf_payload_under_a_generic_media_type_degrades_to_indeterminate() {
    // The other half of the allowlist, pinned on its own: these four payloads
    // really are RDF and really do parse non-empty under the format
    // `from_media_type` picks, so the non-empty-triple requirement cannot save
    // us here. Only rejecting the generic media type does. The trade is
    // deliberate and stated in `client.rs`: a real description served as
    // `application/xml` degrades to `indeterminate`, which is honest, because
    // the same media types are what a JSON error page and a SPARQL-results
    // document arrive under.
    const NTRIPLES: &str = "<http://example.org/s> <http://example.org/p> <http://example.org/o> .\n";
    const JSONLD: &str = r#"{"@id":"http://example.org/s","http://example.org/p":{"@id":"http://example.org/o"}}"#;
    const RDFXML: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="http://example.org/s">
    <rdf:type rdf:resource="http://example.org/C"/>
  </rdf:Description>
</rdf:RDF>
"#;
    let cases = [
        ("text/plain", NTRIPLES),
        ("application/json", JSONLD),
        ("application/xml", RDFXML),
        ("text/xml", RDFXML),
    ];
    let mut misread = Vec::new();
    for (ctype, body) in cases {
        let (verdict, level, kind) = fetch_and_resolve(200, Some(ctype), body).await;
        if kind != BodyKind::Other || verdict != Verdict::Indeterminate || level.is_some() {
            misread.push(format!("{ctype}: {kind:?} {verdict:?} {level:?}"));
        }
    }
    assert!(misread.is_empty(), "generic media types must not license an RDF verdict: {misread:?}");
}
