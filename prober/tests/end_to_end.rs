use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, NotMeasured, NotMeasuredReason, RunEmission, RunId};
use sparqlwatch_prober::metrics::{load_metrics, within_cost, Cost, MetricDef, ProbeKind};
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::registry::load_endpoints;
use sparqlwatch_prober::run_sweep;
use sparqlwatch_prober::Sweep;
use sparqlwatch_prober::verdict::{Level, Verdict};
use oxrdf::{NamedNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use wiremock::http::Method;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};
mod common;
use common::without_deadlocking;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
        emit_nquads(RunEmission {
            run: &RunId("test".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: "test-revision",
            rows: &rows,
            declarations_read: &_declarations_read,
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        })
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &[], &client, Budget::default())).await;
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
        sample_limit: None,
    };

    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&[url], &[def], &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
        sample_limit: None,
    };

    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &[def], &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
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
            sample_limit: None,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(60),
    };
    let client = Client::new(budget, Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let started = std::time::Instant::now();
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget)).await;
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
            sample_limit: None,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(100),
    };
    let client = Client::new(budget, Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget)).await;

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

/// The case the per-endpoint accumulator exists for, and the one nothing else
/// in this file pins: some metrics finish, and then the endpoint budget
/// expires. `an_endpoint_budget_expiry_still_yields_one_row_per_metric` above
/// reaches no metric at all, so every row it inspects is `Indeterminate`
/// whether or not partial work survived; it cannot tell the two cases apart.
/// Here the first metric completes with a verdict, an `elapsedMs` and a content
/// sample before the stall, so a shape that returns the accumulator out of the
/// cancelled future, rather than writing through one the caller of the timeout
/// owns, loses all three and overwrites an earned verdict with `Indeterminate`.
///
/// The shape: the queryless description fetch and the first metric's query are
/// answered at once, matched on their exact `query` parameter, while the query
/// the remaining metrics send stalls far past the endpoint budget.
#[tokio::test]
async fn a_partial_endpoint_keeps_the_verdicts_it_already_earned() {
    const FAST_QUERY: &str = "SELECT ?s WHERE { ?s a ?c } LIMIT 5";
    const SLOW_QUERY: &str = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1";

    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(STUB_TTL.as_bytes().to_vec(), "text/turtle"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param("query", FAST_QUERY))
        .respond_with(ResponseTemplate::new(200).set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql")).and(query_param("query", SLOW_QUERY))
        .respond_with(ResponseTemplate::new(200)
            .set_delay(std::time::Duration::from_millis(1500))
            .set_body_string(WORKING_QUERY_RESPONSE))
        .mount(&server).await;

    // First an enumerating metric that answers, so there is a verdict, an
    // elapsed time and a sample to lose. `declared_by` is None, so `resolve`
    // grades bound IRIs from a 200 as `Verified`, which is one of the two
    // verdicts a sample may be published under.
    let mut defs = vec![MetricDef {
        id: "classes-sample".into(),
        label: "enumerates classes".into(),
        dimension: "content".into(),
        kind: ProbeKind::SelectIris,
        query: Some(FAST_QUERY.into()),
        expect: None,
        var: Some("s".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        sample_limit: Some(5),
    }];
    // Then the metrics that stall, so the endpoint budget expires with the
    // first metric's results already in hand.
    defs.extend((0..2).map(|i| MetricDef {
        id: format!("liveness-{i}"),
        label: "answers a trivial query".into(),
        dimension: "availability".into(),
        kind: ProbeKind::Liveness,
        query: Some(SLOW_QUERY.into()),
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        sample_limit: None,
    }));

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(400),
    };
    let client = Client::new(budget, Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget)).await;

    assert_eq!(rows.len(), defs.len(), "one row per (endpoint, metric) regardless of timing");
    for (row, def) in rows.iter().zip(&defs) {
        assert_eq!(row.metric_id, def.id, "rows stay aligned with the definitions");
        assert_eq!(row.endpoint, url);
    }
    // The budget really did cut the loop short, or this proves nothing.
    assert!(
        rows[1..].iter().all(|r| r.verdict == Verdict::Indeterminate),
        "the metrics the stall kept us from must be Indeterminate, or the loop was never cut short"
    );
    assert!(
        rows[1..].iter().all(|r| r.elapsed_ms.is_none()),
        "an unmeasured metric has no elapsed time, not a zero one"
    );

    assert_eq!(
        rows[0].verdict,
        Verdict::Verified,
        "the metric that answered before the stall keeps the verdict it earned; the expiry fill must not write over it"
    );
    assert!(
        rows[0].elapsed_ms.is_some(),
        "and keeps the elapsed time that was actually measured"
    );

    assert_eq!(read.len(), 1);
    assert!(
        read[0].read,
        "the description was fetched and parsed before the budget expired, so the published fact must say so"
    );

    assert_eq!(samples.len(), 1, "the sample collected before the stall survives the expiry");
    assert_eq!(samples[0].endpoint, url);
    assert_eq!(samples[0].metric_id, "classes-sample");
    assert_eq!(samples[0].values, vec!["http://example.org/a".to_string()]);
    assert!(!samples[0].truncated, "one value under a limit of five is not truncated");
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&[bare_url.clone(), rich_url.clone()], &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/a", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/plain/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
    emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &declarations_read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap()
}

/// Sweep one mock endpoint where every request, queryless or not, gets
/// `status` with an empty body. The description was never readable.
async fn sweep_with_status(status: u16) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server).await;

    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
    emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &declarations_read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap()
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
    emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &declarations_read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap()
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let good = format!("{}/sparql", server.uri());
    let broken = "http://127.0.0.1:1/sparql".to_string();
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&[good, broken], &defs, &[], &client, Budget::default())).await;
    emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &declarations_read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap()
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&endpoints, &defs, &[], &client, Budget::default())).await;
    let run = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap();

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(&endpoints, &defs, &[], &client, Budget::default())).await;
    let run = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap();

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;
    let run = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &[],
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap();

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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _read, not_measured, content_samples: _content_samples } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), run, declined, &client, Budget::default())).await;
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
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples: _content_samples } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &run, &declined, &client, Budget::default())).await;
    let nq = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &not_measured,
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
    }).unwrap();
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

    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let urls = vec![format!("{}/sparql", a.uri()), format!("{}/sparql", b.uri())];
    let Sweep { rows: _rows, declarations_read: _read, not_measured, content_samples: _content_samples } =
        without_deadlocking(run_sweep(&urls, &run, &declined, &client, Budget::default())).await;

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

/// `availability` must not report `verified` for an endpoint that refused to
/// serve us. A throttle carrying a SPARQL-results body was resolving to a
/// confirmation, because the Liveness positive case was the only one in
/// `resolve()` reading a parsed body without checking the status alongside it.
/// `Cors` is the one deliberate exception, and it is header-justified.
#[tokio::test]
async fn a_throttled_endpoint_is_not_reported_available_even_with_a_parseable_body() {
    for status in [429u16, 503] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("content-type", "application/sparql-results+json")
                    .set_body_raw(r#"{"head":{"vars":["s"]},"results":{"bindings":[]}}"#,
                                  "application/sparql-results+json"),
            )
            .mount(&server)
            .await;

        let defs = load_shipped_metrics();
        let def = defs.iter().find(|d| d.id == "availability").unwrap().clone();
        let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
        let url = format!("{}/sparql", server.uri());
        let Sweep { rows, declarations_read: _read, not_measured: _nm, content_samples: _content_samples } =
            without_deadlocking(run_sweep(std::slice::from_ref(&url), &[def], &[], &client, Budget::default())).await;

        assert_eq!(
            rows[0].verdict,
            Verdict::Indeterminate,
            "status {status} means it refused to answer, so availability is unknown, not confirmed"
        );
    }
}

/// Why a recorded `Retry-After` needs no bound of its own, as a test rather
/// than as prose. A host that asks for an hour gets an hour: the metrics that
/// follow wait at the gate, their metric budgets cancel them, and they report
/// `indeterminate`, which is exactly what never getting to ask looks like. A
/// bound inside the gate would be a second place deciding how long we wait,
/// and the budgets already decide it.
///
/// The budgets are scaled so the hour is cancelled in milliseconds rather than
/// in an hour. The ratios are what the sweep depends on, not the numbers.
#[tokio::test]
async fn a_host_that_asked_for_an_hour_indeterminates_the_rest_of_its_sweep() {
    let server = MockServer::start().await;
    // Throttled once with an hour, and healthy from then on. Answering
    // normally afterwards is what makes the assertions below about the gate
    // rather than about the server: every row is `indeterminate` because we
    // never asked again, not because we were refused again.
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .set_body_string(r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://example.org/a"}}]},"boolean":true}"#))
        .with_priority(2)
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"))
        .mount(&server).await;

    let budget = Budget {
        request: std::time::Duration::from_millis(200),
        metric: std::time::Duration::from_millis(400),
        endpoint: std::time::Duration::from_secs(10),
    };
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    // The real default cap, so the hour is beyond it: we do not wait for our
    // own retry, and the host is deferred all the same.
    let client = Client::new(budget, Politeness::new(std::time::Duration::ZERO)).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget)).await;

    assert_eq!(server.received_requests().await.unwrap().len(), 1,
               "one request, and the hour it asked for held every later probe back");
    let not_indeterminate: Vec<&str> = rows.iter()
        .filter(|r| r.verdict != Verdict::Indeterminate)
        .map(|r| r.metric_id.as_str())
        .collect();
    assert!(not_indeterminate.is_empty(),
            "a metric we never got to ask about must read indeterminate, got {not_indeterminate:?}");
    // Exactly one row measured anything: the queryless description fetch, which
    // is the request that was throttled. Every other metric was cancelled while
    // waiting at the gate, so it carries no `elapsedMs`, because nothing was
    // measured.
    let measured: Vec<&str> = rows.iter().filter(|r| r.elapsed_ms.is_some())
        .map(|r| r.metric_id.as_str()).collect();
    assert_eq!(measured, ["service-description"],
               "only the throttled request measured a time; the cancelled metrics measured nothing");
}

// --- Content samples -------------------------------------------------------
//
// The point of the whole slice: `classes` already ran `SELECT DISTINCT ?c ...
// LIMIT 200`, used the bindings to reach a verdict, and threw them away, so
// the published graph could say "this endpoint has classes: verified" and not
// which classes, for a task whose entire purpose is knowing what is in an
// endpoint before you write a query.

/// An endpoint that binds `?c` to exactly these IRIs, in this order, for every
/// query it is asked. The order is the endpoint's, and it is evidence.
async fn an_endpoint_binding_classes(values: &[&str]) -> MockServer {
    let bindings: Vec<String> = values
        .iter()
        .map(|v| format!(r#"{{"c":{{"type":"uri","value":"{v}"}}}}"#))
        .collect();
    let body = format!(
        r#"{{"head":{{"vars":["c"]}},"results":{{"bindings":[{}]}},"boolean":true}}"#,
        bindings.join(",")
    );
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// A `SelectIris` metric that enumerates up to `limit`. `sample_limit` 200 in
/// the shipped file is impractical to fill from a mock, so the truncation tests
/// drive a definition whose cap is small; `load_metrics` checks the two agree,
/// and the query here is written to match.
fn enumerating_metric(limit: usize) -> MetricDef {
    MetricDef {
        id: "classes-small".into(),
        label: "Distinct classes, small cap".into(),
        dimension: "content".into(),
        kind: ProbeKind::SelectIris,
        query: Some(format!("SELECT DISTINCT ?c WHERE {{ ?s a ?c }} LIMIT {limit}")),
        expect: None,
        var: Some("c".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Expensive,
        sample_limit: Some(limit),
    }
}

async fn sample_from(server: &MockServer, def: &MetricDef) -> Vec<sparqlwatch_prober::emit::ContentSample> {
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { content_samples, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url),
        std::slice::from_ref(def),
        &[],
        &client,
        Budget::default(),
    ))
    .await;
    content_samples
}

/// Deliberately not alphabetical: the endpoint's order is evidence about the
/// endpoint, so an implementation that sorts must fail this, and one that
/// publishes the right NUMBER of placeholder IRIs must fail it too. This
/// project has shipped a count-only assertion before.
const ZEBRA: &str = "http://example.org/Zebra";
const APPLE: &str = "http://example.org/Apple";
const MANGO: &str = "http://example.org/Mango";

#[tokio::test]
async fn the_classes_metric_publishes_the_iris_it_bound() {
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO]).await;
    let defs = load_shipped_metrics();
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default())).await;

    // Only the metric that declared a `sample_limit` publishes one:
    // `has-classes` binds the very same `?c` in this sweep and must publish
    // nothing, or the cheap probe would quietly enumerate too.
    let ids: Vec<&str> = content_samples.iter().map(|s| s.metric_id.as_str()).collect();
    assert_eq!(ids, ["classes"], "only a metric declaring a sample_limit enumerates");
    let sample = &content_samples[0];
    assert_eq!(
        sample.values,
        vec![ZEBRA.to_string(), APPLE.to_string(), MANGO.to_string()],
        "the IRIs the endpoint returned, in its order, not ours"
    );
    assert_eq!(sample.endpoint, url);
    assert!(!sample.truncated, "three of a cap of 200 is not truncated");

    // And it survives emission: a requirement met in memory and lost on disk is
    // not met. Read back as quads, in order.
    let nq = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &not_measured,
        max_cost: Cost::Expensive,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &content_samples,
    }).unwrap();
    let quads = quads_of(&nq);
    let published: Vec<String> = quads.iter()
        .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:sampledValue")
        .map(|q| match &q.object {
            Term::NamedNode(n) => n.as_str().to_string(),
            other => panic!("a sampled value must be an IRI, got {other}"),
        })
        .collect();
    assert_eq!(published, vec![ZEBRA.to_string(), APPLE.to_string(), MANGO.to_string()],
               "the published order is the endpoint's order");
    // The sample is joinable to the endpoint and to the metric that took it.
    let subject = quads.iter()
        .find(|q| q.predicate.as_str() == "urn:sparqlwatch:sampledValue")
        .map(|q| q.subject.clone())
        .expect("a sample must reach the graph");
    assert!(quads.iter().any(|q| q.subject == subject
        && q.predicate.as_str() == "urn:sparqlwatch:sampledFrom"
        && q.object == Term::NamedNode(NamedNode::new(&url).unwrap())));
    assert!(quads.iter().any(|q| q.subject == subject
        && q.predicate.as_str() == "urn:sparqlwatch:sampledBy"
        && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:classes").unwrap())));

    // And the verdict has not moved: this slice adds a fact, it does not
    // regrade anything.
    assert_eq!(verdict_of(&nq, "classes"), Verdict::Verified);
    assert_eq!(verdict_of(&nq, "has-classes"), Verdict::Verified);
}

#[tokio::test]
async fn a_sample_that_fills_the_limit_is_marked_truncated() {
    // Exactly at the cap. A 201st value may exist and we did not see it, so a
    // reader must not treat this list as the endpoint's classes.
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO]).await;
    let samples = sample_from(&server, &enumerating_metric(3)).await;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].values, vec![ZEBRA.to_string(), APPLE.to_string(), MANGO.to_string()]);
    assert!(samples[0].truncated,
            "three values under a cap of three tells us nothing about a fourth");
}

#[tokio::test]
async fn an_endpoint_that_ignores_the_limit_is_still_a_truncated_sample() {
    // The reason the implementation says `>=` and not `==`. Endpoints that
    // ignore `LIMIT` are not hypothetical, and under `==` this case would be
    // published as a complete list: the confident wrong answer this fact
    // exists to prevent.
    let extra = "http://example.org/Quince";
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO, extra]).await;
    let samples = sample_from(&server, &enumerating_metric(3)).await;
    assert_eq!(samples.len(), 1);
    assert_eq!(
        samples[0].values,
        vec![ZEBRA.to_string(), APPLE.to_string(), MANGO.to_string(), extra.to_string()],
        "everything the endpoint sent is kept: we do not trim it to the cap we asked for"
    );
    assert!(samples[0].truncated,
            "an endpoint that returned MORE than the cap has still given us a sample we cannot call complete");
}

#[tokio::test]
async fn a_sample_under_the_limit_is_not_marked_truncated() {
    // Two of a cap of three: we saw them all, for that query's graph scope, so
    // a consumer may treat the list as complete.
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE]).await;
    let samples = sample_from(&server, &enumerating_metric(3)).await;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].values, vec![ZEBRA.to_string(), APPLE.to_string()]);
    assert!(!samples[0].truncated, "two of a cap of three is the whole answer");
}

#[tokio::test]
async fn a_metric_that_binds_nothing_publishes_no_sample() {
    // An empty list is not a sample. The measurement row already says `absent`,
    // and publishing a sample of size zero would give a consumer a node to join
    // that says nothing the verdict did not.
    let server = an_endpoint_binding_classes(&[]).await;
    let samples = sample_from(&server, &enumerating_metric(3)).await;
    assert!(samples.is_empty(), "nothing bound, nothing sampled: {samples:?}");
}

#[tokio::test]
async fn a_declined_metric_publishes_no_sample_and_still_says_why() {
    // At the default cost ceiling `classes` is declined, so there is no sample
    // to publish, and the existing NotMeasured fact is what tells a reader the
    // absence is a choice rather than an empty endpoint. No new machinery: the
    // distinction is already published.
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO]).await;
    let (run, declined) = within_cost(&load_shipped_metrics(), Cost::Cheap);
    assert!(declined.iter().any(|d| d.id == "classes"),
            "the fixture assumes the cheap ceiling declines classes");
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &run, &declined, &client, Budget::default())).await;

    assert!(content_samples.is_empty(),
            "a metric that was never run cannot have sampled anything: {content_samples:?}");
    let nq = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &not_measured,
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &content_samples,
    }).unwrap();
    let quads = quads_of(&nq);
    assert!(!quads.iter().any(|q| q.predicate.as_str().starts_with("urn:sparqlwatch:sample")),
            "no sample quad of any kind reaches the graph");
    // ...and the reader is still told why there is nothing here.
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredMetric"
        && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:classes").unwrap())),
        "the absence is published as a choice, not left as a silence");
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredReason"
        && q.object == Term::Literal(oxrdf::Literal::new_simple_literal("cost-ceiling"))));
    // The cheap counterpart still measured, and its verdict has not moved.
    assert_eq!(verdict_of(&nq, "has-classes"), Verdict::Verified);
}

/// An endpoint that answers every query with `status` and a SPARQL-results
/// body carrying real bindings. Not hypothetical: a service under load, or an
/// intermediary in front of one, answers `429`/`503` with whatever body it has,
/// and `select_iris` parses bindings out of any status.
async fn an_endpoint_answering(status: u16, values: &[&str]) -> MockServer {
    let bindings: Vec<String> = values
        .iter()
        .map(|v| format!(r#"{{"c":{{"type":"uri","value":"{v}"}}}}"#))
        .collect();
    let body = format!(
        r#"{{"head":{{"vars":["c"]}},"results":{{"bindings":[{}]}}}}"#,
        bindings.join(",")
    );
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("content-type", "application/sparql-results+json")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_status_the_resolver_distrusts_publishes_no_sample_at_all() {
    // The verdict and the sample are asserted together in one test, because
    // agreement between them is the whole point: a graph saying both "we could
    // not determine whether this endpoint has classes" and "here are its
    // classes, complete" is worse than either half alone, since a consumer
    // joining on `sampledFrom` never sees the measurement that contradicts it.
    for status in [429u16, 500, 502, 503] {
        let server = an_endpoint_answering(status, &[ZEBRA, APPLE]).await;
        let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
        let url = format!("{}/sparql", server.uri());
        let def = enumerating_metric(3);
        let Sweep { rows, declarations_read: read, not_measured, content_samples } =
            without_deadlocking(run_sweep(
                std::slice::from_ref(&url),
                std::slice::from_ref(&def),
                &[],
                &client,
                Budget::default(),
            ))
            .await;

        assert_eq!(
            rows.iter().map(|r| r.verdict).collect::<Vec<_>>(),
            vec![Verdict::Indeterminate],
            "status {status} carrying bindings is not the engine confirming anything"
        );
        assert!(
            content_samples.is_empty(),
            "status {status}: the resolver refused to trust this response, so there is \
             nothing to publish a sample from: {content_samples:?}"
        );

        // And nothing reaches the graph either: a requirement met in memory and
        // lost on disk is not met.
        let nq = emit_nquads(RunEmission {
            run: &RunId("test".into()),
            generated_at: "2026-08-22T08:00:00Z",
            metric_revision: "test-revision",
            rows: &rows,
            declarations_read: &read,
            not_measured: &not_measured,
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &content_samples,
        })
        .unwrap();
        assert_eq!(
            verdict_of(&nq, "classes-small"),
            Verdict::Indeterminate,
            "status {status}: the measurement is unchanged by this gate"
        );
        assert!(
            !nq.contains("urn:sparqlwatch:sample"),
            "status {status}: no sample quad of any kind, so the graph cannot contradict itself"
        );
        assert!(
            !nq.contains(ZEBRA),
            "status {status}: and no value from the distrusted body leaks in by another route"
        );
    }
}

#[tokio::test]
async fn each_sample_names_its_own_endpoint_its_own_values_and_its_own_metric() {
    // Two endpoints, returning DIFFERENT classes, in one sweep. A
    // single-endpoint fixture cannot tell a correct implementation from one
    // that hardcodes what it names, which is why two mutations survived the
    // whole suite green before this test existed: hardcoding the sample's
    // `metric_id`, and re-attributing every sample to `endpoints[0]`.
    // `content_samples` is a shared accumulator threaded through
    // `probe_endpoint` as a `&mut Vec`, so cross-endpoint leakage is exactly
    // the shape a real bug in it would take.
    let first = an_endpoint_binding_classes(&[ZEBRA, APPLE]).await;
    let second = an_endpoint_binding_classes(&[MANGO]).await;
    let a = format!("{}/sparql", first.uri());
    let b = format!("{}/sparql", second.uri());
    let def = enumerating_metric(3);
    // Deliberately not the shipped id: a sample labelled "classes" whatever
    // took it would pass a fixture built on the shipped metric.
    assert_eq!(def.id, "classes-small", "the fixture's point is an id that is not `classes`");
    let client = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let Sweep { rows, declarations_read: read, not_measured, content_samples } =
        without_deadlocking(run_sweep(
            &[a.clone(), b.clone()],
            std::slice::from_ref(&def),
            &[],
            &client,
            Budget::default(),
        ))
        .await;

    assert_eq!(content_samples.len(), 2, "two endpoints that both sampled are two samples");
    let of = |ep: &str| {
        content_samples
            .iter()
            .find(|s| s.endpoint == ep)
            .unwrap_or_else(|| panic!("no sample attributed to {ep}: {content_samples:?}"))
    };
    // Looked up by endpoint, never by index: the values are what prove the
    // attribution, so each endpoint must carry back exactly what it returned.
    assert_eq!(of(&a).values, vec![ZEBRA.to_string(), APPLE.to_string()]);
    assert_eq!(of(&b).values, vec![MANGO.to_string()]);
    for s in &content_samples {
        assert_eq!(s.metric_id, "classes-small", "the sample names the metric that took it");
    }

    // The same three facts, read back out of the graph, since that is what a
    // consumer actually joins on.
    let nq = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-22T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &not_measured,
        max_cost: Cost::Expensive,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &content_samples,
    })
    .unwrap();
    let quads = quads_of(&nq);
    let subject_for = |ep: &str| {
        quads
            .iter()
            .find(|q| {
                q.predicate.as_str() == "urn:sparqlwatch:sampledFrom"
                    && matches!(&q.object, Term::NamedNode(n) if n.as_str() == ep)
            })
            .map(|q| q.subject.clone())
            .unwrap_or_else(|| panic!("no sample subject sampledFrom {ep}"))
    };
    let published = |ep: &str| -> Vec<String> {
        let subj = subject_for(ep);
        quads
            .iter()
            .filter(|q| q.subject == subj && q.predicate.as_str() == "urn:sparqlwatch:sampledValue")
            .map(|q| match &q.object {
                Term::NamedNode(n) => n.as_str().to_string(),
                other => panic!("a sampled value must be an IRI, got {other}"),
            })
            .collect()
    };
    assert_eq!(published(&a), vec![ZEBRA.to_string(), APPLE.to_string()]);
    assert_eq!(published(&b), vec![MANGO.to_string()]);
    assert_ne!(subject_for(&a), subject_for(&b), "two samples, two subjects");
    for ep in [&a, &b] {
        let subj = subject_for(ep);
        assert!(
            quads.iter().any(|q| q.subject == subj
                && q.predicate.as_str() == "urn:sparqlwatch:sampledBy"
                && q.object
                    == Term::NamedNode(
                        NamedNode::new("urn:sparqlwatch:metric:classes-small").unwrap()
                    )),
            "the sample from {ep} must name the metric that took it"
        );
    }

    // No verdict moved: both endpoints answered, so both are verified, exactly
    // as they were before samples existed.
    assert_eq!(verdict_of(&nq, "classes-small"), Verdict::Verified);
}
