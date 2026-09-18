use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::observe::BodyKind;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
mod common;
use common::without_deadlocking;

#[tokio::test]
async fn ask_reads_boolean_and_cors() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(r#"{"head":{},"boolean":true}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
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

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.body_kind, BodyKind::Html);
    assert_eq!(o.boolean, None);
}

#[tokio::test]
async fn missing_cors_header_is_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"boolean":false}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert!(!o.cors);
    assert_eq!(o.boolean, Some(false));
}

#[tokio::test]
async fn a_connection_failure_becomes_an_error_not_a_panic() {
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    // Port 1 is reserved and nothing listens there.
    let o = without_deadlocking(c.ask("http://127.0.0.1:1/sparql", "ASK{}")).await;
    assert!(o.error.is_some());
    assert_eq!(o.status, None);
}

#[tokio::test]
async fn an_html_body_without_a_content_type_is_still_detected() {
    // The doctype-sniffing branch: no content-type header at all, but the
    // body itself starts with `<!doctype html>`.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string("<!doctype html><html><body>YASGUI</body></html>"))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.body_kind, BodyKind::Html);
}

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

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.select_iris(&format!("{}/sparql", server.uri()), "SELECT ?c WHERE{}", "c")).await;
    assert_eq!(o.bindings, vec![
        "http://example.org/Feature".to_string(),
        "http://example.org/Geometry".to_string(),
    ]);
}

#[tokio::test]
async fn aswkt_probe_rejects_non_literal_objects() {
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

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal(&format!("{}/sparql", server.uri()), "SELECT ?g WHERE{}", "g")).await;
    assert_eq!(o.boolean, Some(false), "an IRI object must not count as geometry");
}

#[tokio::test]
async fn aswkt_probe_accepts_a_literal_object() {
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

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal(&format!("{}/sparql", server.uri()), "SELECT ?g WHERE{}", "g")).await;
    assert_eq!(o.boolean, Some(true));
}

#[tokio::test]
async fn only_the_two_cors_probes_announce_an_origin() {
    // One request shape served all six metrics, so every probe carried an
    // Origin. A server that rejects unknown origins could then perturb the
    // evidence for the metrics that are not about CORS at all -- which
    // compounds the absence-from-a-non-answer problem in resolve().
    //
    // Two probes ask a CORS question and so must announce an origin: the
    // simple-GET `cors` probe and the `preflight` one. Every other probe must
    // not. This test was `only_the_cors_probe_sends_an_origin_header` while
    // there was one; the invariant is unchanged, the count is not.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(r#"{"head":{},"boolean":true}"#))
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*"))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    without_deadlocking(c.ask(&url, "ASK{}")).await;
    without_deadlocking(c.select_iris(&url, "SELECT ?c WHERE{}", "c")).await;
    without_deadlocking(c.ask_literal(&url, "SELECT ?g WHERE{}", "g")).await;
    without_deadlocking(c.fetch_rdf(&url)).await;
    let cors = without_deadlocking(c.cors(&url, "ASK{}")).await;
    assert!(cors.cors, "the cors probe still reads the header back");
    let preflight = without_deadlocking(c.preflight(&url)).await;
    assert_eq!(preflight.allow_origin.as_deref(), Some("*"), "the preflight still reads its headers back");

    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 6);
    let with_origin = seen.iter().filter(|r| r.headers.contains_key("origin")).count();
    assert_eq!(with_origin, 2, "only the two CORS probes may announce an Origin");
    assert!(seen[4].headers.contains_key("origin"), "the simple-GET CORS probe announces one");
    assert!(seen[5].headers.contains_key("origin"), "and so does the preflight");
}

// ---------------------------------------------------------------------------
// The class profile probe.
//
// One query per class, returning a grouped row per property rather than a flat
// list of bindings. It resolves to no verdict: a profile is not a measurement,
// so this probe's output is evidence and the caller publishes it as a
// ContentProfile fact. See Ruling 2 in
// docs/superpowers/specs/2026-08-29-content-profiles-design.md.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_class_profile_reads_one_row_per_property() {
    // The shape measured on 2026-08-29: subjects counted DISTINCT so a
    // multi-valued property cannot exceed the denominator, and the rdf:type row
    // supplying that denominator so no second query is needed.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{
          "head": {"vars": ["p", "subjects", "datatypes", "anyDatatype"]},
          "results": {"bindings": [
            {"p": {"type": "uri", "value": "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"},
             "subjects": {"type": "literal", "value": "500"},
             "datatypes": {"type": "literal", "value": "1"},
             "anyDatatype": {"type": "literal", "value": "IRI"}},
            {"p": {"type": "uri", "value": "http://xmlns.com/foaf/0.1/name"},
             "subjects": {"type": "literal", "value": "500"},
             "datatypes": {"type": "literal", "value": "1"},
             "anyDatatype": {"type": "uri", "value": "http://www.w3.org/2001/XMLSchema#string"}},
            {"p": {"type": "uri", "value": "http://xmlns.com/foaf/0.1/mbox"},
             "subjects": {"type": "literal", "value": "475"},
             "datatypes": {"type": "literal", "value": "1"},
             "anyDatatype": {"type": "literal", "value": "IRI"}}
          ]}
        }"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(
        c.profile_class(&format!("{}/sparql", server.uri()), "SELECT ?p WHERE{}")
    ).await;

    let rows = o.profile.expect("a 200 with grouped rows must yield a profile");
    assert_eq!(rows.len(), 3, "one row per property, rdf:type included");

    // The denominator comes from the rdf:type row, not from a second query.
    let denominator = rows.iter()
        .find(|r| r.property.ends_with("22-rdf-syntax-ns#type"))
        .map(|r| r.subjects)
        .expect("the rdf:type row is the denominator");
    assert_eq!(denominator, 500);

    let mbox = rows.iter().find(|r| r.property.ends_with("mbox")).unwrap();
    assert_eq!(mbox.subjects, 475);
    assert_eq!(mbox.datatypes, 1);
    assert_eq!(mbox.any_datatype.as_deref(), Some("IRI"));

    let name = rows.iter().find(|r| r.property.ends_with("name")).unwrap();
    assert_eq!(
        name.any_datatype.as_deref(),
        Some("http://www.w3.org/2001/XMLSchema#string"),
        "a datatype IRI arrives as a uri term and must survive as its value"
    );
}

#[tokio::test]
async fn a_profile_of_a_refused_query_yields_no_rows_and_no_verdict() {
    // dbpedia refused `SELECT DISTINCT ?p WHERE { ?s ?p ?o }` in 44 ms, which is
    // a policy refusal rather than a timeout. A refusal must produce NO profile
    // rather than an empty one: an empty profile would say the class carries no
    // properties, which is a confident wrong answer.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(400).set_body_string("query refused"))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(
        c.profile_class(&format!("{}/sparql", server.uri()), "SELECT ?p WHERE{}")
    ).await;

    assert!(o.profile.is_none(), "a refusal is not an empty profile");
    assert_eq!(o.status, Some(400));
}

#[tokio::test]
async fn a_profile_row_with_an_unparseable_count_is_dropped_not_guessed() {
    // A count that is not an integer is a row we cannot use. Dropping it loses
    // one property; guessing zero would publish "no subjects carry this", which
    // is a fact about the endpoint that nothing observed.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{
          "head": {"vars": ["p", "subjects"]},
          "results": {"bindings": [
            {"p": {"type": "uri", "value": "http://example.org/good"},
             "subjects": {"type": "literal", "value": "12"}},
            {"p": {"type": "uri", "value": "http://example.org/bad"},
             "subjects": {"type": "literal", "value": "not-a-number"}}
          ]}
        }"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(
        c.profile_class(&format!("{}/sparql", server.uri()), "SELECT ?p WHERE{}")
    ).await;

    let rows = o.profile.expect("the good row still makes a profile");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].property.ends_with("good"));
}

// ---------------------------------------------------------------------------
// The fallback query: a 4xx refusal of the primary form, answered by a simpler
// one. See MetricDef::fallback_query and metrics.toml's geo-data block.
// ---------------------------------------------------------------------------

/// The body sparql.dsmz.de actually returns, copied from a real response on
/// 2026-09-18. Three of the 63 YummyData endpoints answer this way.
const NAMED_GRAPHS_UNSUPPORTED: &str =
    r#"{ "exception": "Not supported: Named Graphs (FROM, GRAPH) are currently not supported" }"#;

#[tokio::test]
async fn a_refused_query_falls_back_and_the_fallback_answers() {
    // The real shape: the UNION form is refused with a 400, the default-graph
    // form answers. Before the fallback this endpoint read `indeterminate` for
    // geo-data -- "we could not tell" -- when the truth was that the half of
    // the query it could answer was never asked.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "GRAPH"))
        .respond_with(ResponseTemplate::new(400).set_body_string(NAMED_GRAPHS_UNSUPPORTED))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"head":{"vars":["g"]},"results":{"bindings":[{"g":{"type":"literal","value":"POINT(5 52)"}}]}}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { { ?s ?p ?g } UNION { GRAPH ?anyg { ?s ?p ?g } } } LIMIT 1",
        Some("SELECT ?g WHERE { ?s ?p ?g } LIMIT 1"),
        "g",
    )).await;
    assert_eq!(o.status, Some(200), "the fallback's answer is what stands");
    assert_eq!(o.boolean, Some(true));
    assert_eq!(o.bindings, vec!["POINT(5 52)".to_string()]);
}

#[tokio::test]
async fn an_empty_fallback_is_an_answer_and_not_a_shrug() {
    // The other half of the licence. A store that rejects GRAPH as unsupported
    // has no named graphs for data to hide in, so an empty default-graph result
    // is the complete answer: `boolean` false, which resolves to `absent`, not
    // the `indeterminate` a refusal alone would have produced.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "GRAPH"))
        .respond_with(ResponseTemplate::new(400).set_body_string(NAMED_GRAPHS_UNSUPPORTED))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"head":{"vars":["g"]},"results":{"bindings":[]}}"#))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { { ?s ?p ?g } UNION { GRAPH ?anyg { ?s ?p ?g } } } LIMIT 1",
        Some("SELECT ?g WHERE { ?s ?p ?g } LIMIT 1"),
        "g",
    )).await;
    assert_eq!(o.status, Some(200));
    assert_eq!(o.boolean, Some(false), "an empty fallback is `absent`, not a shrug");
    assert!(o.bindings.is_empty());
}

#[tokio::test]
async fn a_5xx_does_not_trigger_the_fallback() {
    // The trigger is a 4xx and nothing else. A 500, a timeout or a transport
    // error say nothing about the query's SHAPE, and a second request would be
    // spent on an endpoint that is already struggling. One request, and the
    // observation is the first one's.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream is down"))
        .expect(1)
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { { ?s ?p ?g } UNION { GRAPH ?anyg { ?s ?p ?g } } } LIMIT 1",
        Some("SELECT ?g WHERE { ?s ?p ?g } LIMIT 1"),
        "g",
    )).await;
    assert_eq!(o.status, Some(503));
    assert_eq!(o.boolean, None);
    // `expect(1)` above is the assertion that matters: a second request would
    // fail the mock on drop.
}

#[tokio::test]
async fn a_refused_fallback_leaves_the_first_observation_standing() {
    // Both refused. The FIRST observation is kept, because its status is the
    // one describing the metric's own query rather than the retry's. Asserted
    // on the status alone: a query observation carries no body -- `query_chain`
    // sets `body: None`, and only the declaration fetch retains one -- which I
    // asserted wrongly here first and the test caught.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "GRAPH"))
        .respond_with(ResponseTemplate::new(400).set_body_string(NAMED_GRAPHS_UNSUPPORTED))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no"))
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { { ?s ?p ?g } UNION { GRAPH ?anyg { ?s ?p ?g } } } LIMIT 1",
        Some("SELECT ?g WHERE { ?s ?p ?g } LIMIT 1"),
        "g",
    )).await;
    assert_eq!(o.status, Some(400), "the primary query's own refusal is reported");
    assert_eq!(o.boolean, None, "nothing was established either way");
}

#[tokio::test]
async fn no_fallback_means_one_request_and_todays_behaviour() {
    // Every other AskData metric passes None and must be unaffected.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(400).set_body_string(NAMED_GRAPHS_UNSUPPORTED))
        .expect(1)
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { GRAPH ?anyg { ?s ?p ?g } } LIMIT 1",
        None,
        "g",
    )).await;
    assert_eq!(o.status, Some(400));
}

#[tokio::test]
async fn the_real_dsmz_refusal_shape_triggers_the_fallback() {
    // The response sparql.dsmz.de actually sends, headers included: a 400 with
    // `content-type: application/json` and a JSON body. My first test for this
    // sent the 400 as text/plain and passed while the live endpoint still read
    // `indeterminate`, so the mock was not the thing being measured. This one
    // is the wire shape.
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "GRAPH"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("content-type", "application/json")
                .set_body_string(NAMED_GRAPHS_UNSUPPORTED),
        )
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/sparql-results+json")
                .set_body_string(r#"{"head":{"vars":["g"]},"results":{"bindings":[]}}"#),
        )
        .mount(&server).await;

    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.ask_literal_with_fallback(
        &format!("{}/sparql", server.uri()),
        "SELECT ?g WHERE { { ?s ?p ?g } UNION { GRAPH ?anyg { ?s ?p ?g } } } LIMIT 1",
        Some("SELECT ?g WHERE { ?s ?p ?g } LIMIT 1"),
        "g",
    )).await;
    assert_eq!(o.status, Some(200), "the fallback's 200 must be what stands");
    assert_eq!(o.boolean, Some(false), "an empty fallback is absent, not indeterminate");
}
