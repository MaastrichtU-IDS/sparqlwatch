use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, NotMeasured, NotMeasuredReason, RunId};
use sparqlwatch_prober::metrics::{load_metrics, within_cost, Cost, MetricDef, ProbeKind};
use sparqlwatch_prober::registry::load_endpoints;
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
    // The browser's question, answered with a grant. Separate from the GET
    // mock above on purpose: the two CORS metrics are two different facts, and
    // this endpoint has both.
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

    assert_eq!(rows.len(), defs.len(), "one measurement per metric per endpoint");

    // Assert the verdicts themselves, not just the row count: an
    // implementation that resolved everything to `Absent` -- the exact failure
    // this project exists to prevent -- passed the old assertions.
    let got: BTreeMap<&str, Verdict> =
        rows.iter().map(|r| (r.metric_id.as_str(), r.verdict)).collect();
    let expected: BTreeMap<&str, Verdict> = BTreeMap::from([
        // A SPARQL JSON body proves it speaks the protocol.
        ("availability", Verdict::Verified),
        // The mock sets access-control-allow-origin. No term in the
        // service-description vocabulary can declare CORS, so `cors` carries
        // no `declared_by` and a confirmation is simply `verified`: the
        // declared/observed axis applies only where a declaration is possible.
        ("cors", Verdict::Verified),
        // And it answers the OPTIONS preflight with a wildcard grant that
        // lists GET, so a browser would be allowed to query it too. Same
        // reasoning, same verdict as the `cors` row.
        ("cors-preflight", Verdict::Verified),
        // boolean:true against expect = true. The only shipped metric with a
        // `declared_by`, and this mock serves no parseable description, so the
        // capability is confirmed and undeclared: the one row in this sweep
        // where `undeclared-but-verified` says something about the endpoint.
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
        // Same unbound ?c, same honest absence, at the cheap end of the split.
        ("has-classes", Verdict::Absent),
    ]);
    assert_eq!(got, expected);

    let nq =
        emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &_declarations_read, &[], Cost::Cheap)
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

/// I5. The two CORS metrics ask genuinely different questions, and the closed
/// `match` in `probe_endpoint` makes forgetting to dispatch a kind a compile
/// error but says nothing about dispatching one to the WRONG probe. Re-routing
/// `ProbeKind::Cors` to `client.preflight` used to cause zero test failures,
/// which would publish the preflight's header under the label "Sends
/// access-control-allow-origin on a simple GET".
///
/// This is the endpoint shape the pair exists to distinguish, and the commonest
/// one in the wild: a front-end filter sets the header on a simple GET while the
/// handler refuses `OPTIONS` outright, so a `curl` user sees CORS and a browser
/// gets nothing. Either mis-route flips one of these two verdicts.
#[tokio::test]
async fn the_two_cors_metrics_are_not_the_same_probe() {
    let server = MockServer::start().await;
    // A simple GET is answered, with the header.
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;
    // The browser's preflight is refused. 405 is the endpoint answering the
    // question we asked, which is what licenses an absence claim.
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server).await;

    let defs = load_shipped_metrics();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    let verdict = |id: &str| rows.iter().find(|r| r.metric_id == id).unwrap().verdict;

    assert_eq!(
        verdict("cors"),
        Verdict::Verified,
        "the simple GET carried access-control-allow-origin; routing this metric at the preflight sees only the 405"
    );
    assert_eq!(
        verdict("cors-preflight"),
        Verdict::Absent,
        "the endpoint refused OPTIONS; routing this metric at the simple GET would publish a grant it never made"
    );

    // And the probes really did send the two different requests, rather than
    // agreeing by accident on one.
    let sent = server.received_requests().await.unwrap();
    assert!(sent.iter().any(|r| r.method == Method::OPTIONS), "no preflight was ever sent");
    assert!(sent.iter().any(|r| r.method == Method::GET), "no simple GET was ever sent");
}

#[tokio::test]
async fn an_unreachable_endpoint_yields_indeterminate_not_a_panic() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let (rows, _declarations_read, _not_measured) = run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &[], &client, Budget::default()).await;
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
        cost: Cost::Cheap,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _declarations_read, _not_measured) = run_sweep(&[url], &[def], &[], &client, Budget::default()).await;

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].verdict,
        Verdict::Verified,
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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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
        cost: Cost::Cheap,
    };

    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &[def], &[], &client, Budget::default()).await;

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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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

/// The same declaration as `STUB_TTL` and `STUB_RDFXML`, in JSON-LD, the third
/// syntax `RDF_ACCEPT` asks for. Written with full IRIs and no `@context` so
/// the test reads what the parser sees.
const STUB_JSONLD: &str = r#"{
  "@id": "http://example.org/sparql",
  "@type": "http://www.w3.org/ns/sparql-service-description#Service",
  "http://www.w3.org/ns/sparql-service-description#feature": {
    "@id": "http://www.w3.org/ns/sparql-service-description#UnionDefaultGraph"
  },
  "http://www.w3.org/ns/sparql-service-description#extensionFunction": {
    "@id": "http://www.opengis.net/def/function/geosparql/sfWithin"
  }
}"#;

/// Sweep one endpoint whose queryless description fetch serves `body` under
/// `content_type` and whose query probes all answer. Returns the
/// `service-description` verdict and level, the `geo-functions` verdict, and
/// the endpoint's `declarationsRead` fact: I2 corrupted all four at once.
async fn sweep_description_served_as(
    body: &str,
    content_type: &str,
) -> (Verdict, Option<Level>, Verdict, bool) {
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
    let (rows, read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    let row = |id: &str| rows.iter().find(|r| r.metric_id == id).unwrap();
    assert_eq!(read.len(), 1);
    let description = row("service-description");
    (description.verdict, description.level, row("geo-functions").verdict, read[0].read)
}

/// I2. A `Content-Type` parameter is routine, and it used to split the
/// client's classification from the declaration parse: the client stripped
/// `; charset=utf-8` before choosing a format, `declare.rs` handed the full
/// header to `RdfFormat::from_media_type`, got `None`, and reparsed the body
/// as Turtle. For RDF/XML and JSON-LD that yields zero triples, so one
/// response published three wrong facts at once: `service-description =
/// verified` with `Level(0)`, which means "none served"; `declarationsRead =
/// false` for a document we had just read; and `geo-functions =
/// undeclared-but-verified`, the declaration itself lost.
///
/// `geo-functions` is the assertion that reads the declaration's CONTENT
/// rather than the grade: it is `Verified` only if `geof:sfWithin` was
/// actually parsed out of the body and matched against a working probe. A
/// Turtle fallback that happens to survive cannot fake it.
/// Every published fact this response should produce, asserted for each of the
/// headers a server might serve it under. `geo` is the one that reads the
/// declaration's CONTENT rather than a count or a grade: `Verified` requires
/// `geof:sfWithin` to have been parsed out of this body AND matched against a
/// working probe, so a fallback parser that happens to yield some triples
/// cannot fake it.
async fn assert_declarations_survive(body: &str, content_type: &str) {
    let (verdict, level, geo, read) = sweep_description_served_as(body, content_type).await;
    assert_eq!(verdict, Verdict::Verified, "under {content_type}");
    assert_ne!(level, Some(Level(0)), "a description with real triples is not `none served`: {content_type}");
    assert!(read, "we read this document, so the published fact has to say so: {content_type}");
    assert_eq!(geo, Verdict::Verified, "the declared geof:sfWithin was lost under {content_type}");
}

#[tokio::test]
async fn a_charset_parameter_does_not_lose_an_rdf_xml_descriptions_declarations() {
    // `charset=utf-8` is the routine case and `oxrdfio` happens to tolerate it
    // even on the raw header, so it pins nothing on its own. The other two are
    // headers real servers send that it refuses: the review's live
    // reproduction used a legacy charset, and a quoted parameter value is
    // legal per RFC 9110.
    for ctype in [
        "application/rdf+xml; charset=utf-8",
        "application/rdf+xml; charset=iso-8859-1",
        "application/rdf+xml; charset=\"utf-8\"",
    ] {
        assert_declarations_survive(STUB_RDFXML, ctype).await;
    }
}

#[tokio::test]
async fn a_charset_parameter_does_not_lose_a_json_ld_descriptions_declarations() {
    for ctype in ["application/ld+json; charset=utf-8", "application/ld+json; charset=\"utf-8\""] {
        assert_declarations_survive(STUB_JSONLD, ctype).await;
    }
}

/// The format the old fallback happened to be right about. It has to keep
/// working, which is the only thing these cases can prove.
#[tokio::test]
async fn a_charset_parameter_does_not_lose_a_turtle_descriptions_declarations() {
    for ctype in ["text/turtle; charset=utf-8", "text/turtle; utf-8"] {
        assert_declarations_survive(STUB_TTL, ctype).await;
    }
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
            cost: Cost::Cheap,
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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget).await;
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

/// I6. The whole justification for `declarations_read: &mut bool` is that the
/// flag must be written as soon as the `Declarations` are known, because
/// `run_sweep` wraps `probe_endpoint` in the endpoint budget and a cancelled
/// future returns nothing. Moving that assignment to after the metric loop used
/// to cause zero test failures, while making this endpoint publish
/// `declarationsRead = false` about a description we had just read: exactly the
/// dishonest fact the flag exists to prevent.
///
/// The shape: the queryless description fetch answers at once, then the first
/// metric's query hangs long enough for the endpoint budget to expire, so the
/// loop is cut short after the fetch and before any row.
#[tokio::test]
async fn a_budget_expiry_after_the_fetch_still_publishes_declarations_read() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .set_delay(std::time::Duration::from_millis(400))
            .set_body_string(WORKING_QUERY_RESPONSE))
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
            cost: Cost::Cheap,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(100),
    };
    let client = Client::new(budget).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget).await;

    // The budget really did cut the loop short, or this proves nothing about
    // the write position.
    assert_eq!(rows.len(), defs.len(), "one row per (endpoint, metric) regardless of timing");
    assert!(
        rows.iter().all(|r| r.verdict == Verdict::Indeterminate),
        "the metric loop must have been cut short for this test to say anything"
    );

    assert_eq!(read.len(), 1);
    assert!(
        read[0].read,
        "the description was fetched and parsed before the budget expired, so the published fact must say so"
    );
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
    let (rows, _declarations_read, _not_measured) = run_sweep(&[bare_url.clone(), rich_url.clone()], &defs, &[], &client, Budget::default()).await;

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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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
    let (rows, _declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;

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
    let (rows, declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read, &[], Cost::Cheap).unwrap()
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
    let (rows, declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read, &[], Cost::Cheap).unwrap()
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
    let (rows, declarations_read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read, &[], Cost::Cheap).unwrap()
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
    let (rows, declarations_read, _not_measured) = run_sweep(&[good, broken], &defs, &[], &client, Budget::default()).await;
    emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &declarations_read, &[], Cost::Cheap).unwrap()
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

/// I3. `run_sweep` emits one `declarationsRead` fact per LIST ENTRY, so a URL
/// listed twice put two of them on one endpoint IRI in one run graph, and with
/// two differing fetches they disagreed: `ASK { ?ep :declarationsRead false }`
/// and its negation both succeeded, with nothing in the graph to resolve it.
/// The registry loader is where that is stopped, so this test goes through the
/// loader exactly as `main.rs` does.
#[tokio::test]
async fn a_registry_that_lists_one_url_twice_probes_it_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    let url = format!("{}/sparql", server.uri());
    let endpoints = load_endpoints(&format!("endpoint = [{url:?}, {url:?}]")).unwrap();
    assert_eq!(endpoints.len(), 1, "the loader is what drops the duplicate");

    let defs = load_shipped_metrics();
    let client = Client::new(Budget::default()).unwrap();
    let (rows, read, _not_measured) = run_sweep(&endpoints, &defs, &[], &client, Budget::default()).await;
    let run = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &read, &[], Cost::Cheap).unwrap();

    assert_eq!(count_declarations_read_quads(&run), 1, "one endpoint, one fact, whatever the registry said");
    assert_eq!(rows.len(), defs.len(), "one row per metric, not two");
    let ids: BTreeSet<&str> = rows.iter().map(|r| r.metric_id.as_str()).collect();
    assert_eq!(ids.len(), rows.len(), "no metric may appear twice for one endpoint");
}

/// The other half of the ruling: dedupe is on the exact string, so two
/// spellings of what `same_endpoint` would call one service stay two registry
/// entries and are probed and published as two. Collapsing them would hide a
/// registry problem and publish an endpoint IRI the registry does not contain.
#[tokio::test]
async fn a_near_duplicate_differing_by_a_trailing_slash_stays_two_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    let url = format!("{}/sparql", server.uri());
    let slashed = format!("{url}/");
    let endpoints = load_endpoints(&format!("endpoint = [{url:?}, {slashed:?}]")).unwrap();
    assert_eq!(endpoints, vec![url.clone(), slashed.clone()], "these are two entries");

    let defs = load_shipped_metrics();
    let client = Client::new(Budget::default()).unwrap();
    let (rows, read, _not_measured) = run_sweep(&endpoints, &defs, &[], &client, Budget::default()).await;
    let run = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &read, &[], Cost::Cheap).unwrap();

    assert_eq!(count_declarations_read_quads(&run), 2, "two entries, two facts");
    assert_eq!(rows.len(), 2 * defs.len(), "one row per metric per entry");
    for ep in [&url, &slashed] {
        assert_eq!(
            rows.iter().filter(|r| &r.endpoint == ep).count(),
            defs.len(),
            "{ep} must carry a full set of rows of its own"
        );
    }
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

/// The `{ ... }` group that opens after a `GRAPH ?var`, brace-balanced, or
/// `None` if what follows is not a balanced block. Text-level, because the
/// suite has no SPARQL engine in it: see the named-graph gap in
/// `README.md`'s known limitations for what that cannot establish.
fn graph_block_after(after_graph_var: &str) -> Option<&str> {
    let open = after_graph_var.find('{')?;
    let mut depth = 0usize;
    for (i, c) in after_graph_var[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&after_graph_var[open..open + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `block` uses `?var` as a whole variable rather than as the prefix of
/// a longer name: `?g` must not be found inside `?geometry`.
fn binds(block: &str, var: &str) -> bool {
    let needle = format!("?{var}");
    block.match_indices(&needle).any(|(i, _)| {
        block[i + needle.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

/// Pin both halves of the fix, reading the shipped definitions rather than a
/// fixture so editing the file cannot silently narrow them again.
#[test]
fn the_content_metrics_reach_named_graphs_without_colliding_variables() {
    let defs = load_shipped_metrics();
    for id in ["geo-data", "classes", "has-classes"] {
        let d = defs.iter().find(|d| d.id == id).expect("metric must exist");
        let q = d.query.as_deref().unwrap_or("");
        let var = d.var.as_deref().expect("both metrics read a bound variable");

        assert!(q.contains("GRAPH ?"), "{id} must look in named graphs too");
        assert!(q.to_uppercase().contains("UNION"), "{id} must still look in the default graph");

        // Every GRAPH variable in the query must differ from the result
        // variable. `GRAPH ?g { ?s geo:asWKT ?g }` parses, runs, and can never
        // match: the graph name is joined against the geometry literal. The
        // result is an empty binding set, which resolves to `absent`, which is
        // the exact false negative this metric change exists to remove.
        //
        // And the block itself must BIND the result variable, which is the only
        // thing that makes the named-graph branch capable of contributing a row
        // at all. Without this, `GRAPH ?anyg { ?s a ?other }` satisfies every
        // other assertion here (it contains `GRAPH ?`, its graph variable is
        // not the result variable) while contributing nothing, and a
        // named-graph-only endpoint still publishes `absent`.
        let mut graph_blocks = 0;
        for (i, _) in q.match_indices("GRAPH ?") {
            let rest = &q[i + "GRAPH ?".len()..];
            let g: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            assert!(!g.is_empty(), "{id} has a malformed GRAPH variable");
            assert_ne!(
                g, var,
                "{id} binds ?{var} as its result AND as its graph name, so it can never match"
            );
            let block = graph_block_after(&rest[g.len()..])
                .unwrap_or_else(|| panic!("{id}'s GRAPH ?{g} is not followed by a balanced block"));
            assert!(
                binds(block, var),
                "{id}'s GRAPH ?{g} block does not bind ?{var}, so it can never contribute a row: {block}"
            );
            graph_blocks += 1;
        }
        assert_eq!(graph_blocks, 1, "{id} should look in named graphs in exactly one branch");

        assert!(
            !d.label.to_lowercase().contains("default graph"),
            "{id}'s label still claims default-graph-only scope"
        );
    }

    // Every metric in the shipped file must state its cost explicitly. A
    // reader of the file has no access to the code's default, so a metric
    // silent about cost is unreadable on its own terms even though it still
    // loads as cheap.
    let raw = include_str!("../metrics.toml");
    for block in raw.split("[[metric]]").skip(1) {
        let id = block
            .lines()
            .find_map(|l| l.trim().strip_prefix("id = \"").and_then(|s| s.strip_suffix('"')))
            .unwrap_or("<unknown>");
        let states_cost = block.lines().any(|l| {
            let l = l.trim();
            !l.starts_with('#') && l.starts_with("cost")
        });
        assert!(states_cost, "metric {id} does not state cost explicitly in metrics.toml");
    }
}

/// Pins the `SelectIris` choice on `has-classes` against the exact mistake its
/// own comment in `metrics.toml` warns about. Every other fixture in this file
/// leaves `?c` unbound, so `has-classes` reads `absent` under either probe
/// kind and no existing test can tell `SelectIris` from `AskData`. Only a mock
/// that actually binds `?c` to a URI can separate them: `SelectIris` collects
/// it and confirms the metric, while `AskData` would route it through
/// `Client::ask_literal`'s literal guard, find no literal because the value is
/// an IRI, and quietly publish `absent` for an endpoint that plainly holds
/// typed resources.
#[tokio::test]
async fn has_classes_reads_the_iri_c_binds_through_select_iris_not_ask_data() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/Thing"}}]},"boolean":true}"#,
        ))
        .mount(&server).await;

    let defs = load_shipped_metrics();
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, read, _not_measured) = run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default()).await;
    let run = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision", &rows, &read, &[], Cost::Cheap).unwrap();

    assert_eq!(
        verdict_of(&run, "has-classes"),
        Verdict::Verified,
        "has-classes must be confirmed from an IRI bound to ?c; a metric routed through \
         AskData's literal guard would see no literal here and report absent instead"
    );
}

/// What a sweep produced, for the tests that care about the declined half.
/// A struct rather than a tuple because these tests read two of the three
/// halves and positional `_`s stop saying which is which.
struct Swept {
    rows: Vec<sparqlwatch_prober::emit::MeasurementRow>,
    not_measured: Vec<NotMeasured>,
}

/// Drive the real sweep against a mock, with the definition list already split
/// by the shipped `within_cost`.
async fn sweep_against(server: &MockServer, run: &[MetricDef], declined: &[MetricDef]) -> Swept {
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, _read, not_measured) =
        run_sweep(std::slice::from_ref(&url), run, declined, &client, Budget::default()).await;
    Swept { rows, not_measured }
}

/// An endpoint that answers everything, so the only reason a metric can be
/// missing from the rows is that it was never run.
async fn an_endpoint_that_answers_everything() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/Thing"}}]},"boolean":true}"#,
        ))
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"))
        .mount(&server).await;
    server
}

#[tokio::test]
async fn a_declined_metric_is_recorded_as_not_measured_not_as_indeterminate() {
    // Drive the real sweep with a definition list split by `within_cost`, so
    // this exercises the shipped filter rather than one the test performs.
    let server = an_endpoint_that_answers_everything().await;
    let (run, declined) = within_cost(&load_shipped_metrics(), Cost::Cheap);
    let out = sweep_against(&server, &run, &declined).await;

    assert!(out.rows.iter().all(|r| r.metric_id != "classes"),
            "a declined metric produces no measurement row");
    assert!(out.not_measured.iter().any(|n| n.metric_id == "classes"),
            "and is recorded as not measured instead");
    assert!(out.rows.iter().any(|r| r.metric_id == "has-classes"),
            "while its cheap counterpart still runs");
    // Not an `Indeterminate` row wearing a different hat: no row at all, and a
    // fact that says why.
    assert_eq!(out.rows.len() + out.not_measured.len(), load_shipped_metrics().len(),
               "every definition is still accounted for, in one half or the other");
    assert!(out.not_measured.iter().all(|n| n.reason == NotMeasuredReason::CostCeiling));
}

#[tokio::test]
async fn a_declined_metric_issues_no_request() {
    // The whole point of a cost ceiling. A row we do not publish is worthless if
    // we paid for it anyway.
    let (run, declined) = within_cost(&load_shipped_metrics(), Cost::Cheap);
    let server = an_endpoint_that_answers_everything().await;
    let _ = sweep_against(&server, &run, &declined).await;
    // Queries travel as the `query` URL parameter on a GET (`client.rs`
    // `get_with_body`), so an expensive query that was sent would show up in
    // the recorded URL, percent-encoded but with `DISTINCT` intact.
    let urls: Vec<String> = server.received_requests().await.unwrap()
        .iter().map(|r| r.url.to_string()).collect();
    assert!(urls.iter().any(|u| u.contains("query=")),
            "the cheap half really did issue queries, so an absent DISTINCT means something");
    assert!(!urls.iter().any(|u| u.contains("DISTINCT")),
            "the expensive query was never sent: {urls:?}");
}

#[tokio::test]
async fn a_declined_metric_reaches_the_published_graph_with_no_verdict() {
    // The sweep's declined list has to survive emission, or the requirement
    // ("record not measured") is met in memory and lost on disk.
    let server = an_endpoint_that_answers_everything().await;
    let (run, declined) = within_cost(&load_shipped_metrics(), Cost::Cheap);
    let client = Client::new(Budget::default()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let (rows, read, not_measured) =
        run_sweep(std::slice::from_ref(&url), &run, &declined, &client, Budget::default()).await;
    let nq = emit_nquads(&RunId("test".into()), "2026-08-20T08:00:00Z", "test-revision",
                         &rows, &read, &not_measured, Cost::Cheap).unwrap();
    let quads = quads_of(&nq);

    let classes = NamedNode::new("urn:sparqlwatch:metric:classes").unwrap();
    let subjects: Vec<&oxrdf::NamedOrBlankNode> = quads.iter()
        .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredMetric"
                    && q.object == Term::NamedNode(classes.clone()))
        .map(|q| &q.subject)
        .collect();
    assert_eq!(subjects.len(), 1, "the declined metric appears exactly once, as a fact about not measuring it");
    // And no measurement claims it: the two halves are disjoint on the metric
    // as well as on the subject, so a consumer joining on the metric IRI cannot
    // see a pair that both was and was not measured.
    assert!(!quads.iter().any(|q| q.predicate.as_str() == "http://www.w3.org/ns/dqv#isMeasurementOf"
                && q.object == Term::NamedNode(classes.clone())),
            "a declined metric must never also be the subject of a measurement");
    let subject = subjects[0];
    assert!(quads.iter().any(|q| &q.subject == subject
                && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:NotMeasured").unwrap())),
            "and it is typed as a not-measured fact");
    assert!(!quads.iter().any(|q| &q.subject == subject
                && q.predicate.as_str() == "http://www.w3.org/ns/dqv#value"),
            "a consumer asking for its verdict must get nothing, not a misleading zero");
    // The run itself says which ceiling declined it.
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:maxCost"
                && q.object == Term::Literal(oxrdf::Literal::new_simple_literal("cheap"))));
    // ...while the cheap counterpart is a real measurement with a real verdict.
    assert_eq!(verdict_of(&nq, "has-classes"), Verdict::Verified);
}

/// A declined metric produces one not-measured fact PER ENDPOINT, and every
/// existing test uses a single endpoint, which cannot tell per-endpoint apart
/// from per-run or per-declined-metric. Two endpoints and one declined metric
/// must yield exactly two facts, one naming each.
///
/// This matters at registry scale rather than here: with 548 endpoints and one
/// expensive metric, a per-run fanout would publish 1 fact instead of 548, so a
/// consumer asking "was `classes` measured for THIS endpoint" would get nothing
/// back for 547 of them and could not distinguish that from the metric not
/// existing.
#[tokio::test]
async fn a_declined_metric_is_recorded_once_per_endpoint() {
    let a = an_endpoint_that_answers_everything().await;
    let b = an_endpoint_that_answers_everything().await;
    let (run, declined) = within_cost(&load_shipped_metrics(), Cost::Cheap);
    assert_eq!(declined.len(), 1, "the fixture assumes exactly one expensive metric");

    let client = Client::new(Budget::default()).unwrap();
    let urls = vec![format!("{}/sparql", a.uri()), format!("{}/sparql", b.uri())];
    let (_rows, _read, not_measured) =
        run_sweep(&urls, &run, &declined, &client, Budget::default()).await;

    assert_eq!(
        not_measured.len(),
        2,
        "one fact per (endpoint, declined metric), not one per run: {not_measured:?}"
    );
    let named: std::collections::BTreeSet<&str> =
        not_measured.iter().map(|n| n.endpoint.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> = urls.iter().map(|u| u.as_str()).collect();
    assert_eq!(named, expected, "each endpoint is named by exactly one fact");
}
