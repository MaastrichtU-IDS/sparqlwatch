use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use sparqlwatch_prober::emit::{emit_nquads, NotMeasured, NotMeasuredReason, RunEmission, RunId};
use sparqlwatch_prober::metrics::{
    load_metrics, within_cadence, within_cost, Cadence, Cost, MetricDef, ProbeKind,
};
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::registry::load_endpoints;
use sparqlwatch_prober::emit::RunFooter;
use sparqlwatch_prober::run_sweep;
use sparqlwatch_prober::write::RunWriter;
use sparqlwatch_prober::Sweep;

/// How many of these definitions produce a measurement row.
///
/// Not `defs.len()`: a `ClassProfile` metric publishes profile facts and no
/// verdict, so it has no row and no matrix column. Asking `ProbeKind` keeps
/// these tests and `probe_endpoint`'s dispatch reading the same rule, so
/// shipping another non-measuring kind cannot silently pass here.
fn measured(defs: &[MetricDef]) -> usize {
    defs.iter().filter(|d| d.kind.yields_measurement()).count()
}
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

/// `within_cost`, with each declined metric paired to the reason main.rs gives
/// it. Mirrors the composition in `main.rs` so these tests drive the shape the
/// binary actually builds; the cadence half is exercised separately, in the
/// cadence tests below.
fn cheap_split(
    defs: &[MetricDef],
    ceiling: Cost,
) -> (Vec<MetricDef>, Vec<(MetricDef, NotMeasuredReason)>) {
    let (run, declined) = within_cost(defs, ceiling);
    (
        run,
        declined.into_iter().map(|d| (d, NotMeasuredReason::CostCeiling)).collect(),
    )
}

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(rows.len(), measured(&defs), "one measurement per verdict-bearing metric per endpoint");

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
        // `classes` was here, reading `absent` because ?c is unbound. It was
        // retired as a verdict on 2026-09-04 and its cheap counterpart
        // `has-classes` on 2026-08-28.
        //
        // The content dimension's verdict is this one now. `absent` and not
        // `indeterminate`: this sweep declines nothing, so the profile pass RAN
        // and the enumeration answered, with ?c unbound and therefore no
        // classes at all. The endpoint holds no typed subjects and declares
        // none, which is the one case where absent is the true reading.
        // `indeterminate` is what a declined or failed pass produces, and
        // an_unreachable_endpoint_yields_indeterminate_not_a_panic covers that.
        ("vocabulary-described", Verdict::Absent),
        // The three counts, all `indeterminate`. This mock answers every query
        // with a result set carrying no rows, and a COUNT with no GROUP BY
        // returns exactly one row from any real engine, so no row means the
        // number was never learned. NOT `absent`, which is reserved for an
        // endpoint that answered and said zero.
        ("triple-count", Verdict::Indeterminate),
        ("graph-count", Verdict::Indeterminate),
        ("class-count", Verdict::Indeterminate),
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
            content_profiles: &[],
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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

/// An endpoint nothing answers costs ONE request and one row, not the battery.
///
/// This test asserted the opposite until 2026-09-14: every metric got an
/// `Indeterminate` row, which meant a dead endpoint was asked the same
/// unanswerable question once per metric. bio2rdf.org, whose zone stopped
/// resolving, was costing ~68 seconds a sweep that way. The reachability gate
/// in `probe_endpoint` sends one request and, when nothing answers it,
/// declines the rest as `Unreachable`.
///
/// `Indeterminate` is still right for availability itself: we asked, and not
/// getting an answer is what we found out. It is the OTHER nine that were
/// wrong, because an indeterminate verdict asserts a measurement happened.
#[tokio::test]
async fn an_endpoint_that_answers_nothing_is_asked_once_and_declined_for_the_rest() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let Sweep { rows, not_measured, declarations_read: _declarations_read, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&["http://127.0.0.1:1/sparql".to_string()], &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(rows.len(), 1, "only availability is measured on an endpoint nothing answers");
    assert_eq!(rows[0].metric_id, "availability");
    assert_eq!(rows[0].verdict, Verdict::Indeterminate);

    // Every other metric in the run's definition set is declined, and the
    // reason names the endpoint rather than our budget or our crash.
    let declined: Vec<&str> = not_measured
        .iter()
        .filter(|n| n.reason == NotMeasuredReason::LivenessFailed)
        .map(|n| n.metric_id.as_str())
        .collect();
    assert_eq!(declined.len(), defs.len() - 1, "every metric but availability is declined");
    assert!(!declined.contains(&"availability"), "availability was measured, not declined");
}

/// REACHABLE IS NOT THE SAME AS WORKING, and the gate must not confuse them.
///
/// A host answering HTML is answering: its CORS headers and its service
/// description are plain HTTP facts that can be true while the query engine
/// refuses work. Three of the nine endpoints the DBpedia KG catalog declares
/// are exactly this shape -- `https://query.wikidata.org/` serves the console,
/// not the protocol -- so gating on "availability is not verified" rather than
/// on "nothing answered" would have silently stopped measuring them.
#[tokio::test]
async fn an_endpoint_answering_html_is_reachable_and_gets_the_whole_battery() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html>a console</html>", "text/html"))
        .mount(&server)
        .await;

    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, not_measured, declarations_read: _declarations_read, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(rows.len(), measured(&defs), "an answering host is measured in full");
    assert!(
        !not_measured.iter().any(|n| n.reason == NotMeasuredReason::LivenessFailed),
        "a host that answered must never have its battery declined"
    );
}

/// The gate's answer IS the availability measurement, so it is paid for once.
///
/// Without this the cheapest metric in the set would be the only one probed
/// twice, and an operator reading their logs would see two identical queries a
/// few hundred milliseconds apart -- which is precisely the complaint the
/// once-per-endpoint description fetch exists to avoid.
#[tokio::test]
async fn a_reachable_endpoint_is_asked_the_liveness_question_only_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://e.org/a"}}]}}"#,
            "application/sparql-results+json",
        ))
        .mount(&server)
        .await;

    let live = MetricDef {
        id: "availability".into(),
        label: "Answers a trivial query".into(),
        dimension: "availability".into(),
        kind: ProbeKind::Liveness,
        query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
        fallback_query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: None,
        sample_prefix: None,
        tolerance: None,
    };

    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, .. } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &[live], &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
    assert_eq!(rows.len(), 1);

    // One liveness query, plus the one queryless description fetch every
    // endpoint gets. Two requests, never three.
    let sent = server.received_requests().await.unwrap();
    let with_query = sent.iter().filter(|r| r.url.query().is_some_and(|q| q.contains("query="))).count();
    assert_eq!(with_query, 1, "the liveness question was asked more than once: {sent:#?}");
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
        fallback_query: None,
        expect: None,
        var: Some("thing".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: None,
        sample_prefix: None, tolerance: None,
    };

    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&[url], &[def], &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    let queryless = server.received_requests().await.unwrap().iter()
        .filter(|r| r.method == Method::GET && r.url.query().is_none()).count();
    assert_eq!(queryless, 1, "expected exactly one queryless fetch per endpoint");
    assert_eq!(rows.len(), measured(&defs));
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
        fallback_query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: None,
        sample_prefix: None, tolerance: None,
    };

    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &[def], &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
            // NOT `Liveness`, and the distinction is load-bearing since
            // 2026-09-14. Liveness is the reachability gate: `probe_endpoint`
            // probes it before anything else, so a slow one here would spend
            // the endpoint budget this test needs spent on the metrics AFTER
            // the fast one. `AskFilter` dispatches through the same
            // `client.ask` and takes no `var`, so it stalls identically
            // without being hoisted. What this test is about -- a budget
            // running out mid-loop -- is unchanged.
            id: format!("stalls-{i}"),
            label: "a metric that stalls".into(),
            dimension: "availability".into(),
            kind: ProbeKind::AskFilter,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            fallback_query: None,
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            cadence: Default::default(),
            sample_limit: None,
            sample_prefix: None, tolerance: None,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(60),
    };
    let client = std::sync::Arc::new(Client::new(budget, Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let started = std::time::Instant::now();
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget, NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
    let took = started.elapsed();

    assert_eq!(rows.len(), measured(&defs), "one row per (endpoint, metric) regardless of timing");
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
            // NOT `Liveness`, and the distinction is load-bearing since
            // 2026-09-14. Liveness is the reachability gate: `probe_endpoint`
            // probes it before anything else, so a slow one here would spend
            // the endpoint budget this test needs spent on the metrics AFTER
            // the fast one. `AskFilter` dispatches through the same
            // `client.ask` and takes no `var`, so it stalls identically
            // without being hoisted. What this test is about -- a budget
            // running out mid-loop -- is unchanged.
            id: format!("stalls-{i}"),
            label: "a metric that stalls".into(),
            dimension: "availability".into(),
            kind: ProbeKind::AskFilter,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            fallback_query: None,
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            cadence: Default::default(),
            sample_limit: None,
            sample_prefix: None, tolerance: None,
        })
        .collect();

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(100),
    };
    let client = std::sync::Arc::new(Client::new(budget, Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget, NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    // The budget really did cut the loop short, or this proves nothing about
    // the write position.
    assert_eq!(rows.len(), measured(&defs), "one row per (endpoint, metric) regardless of timing");
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
        fallback_query: None,
        expect: None,
        var: Some("s".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: Some(5),
        sample_prefix: None, tolerance: None,
    }];
    // Then the metrics that stall, so the endpoint budget expires with the
    // first metric's results already in hand.
    defs.extend((0..2).map(|i| MetricDef {
        // NOT `Liveness`: it is the reachability gate and is probed before
        // everything else, which would spend this test's 400 ms endpoint
        // budget before the metric whose earned verdict is the point. Same
        // dispatch (`client.ask`), same stall, no hoisting.
        id: format!("stalls-{i}"),
        label: "a metric that stalls".into(),
        dimension: "availability".into(),
        kind: ProbeKind::AskFilter,
        query: Some(SLOW_QUERY.into()),
        fallback_query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: None,
        sample_prefix: None, tolerance: None,
    }));

    let budget = Budget {
        request: std::time::Duration::from_secs(5),
        metric: std::time::Duration::from_secs(5),
        endpoint: std::time::Duration::from_millis(400),
    };
    let client = std::sync::Arc::new(Client::new(budget, Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget, NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(rows.len(), measured(&defs), "one row per (endpoint, metric) regardless of timing");
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&[bare_url.clone(), rich_url.clone()], &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/a", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/plain/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let good = format!("{}/sparql", server.uri());
    let broken = "http://127.0.0.1:1/sparql".to_string();
    let Sweep { rows, declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&[good, broken], &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
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
    let endpoints = load_endpoints(&format!("endpoint = [{url:?}, {url:?}]"), &[]).unwrap();
    assert_eq!(endpoints.len(), 1, "the loader is what drops the duplicate");

    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&endpoints, &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
    }).unwrap();

    assert_eq!(count_declarations_read_quads(&run), 1, "one endpoint, one fact, whatever the registry said");
    assert_eq!(rows.len(), measured(&defs), "one row per metric, not two");
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
    let endpoints = load_endpoints(&format!("endpoint = [{url:?}, {slashed:?}]"), &[]).unwrap();
    assert_eq!(endpoints, vec![url.clone(), slashed.clone()], "these are two entries");

    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let Sweep { rows, declarations_read: read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(&endpoints, &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
    }).unwrap();

    assert_eq!(count_declarations_read_quads(&run), 2, "two entries, two facts");
    assert_eq!(rows.len(), 2 * measured(&defs), "one row per metric per entry");
    for ep in [&url, &slashed] {
        assert_eq!(
            rows.iter().filter(|r| &r.endpoint == ep).count(),
            measured(&defs),
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
        let verdict = verdict_of(&run, &d.id);
        // THE RULE, of every declaration-backed metric: a description we could
        // not read supports no claim about what was declared. All three of
        // these assert one.
        assert!(
            !matches!(
                verdict,
                Verdict::Verified | Verdict::DeclaredOnly | Verdict::DeclaredButWrong
            ),
            "metric {} claimed a declaration from a description we could not read: {verdict:?}",
            d.id
        );
        // And for a metric that actually SENT a probe here, the confirmation is
        // real, so the verdict is pinned exactly. A derived metric is excluded
        // because nothing confirmed anything for it: `vocabulary-described`
        // grades the class profile pass, which this sweep never ran, so it
        // reads `indeterminate` and that is the honest answer rather than a
        // weaker one. Revision 1 asserted only that the verdict was not
        // `declared-only` or `declared-but-wrong`, which a `verified` slips
        // past; that sharpness is kept above.
        // ...and only for a kind this fixture's mock can actually satisfy. It
        // answers ASK and SELECT with one generic body and binds no aggregate,
        // so a `Counted` metric probed and confirmed NOTHING here: its COUNT
        // came back with no row, which is not a number, and it reads
        // `indeterminate`. The rule above still binds it, which is the part
        // that matters; pinning it to `undeclared-but-verified` would be
        // pinning the mock rather than the prober.
        if d.kind.dispatched_per_metric() && d.kind != ProbeKind::Counted {
            assert_eq!(
                verdict,
                Verdict::UndeclaredButVerified,
                "metric {} probed and confirmed, so it must read undeclared-but-verified",
                d.id
            );
        }
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
    for id in ["geo-data", "class-profiles"] {
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

/// Pins the class enumeration against the exact mistake `metrics.toml` warns
/// about, now that the enumeration lives in the profile pass.
///
/// The warning and this test were about `has-classes` until 2026-08-28 and
/// about `classes` until 2026-09-04, and the trap is unchanged across all
/// three because the query is: `?c` in `?s a ?c` binds an IRI, and a
/// literal-extracting path (`Client::ask_literal`, whose literal guard sits at
/// `client.rs:472`) finds no literal in one.
///
/// What changed is where the damage shows. `classes` published a verdict, so
/// the mistake read as `absent` for an endpoint plainly full of typed
/// resources. The pass publishes no verdict, so it now reads as an EMPTY
/// SAMPLE, and the assertion moved with it. Every other fixture in this file
/// leaves `?c` unbound, so only a mock that binds it to a URI can tell the two
/// paths apart at all.
#[tokio::test]
async fn the_class_enumeration_collects_the_iri_c_binds_rather_than_seeking_a_literal() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/Thing"}}]},"boolean":true}"#,
        ))
        .mount(&server).await;

    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows: _rows, declarations_read: _read, not_measured: _not_measured, content_samples, failed_endpoints: _failed_endpoints } = without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    let sampled: Vec<&str> = content_samples
        .iter()
        .flat_map(|s| s.values.iter().map(|v| v.as_str()))
        .collect();
    assert_eq!(
        sampled,
        ["http://example.org/Thing"],
        "the enumeration must collect the IRI bound to ?c; a path through \
         AskData's literal guard would find no literal here and sample nothing"
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
async fn sweep_against(
    server: &MockServer,
    run: &[MetricDef],
    declined: &[(MetricDef, NotMeasuredReason)],
) -> Swept {
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _read, not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), run, declined, &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
    let (run, declined) = cheap_split(&load_shipped_metrics(), Cost::Cheap);
    let out = sweep_against(&server, &run, &declined).await;

    // Named as the rule over the whole declined set, not as one example. The
    // comment below already learned this when `has-classes` went; `classes`
    // went the same way on 2026-09-04 and this assertion no longer has to move.
    assert!(!declined.is_empty(), "the cheap ceiling must decline something");
    for (d, _reason) in &declined {
        assert!(out.rows.iter().all(|r| r.metric_id != d.id),
                "{}: a declined metric produces no measurement row", d.id);
        assert!(out.not_measured.iter().any(|n| n.metric_id == d.id),
                "{}: and is recorded as not measured instead", d.id);
    }
    // Every cheap definition ran, asserted as a property rather than by naming
    // one: this said `has-classes` until that metric was removed on 2026-08-28,
    // and a test that names an example breaks when the example goes while a
    // test that names the rule does not.
    for cheap in &run {
        assert!(out.rows.iter().any(|r| r.metric_id == cheap.id),
                "{} is within the ceiling and produced no row", cheap.id);
    }
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
    let (run, declined) = cheap_split(&load_shipped_metrics(), Cost::Cheap);
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
    let (run, declined) = cheap_split(&load_shipped_metrics(), Cost::Cheap);
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &run, &declined, &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();
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
        content_profiles: &[],
    }).unwrap();
    let quads = quads_of(&nq);

    // Over every declined metric rather than one named example, for the reason
    // a_declined_metric_is_recorded_as_not_measured_not_as_indeterminate gives.
    assert!(!declined.is_empty(), "the cheap ceiling must decline something");
    for (d, _reason) in &declined {
        let iri = NamedNode::new(format!("urn:sparqlwatch:metric:{}", d.id)).unwrap();
        let subjects: Vec<&oxrdf::NamedOrBlankNode> = quads.iter()
            .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredMetric"
                        && q.object == Term::NamedNode(iri.clone()))
            .map(|q| &q.subject)
            .collect();
        assert_eq!(subjects.len(), 1,
                   "{}: appears exactly once, as a fact about not measuring it", d.id);
        // And no measurement claims it: the two halves are disjoint on the
        // metric as well as on the subject, so a consumer joining on the metric
        // IRI cannot see a pair that both was and was not measured.
        assert!(!quads.iter().any(|q| q.predicate.as_str() == "http://www.w3.org/ns/dqv#isMeasurementOf"
                    && q.object == Term::NamedNode(iri.clone())),
                "{}: a declined metric must never also be the subject of a measurement", d.id);

        // Inside the loop with the rest, so these hold for every declined
        // metric rather than for whichever one happened to be first.
        let subject = subjects[0];
        assert!(quads.iter().any(|q| &q.subject == subject
                    && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:NotMeasured").unwrap())),
                "{}: and it is typed as a not-measured fact", d.id);
        assert!(!quads.iter().any(|q| &q.subject == subject
                    && q.predicate.as_str() == "http://www.w3.org/ns/dqv#value"),
                "{}: a consumer asking for its verdict must get nothing, not a misleading zero", d.id);
    }
    // The run itself says which ceiling declined it.
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:maxCost"
                && q.object == Term::Literal(oxrdf::Literal::new_simple_literal("cheap"))));
    // ...while a metric within the ceiling is a real measurement with a real
    // verdict. This named `has-classes` until 2026-08-28.
    assert_eq!(verdict_of(&nq, "availability"), Verdict::Verified);
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
    let (run, declined) = cheap_split(&load_shipped_metrics(), Cost::Cheap);
    assert!(!declined.is_empty(), "the fixture needs at least one expensive metric to decline");

    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let urls = vec![format!("{}/sparql", a.uri()), format!("{}/sparql", b.uri())];
    let Sweep { rows: _rows, declarations_read: _read, not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(&urls, &run, &declined, &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(
        not_measured.len(),
        urls.len() * declined.len(),
        "one fact per (endpoint, declined metric), not one per run: {not_measured:?}"
    );
    // The cross product itself, not just its size: the count alone would pass if
    // one endpoint carried every fact and the other none, which is exactly the
    // per-run fanout this test exists to rule out.
    let named: std::collections::BTreeSet<(&str, &str)> =
        not_measured.iter().map(|n| (n.endpoint.as_str(), n.metric_id.as_str())).collect();
    let expected: std::collections::BTreeSet<(&str, &str)> = urls
        .iter()
        .flat_map(|u| declined.iter().map(move |(d, _)| (u.as_str(), d.id.as_str())))
        .collect();
    assert_eq!(named, expected, "every (endpoint, declined metric) pair is named exactly once");
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
        let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
        let url = format!("{}/sparql", server.uri());
        let Sweep { rows, declarations_read: _read, not_measured: _nm, content_samples: _content_samples, failed_endpoints: _failed_endpoints } =
            without_deadlocking(run_sweep(std::slice::from_ref(&url), &[def], &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
    let client = std::sync::Arc::new(Client::new(budget, Politeness::new(std::time::Duration::ZERO)).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: _declarations_read, not_measured: _not_measured, content_samples: _content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, budget, NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert_eq!(server.received_requests().await.unwrap().len(), 1,
               "one request, and the hour it asked for held every later probe back");
    let not_indeterminate: Vec<&str> = rows.iter()
        .filter(|r| r.verdict != Verdict::Indeterminate)
        .map(|r| r.metric_id.as_str())
        .collect();
    assert!(not_indeterminate.is_empty(),
            "a metric we never got to ask about must read indeterminate, got {not_indeterminate:?}");
    // Exactly one row measured anything: whichever request went first is the
    // one that met the 429. Since 2026-09-14 that is `availability`, because
    // the reachability gate asks the liveness question before anything else --
    // it used to be `service-description`, the queryless description fetch.
    // What this test is about is unchanged: one request, and the hour it asked
    // for held every later probe back. Every other metric was cancelled while
    // waiting at the gate, so it carries no `elapsedMs`, because nothing was
    // measured.
    //
    // A 429 carries a status line, so the endpoint is REACHABLE and the gate
    // does not decline anything here. That is the intended reading: a host
    // refusing us for an hour is not a host that is not there.
    let measured: Vec<&str> = rows.iter().filter(|r| r.elapsed_ms.is_some())
        .map(|r| r.metric_id.as_str()).collect();
    assert_eq!(measured, ["availability"],
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
        fallback_query: None,
        expect: None,
        var: Some("c".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Expensive,
        cadence: Default::default(),
        sample_limit: Some(limit),
        sample_prefix: None, tolerance: None,
    }
}

async fn sample_from(server: &MockServer, def: &MetricDef) -> Vec<sparqlwatch_prober::emit::ContentSample> {
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { content_samples, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url),
        std::slice::from_ref(def),
        &[],
        &client,
        Budget::default(),
        NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    ))
    .await.unwrap();
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
async fn the_class_enumeration_publishes_the_iris_it_bound() {
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO]).await;
    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    // Only a metric that declared a `sample_limit` publishes one. This case was
    // sharper while `has-classes` existed, because it bound the very same `?c`
    // and had to publish nothing or the cheap probe would have quietly
    // enumerated too; with that metric gone the assertion is that no OTHER
    // metric in the shipped set produces a sample.
    //
    // ONE id. It was two while `classes` and `class-profiles` both enumerated,
    // which meant sending the same `DISTINCT ?c` query to every endpoint twice;
    // retiring `classes` as a verdict on 2026-09-04 ended that, which is
    // Ruling 4 in docs/superpowers/specs/2026-08-29-content-profiles-design.md
    // carried out. A second entry here means the duplication is back.
    let ids: Vec<&str> = content_samples.iter().map(|s| s.metric_id.as_str()).collect();
    assert_eq!(
        ids,
        ["class-profiles"],
        "exactly one metric enumerates, so an endpoint is asked for its classes once"
    );
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
        content_profiles: &[],
    }).unwrap();
    let quads = quads_of(&nq);
    // Grouped by sample subject rather than flattened, which is what the
    // invariant was always about: one sample's values in the endpoint's order.
    // The grouping was forced when `classes` and `class-profiles` both
    // enumerated and a flat list compared the order against two copies of
    // itself. One metric enumerates again, and the grouping stays because it
    // states the per-sample rule rather than relying on there being one.
    let mut by_sample: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for q in quads.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:sampledValue") {
        let value = match &q.object {
            Term::NamedNode(n) => n.as_str().to_string(),
            other => panic!("a sampled value must be an IRI, got {other}"),
        };
        by_sample.entry(q.subject.to_string()).or_default().push(value);
    }
    assert_eq!(by_sample.len(), 1, "one sample subject per enumerating metric");
    let in_endpoint_order = vec![ZEBRA.to_string(), APPLE.to_string(), MANGO.to_string()];
    for (subject, values) in &by_sample {
        assert_eq!(values, &in_endpoint_order,
                   "{subject} must publish in the endpoint's order");
    }
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
        && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:class-profiles").unwrap())));

    // And no verdict moved: this slice adds facts, it does not regrade
    // anything. It read `classes` until that metric was retired on 2026-09-04,
    // and `availability` is the one every sweep runs, so naming it does not pin
    // this test to a definition that may change again.
    assert_eq!(verdict_of(&nq, "availability"), Verdict::Verified);
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
    // At the default cost ceiling every sampling metric is declined, so there
    // is no sample to publish, and the existing NotMeasured fact is what tells
    // a reader the absence is a choice rather than an empty endpoint. No new
    // machinery: the distinction is already published.
    let server = an_endpoint_binding_classes(&[ZEBRA, APPLE, MANGO]).await;
    let (run, declined) = cheap_split(&load_shipped_metrics(), Cost::Cheap);
    assert!(declined.iter().any(|(d, _)| d.sample_limit.is_some()),
            "the fixture assumes the cheap ceiling declines every sampling metric");
    assert!(run.iter().all(|d| d.sample_limit.is_none()),
            "and that none of the cheap half samples, or a sample would be legitimate");
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, not_measured, content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &run, &declined, &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

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
        content_profiles: &[],
    }).unwrap();
    let quads = quads_of(&nq);
    assert!(!quads.iter().any(|q| q.predicate.as_str().starts_with("urn:sparqlwatch:sample")),
            "no sample quad of any kind reaches the graph");
    // ...and the reader is still told why there is nothing here.
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredMetric"
        && q.object == Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:class-profiles").unwrap())),
        "the absence is published as a choice, not left as a silence");
    assert!(quads.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredReason"
        && q.object == Term::Literal(oxrdf::Literal::new_simple_literal("cost-ceiling"))));
    // A cheap metric still measured, and its verdict has not moved. This named
    // `has-classes` until that metric went on 2026-08-28; availability is the
    // one every sweep runs and the one this file can name without pinning the
    // test to a definition that may change again.
    assert_eq!(verdict_of(&nq, "availability"), Verdict::Verified);
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
        let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
        let url = format!("{}/sparql", server.uri());
        let def = enumerating_metric(3);
        let Sweep { rows, declarations_read: read, not_measured, content_samples, failed_endpoints: _failed_endpoints } =
            without_deadlocking(run_sweep(
                std::slice::from_ref(&url),
                std::slice::from_ref(&def),
                &[],
                &client,
                Budget::default(),
                NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
            ))
            .await.unwrap();

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
            content_profiles: &[],
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
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let Sweep { rows, declarations_read: read, not_measured, content_samples, failed_endpoints: _failed_endpoints } =
        without_deadlocking(run_sweep(
            &[a.clone(), b.clone()],
            std::slice::from_ref(&def),
            &[],
            &client,
            Budget::default(),
            NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
        ))
        .await.unwrap();

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
        content_profiles: &[],
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

// ---------------------------------------------------------------------------
// Bounded concurrency, reassembled by input slot.
//
// Every assertion below is read off the mocks' own arrival records rather than
// off a wall-clock bound on the sweep. A generous bound on a multi-second
// baseline is satisfied by no overlap at all, and CI is not a quiet machine.
// ---------------------------------------------------------------------------

/// One request reaching a mock: which endpoint it was aimed at, and when.
///
/// The instant is the ARRIVAL, not the reply: `wiremock` calls `respond` when
/// the request lands and applies any configured delay afterwards.
#[derive(Clone, Copy, Debug)]
struct Arrival {
    label: &'static str,
    at: std::time::Instant,
}

/// A mock that records every arrival and then answers after a fixed
/// server-side delay.
///
/// The delay is what makes the assertions below structural rather than a guess
/// about machine speed: a request this mock answered after `delay` was in
/// flight over at least `[at, at + delay]`, so two arrivals less than `delay`
/// apart overlapped, and two more than `delay + min_gap` apart did not.
struct Recording {
    label: &'static str,
    delay: std::time::Duration,
    log: std::sync::Arc<std::sync::Mutex<Vec<Arrival>>>,
}

impl wiremock::Respond for Recording {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        // A poisoned log means a test assertion already panicked while holding
        // it, and the test is failing either way; recovering the inner vec
        // keeps the failure the assertion rather than a second panic in here.
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arrival { label: self.label, at: std::time::Instant::now() });
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            // One body that satisfies both metrics `probe_and_declined` builds:
            // a `boolean` for the liveness probe and one IRI binding for the
            // enumerating one, which is what makes a content sample exist.
            .set_body_string(
                r#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/C"}}]},"boolean":true}"#,
            )
            .set_delay(self.delay)
    }
}

type Log = std::sync::Arc<std::sync::Mutex<Vec<Arrival>>>;

fn new_log() -> Log {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
}

/// Every arrival recorded so far, in the order they landed.
fn arrivals(log: &Log) -> Vec<Arrival> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Mount a recording mock for one path on one server. GET only, which is every
/// request the two metrics below make: one queryless description fetch and one
/// query per metric.
async fn mount_recording(
    server: &MockServer,
    at_path: &'static str,
    label: &'static str,
    delay: std::time::Duration,
    log: &Log,
) {
    Mock::given(method("GET"))
        .and(path(at_path))
        .respond_with(Recording { label, delay, log: std::sync::Arc::clone(log) })
        .mount(server)
        .await;
}

/// Two metrics to run and one to decline, so a sweep over them fills all four
/// of `Sweep`'s fact lists for every endpoint: two rows, one
/// `declarationsRead`, one content sample from the enumerating metric, and one
/// `NotMeasured` from the declined one. All four are reassembled by slot, so a
/// test of output order has to be able to see all four.
///
/// Three requests per endpoint follow from this: the one queryless description
/// fetch, plus one query per metric that runs.
fn probe_and_declined() -> (Vec<MetricDef>, Vec<(MetricDef, NotMeasuredReason)>) {
    let run = vec![
        MetricDef {
            id: "availability".into(),
            label: "answers a trivial query".into(),
            dimension: "availability".into(),
            kind: ProbeKind::Liveness,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            fallback_query: None,
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            cadence: Default::default(),
            sample_limit: None,
            sample_prefix: None, tolerance: None,
        },
        MetricDef {
            id: "classes-small".into(),
            label: "distinct classes, capped".into(),
            dimension: "content".into(),
            kind: ProbeKind::SelectIris,
            query: Some("SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 1".into()),
            fallback_query: None,
            expect: None,
            var: Some("c".into()),
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            cadence: Default::default(),
            sample_limit: Some(1),
            sample_prefix: None, tolerance: None,
        },
    ];
    let declined = vec![MetricDef {
        id: "classes".into(),
        label: "distinct classes".into(),
        dimension: "content".into(),
        kind: ProbeKind::SelectIris,
        query: Some("SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200".into()),
        fallback_query: None,
        expect: None,
        var: Some("c".into()),
        declared_by: None,
        graded: false,
        cost: Cost::Expensive,
        cadence: Default::default(),
        sample_limit: Some(200),
        sample_prefix: None, tolerance: None,
    }];
    let declined = declined
        .into_iter()
        .map(|d| (d, NotMeasuredReason::CostCeiling))
        .collect();
    (run, declined)
}

/// The most requests that were ever in flight at once, read off the arrival
/// records: a request answered after `delay` occupied at least
/// `[at, at + delay]`, so the count at one arrival is how many recorded
/// intervals cover that instant.
fn max_in_flight(log: &[Arrival], delay: std::time::Duration) -> usize {
    log.iter()
        .map(|probe| {
            log.iter()
                .filter(|other| other.at <= probe.at && probe.at < other.at + delay)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// The labels of `log`, in arrival order, keeping only the ones in `of`.
fn label_sequence(log: &[Arrival], of: &[&str]) -> Vec<&'static str> {
    log.iter().filter(|a| of.contains(&a.label)).map(|a| a.label).collect()
}

/// Two properties, not one, and they are different properties: the FILE is in
/// completion order, because a chunk is written when its endpoint finishes,
/// which is the whole point of writing incrementally; the returned `Sweep` is in
/// INPUT order, because it is built from the slots after the last chunk was
/// written. This test was `output_order_is_input_order_not_completion_order`
/// until stage 1c-b4, whose first half made half of it false.
#[tokio::test]
async fn the_file_is_in_completion_order_and_the_sweep_is_in_input_order() {
    // The first endpoint answers slowly, the second at once, and they are on
    // two hosts so both run at once. A sequential sweep produces input order
    // for the trivial reason that nothing overlapped; here the second endpoint
    // finishes first, so both halves below have something to say.
    let slow = MockServer::start().await;
    let fast = MockServer::start().await;
    let log = new_log();
    mount_recording(&slow, "/sparql", "slow", std::time::Duration::from_millis(200), &log).await;
    mount_recording(&fast, "/sparql", "fast", std::time::Duration::ZERO, &log).await;

    let (run, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![format!("{}/sparql", slow.uri()), format!("{}/sparql", fast.uri())];
    let dir = tempdir("order");
    let out = dir.join("run.nq");
    let run_id = RunId(RUN_AT.into());
    let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();
    let Sweep { rows, declarations_read, not_measured, content_samples, failed_endpoints } =
        without_deadlocking(run_sweep(
            &eps,
            &run,
            &declined,
            &client,
            Budget::default(),
            NonZeroUsize::new(2).unwrap(), &Default::default(), &mut writer,
        ))
        .await.unwrap();
    writer.finish(RunFooter { run: &run_id, failed_endpoints }).unwrap();

    assert_eq!(failed_endpoints, 0, "both endpoints answered, so nothing failed");

    // The file: the endpoint that finished first is the endpoint whose chunk
    // was written first.
    assert_eq!(
        markers(&std::fs::read(&out).unwrap()),
        vec![eps[1].clone(), eps[0].clone()],
        "the chunks are in completion order, so the fast endpoint's marker is first"
    );

    // The precondition this test rests on: the second endpoint really did
    // finish before the first. Without it the assertions below hold trivially.
    let log = arrivals(&log);
    let last_fast = log.iter().rposition(|a| a.label == "fast").expect("the fast endpoint was probed");
    let last_slow = log.iter().rposition(|a| a.label == "slow").expect("the slow endpoint was probed");
    assert!(
        last_fast < last_slow,
        "the second endpoint has to finish first or this test proves nothing: {log:?}"
    );

    // The returned `Sweep`, all four fact lists and not just rows. Every one of
    // them is reassembled by slot, so every one of them can be got wrong
    // independently.
    assert_eq!(
        rows.iter().map(|r| (r.endpoint.as_str(), r.metric_id.as_str())).collect::<Vec<_>>(),
        vec![
            (eps[0].as_str(), "availability"),
            (eps[0].as_str(), "classes-small"),
            (eps[1].as_str(), "availability"),
            (eps[1].as_str(), "classes-small"),
        ],
        "rows are in input order, and within an endpoint in definition order"
    );
    assert_eq!(
        declarations_read.iter().map(|d| d.endpoint.as_str()).collect::<Vec<_>>(),
        vec![eps[0].as_str(), eps[1].as_str()],
        "one declarationsRead per endpoint, in input order"
    );
    assert_eq!(
        not_measured.iter().map(|n| (n.endpoint.as_str(), n.metric_id.as_str())).collect::<Vec<_>>(),
        vec![(eps[0].as_str(), "classes"), (eps[1].as_str(), "classes")],
        "declined facts are in input order too"
    );
    assert_eq!(
        content_samples.iter().map(|s| s.endpoint.as_str()).collect::<Vec<_>>(),
        vec![eps[0].as_str(), eps[1].as_str()],
        "samples are in input order too"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn two_endpoints_on_different_hosts_interleave() {
    // Overlap as a structural fact rather than a duration: the assertion is
    // that a request to the second endpoint arrived BETWEEN two requests to
    // the first. No sequential sweep can produce that, whatever the machine
    // speed, and it needs no timing threshold.
    let slow = MockServer::start().await;
    let fast = MockServer::start().await;
    let log = new_log();
    mount_recording(&slow, "/sparql", "slow", std::time::Duration::from_millis(200), &log).await;
    mount_recording(&fast, "/sparql", "fast", std::time::Duration::ZERO, &log).await;

    let (run, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![format!("{}/sparql", slow.uri()), format!("{}/sparql", fast.uri())];
    let Sweep { rows, failed_endpoints, .. } = without_deadlocking(run_sweep(
        &eps,
        &run,
        &declined,
        &client,
        Budget::default(),
        NonZeroUsize::new(2).unwrap(), &Default::default(), &mut common::discarding(),
    ))
    .await.unwrap();
    assert_eq!(failed_endpoints, 0);
    assert_eq!(rows.len(), eps.len() * run.len(), "both endpoints were measured in full");

    let log = arrivals(&log);
    let labels = label_sequence(&log, &["slow", "fast"]);
    let first_slow = labels.iter().position(|l| *l == "slow").expect("the first endpoint was probed");
    let last_slow = labels.iter().rposition(|l| *l == "slow").expect("the first endpoint was probed");
    assert!(
        labels[first_slow..last_slow].contains(&"fast"),
        "no request to the second host arrived between two to the first, so nothing overlapped: {labels:?}"
    );
}

#[tokio::test]
async fn two_endpoints_on_one_host_never_overlap() {
    // ONE MockServer with two paths. Two servers would be two `host_key`s,
    // because `host_key` keeps a non-default port, so a two-server version
    // would prove nothing about one host.
    //
    // Both endpoints are in one host group, and a group is one sequential
    // task, so this is a structural property rather than a spacing one: every
    // request of the first endpoint is served before the first request of the
    // second, and no two requests to this host are ever in flight together.
    //
    // Wrapped in `without_deadlocking` rather than an ad-hoc timeout: a
    // politeness reentrancy mistake shows up here as a hang, and that helper's
    // docstring records the 4m47s hang it was built for.
    const DELAY: std::time::Duration = std::time::Duration::from_millis(80);
    const GAP: std::time::Duration = std::time::Duration::from_millis(100);

    let server = MockServer::start().await;
    let log = new_log();
    mount_recording(&server, "/a", "a", DELAY, &log).await;
    mount_recording(&server, "/b", "b", DELAY, &log).await;

    let (run, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::new(GAP)).unwrap());
    let eps = vec![format!("{}/a", server.uri()), format!("{}/b", server.uri())];
    let Sweep { rows, failed_endpoints, .. } = without_deadlocking(run_sweep(
        &eps,
        &run,
        &declined,
        &client,
        Budget::default(),
        NonZeroUsize::new(2).unwrap(), &Default::default(), &mut common::discarding(),
    ))
    .await.unwrap();

    // Assert both endpoints earned a full row set, or a sweep that silently
    // dropped one of them passes everything below.
    assert_eq!(failed_endpoints, 0);
    for ep in &eps {
        assert_eq!(
            rows.iter().filter(|r| &r.endpoint == ep).count(),
            run.len(),
            "{ep} must have one row per metric"
        );
    }

    let log = arrivals(&log);
    assert_eq!(
        label_sequence(&log, &["a", "b"]),
        vec!["a", "a", "a", "b", "b", "b"],
        "one host is one sequential task, so its endpoints are served in blocks \
         rather than interleaved: {log:?}"
    );
    // Non-overlap AND the gap, on every consecutive pair across both
    // endpoints. Spacing alone is not enough: two requests that overlapped and
    // were released together are spaced too, and the block assertion above
    // says nothing about requests to two different paths.
    for pair in log.windows(2) {
        let apart = pair[1].at.duration_since(pair[0].at);
        assert!(
            apart >= DELAY + GAP,
            "{:?} then {:?} are {apart:?} apart: less than the {DELAY:?} the first was \
             still in flight for plus the {GAP:?} gap that must follow it",
            pair[0],
            pair[1]
        );
    }
}

#[tokio::test]
async fn concurrency_counts_hosts_not_endpoints() {
    // Four endpoints on two hosts, at concurrency 4. Two of the four permits
    // can never be used by a second endpoint of an already-running host, so at
    // most two requests are ever in flight: raising the bound past the number
    // of hosts buys nothing, which is what "--concurrency counts hosts" means.
    //
    // Read off the arrival records, where a request answered after DELAY was
    // in flight over at least [at, at + DELAY].
    const DELAY: std::time::Duration = std::time::Duration::from_millis(80);

    let one = MockServer::start().await;
    let two = MockServer::start().await;
    let log = new_log();
    mount_recording(&one, "/a", "one-a", DELAY, &log).await;
    mount_recording(&one, "/b", "one-b", DELAY, &log).await;
    mount_recording(&two, "/a", "two-a", DELAY, &log).await;
    mount_recording(&two, "/b", "two-b", DELAY, &log).await;

    let (run, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![
        format!("{}/a", one.uri()),
        format!("{}/b", one.uri()),
        format!("{}/a", two.uri()),
        format!("{}/b", two.uri()),
    ];
    let Sweep { rows, failed_endpoints, .. } = without_deadlocking(run_sweep(
        &eps,
        &run,
        &declined,
        &client,
        Budget::default(),
        NonZeroUsize::new(4).unwrap(), &Default::default(), &mut common::discarding(),
    ))
    .await.unwrap();
    assert_eq!(failed_endpoints, 0);
    assert_eq!(rows.len(), eps.len() * run.len(), "all four endpoints were measured in full");

    let log = arrivals(&log);
    assert_eq!(
        max_in_flight(&log, DELAY),
        2,
        "two hosts were in flight, and never three: {log:?}"
    );
    // The other half of the same property: within one host the two endpoints
    // are one sequential task, so their requests come in blocks. Interleaved
    // blocks here would mean two endpoints of one host held permits at once,
    // which is the arrangement whose lock waits do not fit a metric budget.
    for host in [["one-a", "one-b"], ["two-a", "two-b"]] {
        let seq = label_sequence(&log, &host);
        assert_eq!(
            seq,
            vec![host[0], host[0], host[0], host[1], host[1], host[1]],
            "{host:?} interleaved rather than running as one sequential group: {log:?}"
        );
    }
}

/// A definition that panics the task probing an endpoint: `SelectIris` reads a
/// variable out of the bindings and `lib.rs` says so with
/// `expect(VAR_REQUIRED)`, so a hand-built definition with no `var` is a
/// programming error that fails loudly rather than measuring the wrong thing.
/// `load_metrics` refuses this shape, which is why it has to be hand-built.
///
/// This is how the `join_next` error arm is reached through the public API. The
/// brief's "`JoinError` has no public constructor" is a reason to unit-test the
/// fold in isolation, which `a_failed_group_still_carries_facts_for_every_endpoint_in_it`
/// does; it is not a reason to leave the translation untested, because a
/// panicking task is reachable from here.
fn panicking_metric() -> MetricDef {
    MetricDef {
        id: "boom".into(),
        label: "reads a variable it never named".into(),
        dimension: "content".into(),
        kind: ProbeKind::SelectIris,
        query: Some("SELECT ?c WHERE { ?s a ?c } LIMIT 1".into()),
        fallback_query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        cadence: Default::default(),
        sample_limit: None,
        sample_prefix: None, tolerance: None,
    }
}

#[tokio::test]
async fn a_panicked_group_publishes_prober_failed_for_every_endpoint_it_held() {
    // One host with two endpoints, whose group panics on the second metric, and
    // one host with a single endpoint that survives. The survivor is FIRST in
    // the input, and the panicking group finishes first, so this also pins that
    // a failure does not shift the survivor's facts out of input order.
    //
    // The survivor survives by budget, not by luck: its mock answers after 3
    // seconds and its endpoint budget is 1 second, so `probe_one_endpoint`'s
    // timeout drops the future during the very first request, long before the
    // panicking definition is dispatched, and the expiry fill gives it a row
    // per metric. Nothing here depends on how fast the machine is: the delay is
    // served by the mock, so it cannot arrive early.
    //
    // Expect one panic message on stderr from the default hook. That is the
    // task dying, which is the thing under test; no panic hook is installed,
    // because a hook is process-wide and the suite runs tests in parallel.
    let solo = MockServer::start().await;
    let pair = MockServer::start().await;
    let log = new_log();
    mount_recording(&solo, "/x", "solo", std::time::Duration::from_secs(3), &log).await;
    mount_recording(&pair, "/a", "pair-a", std::time::Duration::ZERO, &log).await;
    mount_recording(&pair, "/b", "pair-b", std::time::Duration::ZERO, &log).await;

    let (mut run, declined) = probe_and_declined();
    run.truncate(1); // keep `availability`, so a request happens before the panic
    run.push(panicking_metric());
    let budget = Budget {
        request: std::time::Duration::from_secs(30),
        metric: std::time::Duration::from_secs(60),
        endpoint: std::time::Duration::from_secs(1),
    };
    let client = std::sync::Arc::new(Client::new(budget, Politeness::unlimited()).unwrap());
    let eps = vec![
        format!("{}/x", solo.uri()),
        format!("{}/a", pair.uri()),
        format!("{}/b", pair.uri()),
    ];
    // `without_deadlocking` rather than an ad-hoc timeout: the failure this
    // guards against is the sweep never returning at all, which is what a
    // panicked task poisoning the JoinSet would look like.
    let Sweep { rows, declarations_read, not_measured, content_samples, failed_endpoints } =
        without_deadlocking(run_sweep(
            &eps,
            &run,
            &declined,
            &client,
            budget,
            NonZeroUsize::new(2).unwrap(), &Default::default(), &mut common::discarding(),
        ))
        .await.unwrap();

    // Endpoints, not groups: one task panicked and it was holding two.
    assert_eq!(failed_endpoints, 2, "the count is of endpoints the sweep failed on");

    // The survivor's facts are all present, and they are the only ones.
    assert_eq!(
        rows.iter().map(|r| (r.endpoint.as_str(), r.metric_id.as_str(), r.verdict)).collect::<Vec<_>>(),
        vec![
            (eps[0].as_str(), "availability", Verdict::Indeterminate),
            (eps[0].as_str(), "boom", Verdict::Indeterminate),
        ],
        "the surviving endpoint keeps its expiry-filled rows, and the failed ones get none: \
         an Indeterminate row for them would assert a measurement that never happened"
    );
    assert_eq!(
        declarations_read.iter().map(|d| d.endpoint.as_str()).collect::<Vec<_>>(),
        vec![eps[0].as_str()],
        "a failed endpoint publishes no declarationsRead"
    );
    assert!(content_samples.is_empty());

    // The whole contract of the failure path, in input order: the survivor's
    // declined fact, then one prober-failed per metric in `defs` for each
    // endpoint the panicked task held, each still followed by its own declined
    // fact.
    assert_eq!(
        not_measured
            .iter()
            .map(|n| (n.endpoint.as_str(), n.metric_id.as_str(), n.reason))
            .collect::<Vec<_>>(),
        vec![
            (eps[0].as_str(), "classes", NotMeasuredReason::CostCeiling),
            (eps[1].as_str(), "availability", NotMeasuredReason::ProberFailed),
            (eps[1].as_str(), "boom", NotMeasuredReason::ProberFailed),
            (eps[1].as_str(), "classes", NotMeasuredReason::CostCeiling),
            (eps[2].as_str(), "availability", NotMeasuredReason::ProberFailed),
            (eps[2].as_str(), "boom", NotMeasuredReason::ProberFailed),
            (eps[2].as_str(), "classes", NotMeasuredReason::CostCeiling),
        ],
        "every endpoint of the panicked group carries one fact per metric, the declined \
         metrics keep their ceiling reason, and the survivor's facts stay in input order"
    );

    // A pair with two `NotMeasured` facts would make emit's duplicate-subject
    // guard publish NOTHING for that pair, which is worse than either fact.
    let mut pairs: Vec<(&str, &str)> =
        not_measured.iter().map(|n| (n.endpoint.as_str(), n.metric_id.as_str())).collect();
    let before = pairs.len();
    pairs.sort_unstable();
    pairs.dedup();
    assert_eq!(pairs.len(), before, "a pair got two NotMeasured facts");

    // The property the exit status and the site's freshness both hang off: no
    // endpoint the sweep covered contributed nothing at all.
    for ep in &eps {
        assert!(
            rows.iter().any(|r| &r.endpoint == ep) || not_measured.iter().any(|n| &n.endpoint == ep),
            "{ep} contributed nothing to the run graph"
        );
    }
}

// --- The incremental write: what is on disk, and when -----------------------
//
// Everything below is about the FILE rather than about the returned `Sweep`, so
// each of these builds a writer of its own instead of using the sink the other
// call sites pass.

/// The run label every test below uses. One value, because it is both the run
/// IRI's tail and the partial file's name, and a test that got them from two
/// places could assert against a file the sweep never wrote.
const RUN_AT: &str = "2026-08-20T08:00:00Z";

fn run_header(run: &RunId) -> sparqlwatch_prober::emit::RunHeader<'_> {
    sparqlwatch_prober::emit::RunHeader {
        run,
        generated_at: RUN_AT,
        metric_revision: "test-revision",
        max_cost: Cost::Cheap,
        concurrency: NonZeroUsize::new(2).unwrap(),
        // Nothing declined: every test in this file hands `run_sweep` its
        // endpoint list directly rather than through `dormancy::plan_sweep`, so
        // the dormancy section these runs carry is just its zero count. What the
        // section holds is asserted in `emit.rs`, and that a real sweep narrows
        // to `plan.probe` at all is asserted in `tests/binary.rs`, which is the
        // only file here that invokes a process.
        dormant: &[],
    }
}

/// A fresh directory under the one cargo already owns, named for the caller and
/// for this process, so neither two tests in this file nor two runs of the suite
/// collide. Same arrangement as `tests/binary.rs`, and for the same reason:
/// these tests run in parallel and each writes a run under its own name.
fn tempdir(named: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("write-{named}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The endpoints a document's chunk markers name, in the order the file states
/// them. Parsed rather than grepped, so a `sampledValue` whose IRI spells the
/// marker predicate cannot be counted as one: the loader had to learn the same
/// lesson in Task 2.
fn markers(bytes: &[u8]) -> Vec<String> {
    quads_in_order(bytes)
        .iter()
        .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:completedEndpoint")
        .map(|q| q.object.to_string().trim_matches(['<', '>']).to_string())
        .collect()
}

fn quads_in_order(bytes: &[u8]) -> Vec<Quad> {
    RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(bytes)
        .map(|q| q.expect("what the writer wrote must parse as N-Quads"))
        .collect()
}

fn has_predicate(bytes: &[u8], predicate: &str) -> bool {
    quads_in_order(bytes).iter().any(|q| q.predicate.as_str() == predicate)
}

/// One slow endpoint and one immediate one, on DIFFERENT hosts, which is what
/// lets the fast one finish while the slow one is still in flight: host grouping
/// would otherwise put both in one sequential task and the fast one would not
/// finish at all until the slow one had.
///
/// The delay is served by the mock, so it cannot arrive early on a fast machine:
/// every "while the sweep is still running" claim below rests on that and on no
/// threshold of its own.
async fn slow_and_fast(log: &Log) -> (MockServer, MockServer) {
    let slow = MockServer::start().await;
    let fast = MockServer::start().await;
    mount_recording(&slow, "/sparql", "slow", std::time::Duration::from_millis(1000), log).await;
    mount_recording(&fast, "/sparql", "fast", std::time::Duration::ZERO, log).await;
    (slow, fast)
}

/// Wait until `path` names a file with `count` chunk markers in it. Returns once
/// it does; the caller wraps this in `without_deadlocking` so a file that never
/// gets there fails by name rather than hanging the suite.
async fn until_markers(path: &std::path::Path, count: usize) {
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            if markers(&bytes).len() >= count {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// The error a sweep stopped with. Written as a helper because `Sweep` does not
/// implement `Debug` and `expect_err` needs it: a `Sweep` printed on failure
/// would be several hundred facts of no use to a reader anyway.
fn stopped_with(result: anyhow::Result<Sweep>, why: &str) -> anyhow::Error {
    match result {
        Ok(_) => panic!("{why}"),
        Err(e) => e,
    }
}

/// The mechanism, and it has to be a mechanism: reading the file after awaiting
/// `run_sweep` passes on an implementation that buffers everything and writes
/// once at the end, which is what the commit before this one did. So the sweep
/// is spawned, the file is polled until the fast endpoint's marker appears, and
/// the assertion is that the sweep had not returned at that moment. No
/// threshold: the slow host cannot answer before its mock's delay, so the sweep
/// cannot have finished.
#[tokio::test]
async fn an_endpoint_is_on_disk_before_the_sweep_returns() {
    let log = new_log();
    let (slow, fast) = slow_and_fast(&log).await;
    let (defs, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![format!("{}/sparql", slow.uri()), format!("{}/sparql", fast.uri())];
    let dir = tempdir("on-disk");
    let out = dir.join("run.nq");
    let partial = sparqlwatch_prober::write::partial_path(&out, RUN_AT);

    let sweeping = {
        let (eps, defs, declined, out) = (eps.clone(), defs.clone(), declined.clone(), out.clone());
        tokio::spawn(async move {
            let run_id = RunId(RUN_AT.into());
            let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();
            let sweep = run_sweep(
                &eps,
                &defs,
                &declined,
                &client,
                Budget::default(),
                NonZeroUsize::new(2).unwrap(),
                &Default::default(), &mut writer,
            )
            .await
            .unwrap();
            writer
                .finish(RunFooter { run: &run_id, failed_endpoints: sweep.failed_endpoints })
                .unwrap();
            sweep
        })
    };

    without_deadlocking(until_markers(&partial, 1)).await;
    assert!(
        !sweeping.is_finished(),
        "the fast endpoint's chunk reached disk only after the sweep returned, which is what \
         writing once at the end looks like"
    );
    let so_far = std::fs::read(&partial).unwrap();
    assert_eq!(markers(&so_far), vec![eps[1].clone()], "and it is the fast endpoint's chunk");
    assert!(
        !has_predicate(&so_far, "urn:sparqlwatch:finalised"),
        "a run in progress must not claim it finished"
    );
    assert!(!out.exists(), "--out is untouched until the run finishes");

    let sweep = without_deadlocking(sweeping).await.unwrap();
    assert_eq!(sweep.failed_endpoints, 0);
    let finished = std::fs::read(&out).unwrap();
    assert_eq!(markers(&finished).len(), 2, "both endpoints are in the finished run");
    assert!(has_predicate(&finished, "urn:sparqlwatch:finalised"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Covers CANCELLATION, not a crash: dropping a future runs destructors and
/// `SIGKILL` does not, so this cannot prove the buffering rule. It proves the
/// other half, that what a stopped sweep leaves behind is a document rather than
/// a fragment: parsed with `oxrdfio`, not eyeballed.
#[tokio::test]
async fn a_cancelled_sweep_leaves_a_loadable_file_of_what_it_finished() {
    let log = new_log();
    let (slow, fast) = slow_and_fast(&log).await;
    let (defs, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![format!("{}/sparql", slow.uri()), format!("{}/sparql", fast.uri())];
    let dir = tempdir("cancelled");
    let out = dir.join("run.nq");
    let partial = sparqlwatch_prober::write::partial_path(&out, RUN_AT);

    let sweeping = {
        let (eps, out) = (eps.clone(), out.clone());
        tokio::spawn(async move {
            let run_id = RunId(RUN_AT.into());
            let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();
            run_sweep(
                &eps,
                &defs,
                &declined,
                &client,
                Budget::default(),
                NonZeroUsize::new(2).unwrap(),
                &Default::default(), &mut writer,
            )
            .await
            .unwrap();
        })
    };
    without_deadlocking(until_markers(&partial, 1)).await;
    // The cancellation, at an instant the file is known to hold one finished
    // endpoint and the sweep is known to be mid-flight on the other.
    sweeping.abort();
    assert!(sweeping.await.unwrap_err().is_cancelled());

    let left = std::fs::read(&partial).unwrap();
    let quads = quads_in_order(&left);
    assert!(!quads.is_empty(), "the file parses as N-Quads on its own");
    let graphs: BTreeSet<String> = quads.iter().map(|q| q.graph_name.to_string()).collect();
    assert_eq!(
        graphs,
        BTreeSet::from([format!("<urn:sparqlwatch:run:{RUN_AT}>")]),
        "every quad names this run's graph, so a loader can replace it wholesale"
    );
    assert!(
        has_predicate(&left, "http://www.w3.org/ns/prov#generatedAtTime"),
        "the header is there, which is what every read query joins on first"
    );
    assert_eq!(markers(&left), vec![eps[1].clone()], "one endpoint finished, and it says which");
    assert!(
        !has_predicate(&left, "urn:sparqlwatch:finalised"),
        "the run did not finish and nothing in the file says it did"
    );
    assert!(!out.exists(), "a cancelled run never reaches --out");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The C5 guarantee: byte-identical, because `--out` is the source of truth for
/// the loaded store and nothing re-creates last night's file.
#[tokio::test]
async fn a_crashed_sweep_leaves_the_previous_out_file_untouched() {
    let log = new_log();
    let (slow, fast) = slow_and_fast(&log).await;
    let (defs, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps = vec![format!("{}/sparql", slow.uri()), format!("{}/sparql", fast.uri())];
    let dir = tempdir("previous");
    let out = dir.join("run.nq");
    // Last night's run, which is the only copy of it that exists.
    let previous = b"<urn:a> <urn:b> <urn:c> <urn:sparqlwatch:run:yesterday> .\n";
    std::fs::write(&out, previous).unwrap();
    let partial = sparqlwatch_prober::write::partial_path(&out, RUN_AT);

    let sweeping = {
        let out = out.clone();
        let eps = eps.clone();
        tokio::spawn(async move {
            let run_id = RunId(RUN_AT.into());
            let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();
            run_sweep(
                &eps,
                &defs,
                &declined,
                &client,
                Budget::default(),
                NonZeroUsize::new(2).unwrap(),
                &Default::default(), &mut writer,
            )
            .await
            .unwrap();
        })
    };
    without_deadlocking(until_markers(&partial, 1)).await;
    sweeping.abort();
    assert!(sweeping.await.unwrap_err().is_cancelled());

    assert_eq!(
        std::fs::read(&out).unwrap(),
        previous,
        "the previous run is byte-identical: a sweep that died must not have destroyed it"
    );
    assert_eq!(
        markers(&std::fs::read(&partial).unwrap()),
        vec![eps[1].clone()],
        "and this sweep's own partial file is loadable, holding the endpoint it did finish"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The checked invariant the per-chunk duplicate pre-scan rests on, reached
/// through a sweep: two entries naming one endpoint would put its facts either
/// side of that pre-scan, so the second chunk is refused and the sweep stops.
///
/// `registry::load_endpoints` deduplicates before `main.rs` ever gets here, so
/// this shape is not reachable from a real registry file. It is asserted anyway,
/// because the cost of being wrong is a graph that is never rewritten.
#[tokio::test]
async fn a_second_chunk_for_one_endpoint_is_refused() {
    let server = MockServer::start().await;
    let log = new_log();
    mount_recording(&server, "/sparql", "twice", std::time::Duration::ZERO, &log).await;
    let (defs, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let eps = vec![url.clone(), url.clone()];
    let dir = tempdir("twice");
    let out = dir.join("run.nq");
    let run_id = RunId(RUN_AT.into());
    let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();

    let err = stopped_with(
        without_deadlocking(run_sweep(
            &eps,
            &defs,
            &declined,
            &client,
            Budget::default(),
            NonZeroUsize::new(1).unwrap(),
            &Default::default(), &mut writer,
        ))
        .await,
        "a second chunk for one endpoint must stop the sweep",
    );
    assert!(err.to_string().contains(&url), "the refusal names the endpoint: {err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The 1c-b3 contract on the incremental path, and the ORDER it now has to hold
/// in: every real chunk, then the `prober-failed` chunks for the endpoints the
/// panicked group never delivered, then the footer. A footer written before
/// those chunks would certify a run whose failed endpoints are not in the file.
///
/// Expect one panic message on stderr from the default hook, which is the task
/// dying and the thing under test.
#[tokio::test]
async fn a_panicked_group_still_gets_its_prober_failed_chunks_written() {
    let solo = MockServer::start().await;
    let pair = MockServer::start().await;
    let log = new_log();
    mount_recording(&solo, "/x", "solo", std::time::Duration::from_secs(3), &log).await;
    mount_recording(&pair, "/a", "pair-a", std::time::Duration::ZERO, &log).await;
    mount_recording(&pair, "/b", "pair-b", std::time::Duration::ZERO, &log).await;

    let (mut defs, declined) = probe_and_declined();
    defs.truncate(1); // keep `availability`, so a request happens before the panic
    defs.push(panicking_metric());
    let budget = Budget {
        request: std::time::Duration::from_secs(30),
        metric: std::time::Duration::from_secs(60),
        endpoint: std::time::Duration::from_secs(1),
    };
    let client = std::sync::Arc::new(Client::new(budget, Politeness::unlimited()).unwrap());
    let eps = vec![
        format!("{}/x", solo.uri()),
        format!("{}/a", pair.uri()),
        format!("{}/b", pair.uri()),
    ];
    let dir = tempdir("panicked");
    let out = dir.join("run.nq");
    let run_id = RunId(RUN_AT.into());
    let mut writer = RunWriter::create(&out, RUN_AT, run_header(&run_id)).unwrap();
    let sweep = without_deadlocking(run_sweep(
        &eps,
        &defs,
        &declined,
        &client,
        budget,
        NonZeroUsize::new(2).unwrap(),
        &Default::default(), &mut writer,
    ))
    .await
    .unwrap();
    assert_eq!(sweep.failed_endpoints, 2);
    writer.finish(RunFooter { run: &run_id, failed_endpoints: sweep.failed_endpoints }).unwrap();

    let written = std::fs::read(&out).unwrap();
    // Every endpoint has a chunk, and the two the panicked group held come after
    // the survivor's.
    let marks = markers(&written);
    assert_eq!(marks.len(), 3, "one marker per endpoint: {marks:?}");
    assert_eq!(marks[0], eps[0], "the endpoint that was measured is written first");
    assert_eq!(
        BTreeSet::from([marks[1].clone(), marks[2].clone()]),
        BTreeSet::from([eps[1].clone(), eps[2].clone()]),
        "then the two the panicked group never delivered"
    );
    // "And the footer comes after all three" is NOT asserted here, because no
    // defect in `run_sweep` can make it false: `finish` consumes the writer and
    // `run_sweep` holds only a `&mut`, so a chunk written after the footer is
    // unrepresentable through this API. Task 3's review found the assertion this
    // replaces could not fail, and an assertion that cannot fail reads as
    // coverage while providing none. The structural guarantee is where it belongs,
    // in `RunWriter::finish`'s signature, and the footer's own contents are pinned
    // by `write.rs`'s rename test.
    let quads = quads_in_order(&written);
    // And the facts themselves, since a marker alone says only "reached".
    assert_eq!(
        quads
            .iter()
            .filter(|q| q.object.to_string() == "\"prober-failed\"")
            .count(),
        4,
        "two endpoints times the two metrics that would have run"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A sink that fails once it has flushed `fail_at` sections. The header is the
/// first flush and each chunk is one more, so `fail_at: 3` fails on the third
/// chunk. An unwritable path cannot reach this case: it fails in the
/// constructor, before anything is written, which is why the injected sink
/// exists at all.
struct FailsOnChunk {
    wrote: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    flushes: std::sync::Arc<std::sync::Mutex<usize>>,
    fail_at: usize,
}

impl std::io::Write for FailsOnChunk {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if *self.flushes.lock().unwrap() >= self.fail_at {
            return Err(std::io::Error::other("the disk filled up"));
        }
        self.wrote.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut flushes = self.flushes.lock().unwrap();
        if *flushes >= self.fail_at {
            return Err(std::io::Error::other("the disk filled up"));
        }
        *flushes += 1;
        Ok(())
    }
}

/// A write failure stops the sweep rather than being logged and forgotten, and
/// what was written before it is whole. Both halves matter: a swallowed error
/// would let `main.rs` rename a file missing 200 endpoints onto `--out`, over a
/// complete run from the night before.
#[tokio::test]
async fn a_chunk_write_failure_stops_the_sweep_with_what_was_written_intact() {
    let log = new_log();
    let mut servers = Vec::new();
    for name in ["a", "b", "c"] {
        let server = MockServer::start().await;
        mount_recording(&server, "/sparql", name, std::time::Duration::ZERO, &log).await;
        servers.push(server);
    }
    let (defs, declined) = probe_and_declined();
    let client = std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let eps: Vec<String> = servers.iter().map(|s| format!("{}/sparql", s.uri())).collect();

    let wrote = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = FailsOnChunk {
        wrote: std::sync::Arc::clone(&wrote),
        flushes: std::sync::Arc::new(std::sync::Mutex::new(0)),
        // The header, then two chunks, then the failure.
        fail_at: 3,
    };
    let run_id = RunId(RUN_AT.into());
    let mut writer = RunWriter::with_writer(sink, run_header(&run_id)).unwrap();
    let err = stopped_with(
        without_deadlocking(run_sweep(
            &eps,
            &defs,
            &declined,
            &client,
            Budget::default(),
            NonZeroUsize::new(3).unwrap(),
            &Default::default(), &mut writer,
        ))
        .await,
        "a chunk that cannot be written has to stop the sweep",
    );
    assert!(err.to_string().contains("disk filled up"), "the cause survives: {err}");

    let written = wrote.lock().unwrap().clone();
    let marks = markers(&written);
    assert_eq!(marks.len(), 2, "the two chunks that fitted are whole: {marks:?}");
    for mark in &marks {
        assert!(eps.contains(mark), "and each names an endpoint of this sweep");
    }
    assert!(
        !has_predicate(&written, "urn:sparqlwatch:finalised"),
        "a sweep that stopped must not have written a footer"
    );
}


/// The profile pass's enumeration fails, and the graph says so.
///
/// This is the case that had NOTHING in it. A ClassProfile metric publishes no
/// measurement row (ProbeKind::yields_measurement), so before
/// NotMeasuredReason::EnumerationFailed a pass whose enumeration did not
/// answer left no row, no sample and no profile: identical, from a reader's
/// side, to a metric nobody had declared. The endpoint here answers every
/// query with a 500, so the enumeration cannot bind a single class.
#[tokio::test]
async fn a_profile_pass_whose_enumeration_fails_says_so_rather_than_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(
        Client::new(Budget::default(), Politeness::unlimited()).unwrap(),
    );
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows: _rows, declarations_read: _read, not_measured, content_samples, failed_endpoints: _failed } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    let profile_metric = defs
        .iter()
        .find(|d| d.kind == ProbeKind::ClassProfile)
        .expect("the shipped set declares a profile metric");
    let said: Vec<&NotMeasured> = not_measured
        .iter()
        .filter(|n| n.metric_id == profile_metric.id)
        .collect();
    assert_eq!(said.len(), 1, "one fact for the one pass that failed: {not_measured:?}");
    assert_eq!(said[0].reason, NotMeasuredReason::EnumerationFailed);
    assert_eq!(said[0].endpoint, url, "the fact names the endpoint it is about");

    // And nothing was invented to fill the hole. An empty class list published
    // as a sample would read as "this endpoint has no classes", which is the
    // specific wrong answer this path exists to avoid.
    assert!(
        content_samples.iter().all(|s| s.metric_id != profile_metric.id),
        "a failed enumeration publishes no sample: {content_samples:?}"
    );
}

/// The reason is only for a pass that actually failed.
///
/// A working endpoint must not carry it, or the fact means nothing. Same
/// shipped set and the same expensive ceiling as the test above, so the only
/// difference is that the enumeration answers.
#[tokio::test]
async fn a_profile_pass_that_enumerates_publishes_no_enumeration_failure() {
    let server = an_endpoint_binding_classes(&[ZEBRA]).await;
    let defs = load_shipped_metrics();
    let client = std::sync::Arc::new(
        Client::new(Budget::default(), Politeness::unlimited()).unwrap(),
    );
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows: _rows, declarations_read: _read, not_measured, content_samples: _samples, failed_endpoints: _failed } =
        without_deadlocking(run_sweep(std::slice::from_ref(&url), &defs, &[], &client, Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding())).await.unwrap();

    assert!(
        not_measured.iter().all(|n| n.reason != NotMeasuredReason::EnumerationFailed),
        "the enumeration answered, so nothing may say it failed: {not_measured:?}"
    );
}


// ---------------------------------------------------------------------------
// The content verdict: does the endpoint describe the vocabulary it uses?
// ---------------------------------------------------------------------------

/// An endpoint that answers the class enumeration and the profile queries, and
/// serves `description` as its service description.
async fn an_endpoint_describing(description: &str, classes: &[&str]) -> MockServer {
    let bindings: Vec<String> = classes
        .iter()
        .map(|c| format!(r#"{{"c":{{"type":"uri","value":"{c}"}}}}"#))
        .collect();
    let results = format!(
        r#"{{"head":{{"vars":["c","p","subjects","datatypes","anyDatatype"]}},"results":{{"bindings":[{}]}},"boolean":true}}"#,
        bindings.join(",")
    );
    let server = MockServer::start().await;
    // The queryless fetch: the description.
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_is_missing("query"))
        // set_body_raw, not set_body_string with a content-type header:
        // set_body_string OVERWRITES the mime with text/plain (wiremock
        // response_template.rs:208), so the description arrived as plain text,
        // never classified as RDF, and the metric under test saw no declared
        // vocabulary. The header looked right and did nothing.
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            description.to_string().into_bytes(),
            "text/turtle",
        ))
        .mount(&server)
        .await;
    // Requires a `query` param rather than being a catch-all, so the two mocks
    // are disjoint and neither depends on mount order. As a catch-all it also
    // answered the QUERYLESS description fetch, with SPARQL JSON, so the
    // description never parsed as RDF and the metric under test saw no declared
    // vocabulary at all.
    // THE PROFILE QUERY ANSWERS LIKE A PROFILE, which it did not until
    // 2026-09-15. The SELECT mock below returns the class ENUMERATION body --
    // rows carrying `c` and nothing else -- and it answered the per-class
    // profile query too, because that also contains "SELECT". Those rows carry
    // no `p`, so they parse to an EMPTY profile, and the pass used to record an
    // empty profile as a success: the class counted as profiled and this test
    // passed on it. Once an empty profile is correctly read as a refusal, the
    // fixture has to answer the question it is being asked.
    //
    // Matched on "GROUP BY", which only the profile query carries, and given
    // the higher priority so it wins against the generic SELECT mock.
    let profile_rows = r#"{"head":{"vars":["p","subjects","datatypes","anyDatatype"]},"results":{"bindings":[{"p":{"type":"uri","value":"http://example.org/vocab#prop"},"subjects":{"type":"literal","value":"3"},"datatypes":{"type":"literal","value":"1"},"anyDatatype":{"type":"literal","value":"IRI"}}]}}"#;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "GROUP BY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(profile_rows))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "SELECT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(results.clone()))
        .with_priority(5)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_contains("query", "ASK"))
        .respond_with(ResponseTemplate::new(200).set_body_string(results))
        .mount(&server)
        .await;
    server
}

fn description_naming(classes: &[&str]) -> String {
    let partitions: String = classes
        .iter()
        .map(|c| format!("    void:classPartition [ void:class <{c}> ] ;\n"))
        .collect();
    format!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n\
         @prefix void: <http://rdfs.org/ns/void#> .\n\
         <http://example.org/s> a sd:Service ;\n\
         \x20   sd:defaultDataset <http://example.org/d> .\n\
         <http://example.org/d> a void:Dataset ;\n{partitions}\
         \x20   a void:Dataset .\n"
    )
}

async fn vocabulary_verdict(description: &str, classes: &[&str]) -> Verdict {
    let server = an_endpoint_describing(description, classes).await;
    let defs = load_shipped_metrics();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url),
        &defs,
        &[],
        &client,
        Budget::default(),
        NonZeroUsize::new(1).unwrap(),
        &Default::default(),
        &mut common::discarding(),
    ))
    .await
    .unwrap();
    rows.iter()
        .find(|r| r.metric_id == "vocabulary-described")
        .expect("the shipped set declares the content metric")
        .verdict
}


const C1: &str = "http://example.org/vocab#Alpha";
const C2: &str = "http://example.org/vocab#Beta";

/// Declared and found: the endpoint describes what it holds.
#[tokio::test]
async fn a_described_vocabulary_that_is_really_there_is_verified() {
    assert_eq!(
        vocabulary_verdict(&description_naming(&[C1]), &[C1]).await,
        Verdict::Verified
    );
}

/// Found and never declared. The common case in the wild, and the one this
/// metric exists to count: ontoexplorer holds 204 classes and declares none.
#[tokio::test]
async fn a_vocabulary_nobody_declared_is_undeclared_but_verified() {
    assert_eq!(
        vocabulary_verdict(&description_naming(&[]), &[C1, C2]).await,
        Verdict::UndeclaredButVerified
    );
}

/// Declared, and the pass found nothing at all. The description makes a claim
/// this run could not confirm.
#[tokio::test]
async fn a_declared_vocabulary_with_nothing_behind_it_is_declared_only() {
    assert_eq!(
        vocabulary_verdict(&description_naming(&[C1]), &[]).await,
        Verdict::DeclaredOnly
    );
}

/// Neither declared nor found. The endpoint answered and holds no typed
/// subjects, which is a real finding rather than a gap.
#[tokio::test]
async fn an_endpoint_with_no_vocabulary_at_all_is_absent() {
    assert_eq!(
        vocabulary_verdict(&description_naming(&[]), &[]).await,
        Verdict::Absent
    );
}

/// The verdict this metric must NEVER reach, asserted as a property of the
/// shipped rules rather than of one fixture.
///
/// `declared-but-wrong` would mean the description named classes and the
/// endpoint holds none of them. Nothing here can tell that apart from a pass
/// that reached only a subset: the enumeration is capped at 200 and the ladder
/// samples, so a named class missing from the profiles may never have been
/// asked about. It is the harshest verdict in the vocabulary and this evidence
/// cannot support it.
#[tokio::test]
async fn the_content_verdict_never_accuses_a_description_of_being_wrong() {
    for (declared, found) in [
        (vec![C1], vec![C2]),
        (vec![C1, C2], vec![C1]),
        (vec![C1], vec![]),
    ] {
        let v = vocabulary_verdict(&description_naming(&declared), &found).await;
        assert_ne!(
            v, Verdict::DeclaredButWrong,
            "declared {declared:?} found {found:?} must not be an accusation"
        );
    }
}

/// It sends NOTHING. The whole argument for this metric is that it costs an
/// operator no request, so a regression that made it probe would be a real
/// cost increase across the registry.
#[tokio::test]
async fn the_content_verdict_issues_no_request_of_its_own() {
    let server = an_endpoint_describing(&description_naming(&[C1]), &[C1]).await;
    let all = load_shipped_metrics();
    let without: Vec<MetricDef> = all
        .iter()
        .filter(|d| d.kind != ProbeKind::VocabularyDescribed)
        .cloned()
        .collect();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());

    without_deadlocking(run_sweep(
        std::slice::from_ref(&url), &without, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    let baseline = server.received_requests().await.unwrap().len();

    let server2 = an_endpoint_describing(&description_naming(&[C1]), &[C1]).await;
    let url2 = format!("{}/sparql", server2.uri());
    without_deadlocking(run_sweep(
        std::slice::from_ref(&url2), &all, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    let with = server2.received_requests().await.unwrap().len();

    assert_eq!(with, baseline, "the derived metric must cost no request");
}


// ---------------------------------------------------------------------------
// The counts, against what the endpoint says about itself
// ---------------------------------------------------------------------------

/// An endpoint that states `declared` triples in its description and answers
/// every COUNT with `counted`.
async fn an_endpoint_stating_and_holding(
    declared: Option<u64>,
    counted: Option<u64>,
) -> MockServer {
    let void = declared
        .map(|n| format!("    void:triples {n} ;\n"))
        .unwrap_or_default();
    let description = format!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n\
         @prefix void: <http://rdfs.org/ns/void#> .\n\
         <http://example.org/s> a sd:Service ;\n\
         \x20   sd:defaultDataset <http://example.org/d> .\n\
         <http://example.org/d> a void:Dataset ;\n{void}\
         \x20   a void:Dataset .\n"
    );
    let rows = match counted {
        Some(n) => format!(
            r#"{{"head":{{"vars":["n"]}},"results":{{"bindings":[{{"n":{{"type":"literal","value":"{n}"}}}}]}}}}"#
        ),
        // No row at all, which a real engine never returns for a COUNT and
        // which must therefore not be read as zero.
        None => r#"{"head":{"vars":["n"]},"results":{"bindings":[]}}"#.to_string(),
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .and(wiremock::matchers::query_param_is_missing("query"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            description.into_bytes(),
            "text/turtle",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            rows.into_bytes(),
            "application/sparql-results+json",
        ))
        .mount(&server)
        .await;
    server
}

async fn triple_count_verdict(declared: Option<u64>, counted: Option<u64>) -> Verdict {
    let server = an_endpoint_stating_and_holding(declared, counted).await;
    let defs = load_shipped_metrics();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url), &defs, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    rows.iter()
        .find(|r| r.metric_id == "triple-count")
        .expect("the shipped set declares triple-count")
        .verdict
}

#[tokio::test]
async fn a_count_that_matches_its_declaration_is_verified() {
    assert_eq!(triple_count_verdict(Some(1_000_000), Some(1_000_000)).await, Verdict::Verified);
}

/// The tolerance earning its place. A VoID file written before the dataset grew
/// is the ordinary case, not a fault, and calling it wrong would be crying wolf
/// on nearly every real endpoint.
#[tokio::test]
async fn a_declaration_a_few_percent_out_is_still_verified() {
    // 2% high and 4% low, both inside the shipped 5%.
    assert_eq!(triple_count_verdict(Some(1_000_000), Some(1_020_000)).await, Verdict::Verified);
    assert_eq!(triple_count_verdict(Some(1_000_000), Some(960_000)).await, Verdict::Verified);
}

/// The verdict this metric exists to be able to reach, and the most useful
/// thing a monitor can tell a consumer: the description is wrong.
#[tokio::test]
async fn a_declaration_an_order_of_magnitude_out_is_declared_but_wrong() {
    assert_eq!(
        triple_count_verdict(Some(1_000_000), Some(12_500_000)).await,
        Verdict::DeclaredButWrong
    );
    assert_eq!(triple_count_verdict(Some(9_000_000), Some(400)).await, Verdict::DeclaredButWrong);
}

/// Counted and never declared. The common case in the wild.
#[tokio::test]
async fn a_count_nobody_declared_is_undeclared_but_verified() {
    assert_eq!(triple_count_verdict(None, Some(12_500_532)).await, Verdict::UndeclaredButVerified);
}

/// Declared, and the count did not come back. The claim stands unchecked.
#[tokio::test]
async fn a_declared_count_we_could_not_verify_is_declared_only() {
    assert_eq!(triple_count_verdict(Some(1_000_000), None).await, Verdict::DeclaredOnly);
}

/// The distinction that was wrong in the first version of this rule. A COUNT
/// with no GROUP BY returns exactly one row from any real engine, so no row
/// means the number was never learned, while a row saying 0 means the endpoint
/// told us it holds nothing.
#[tokio::test]
async fn no_row_is_not_zero() {
    assert_eq!(
        triple_count_verdict(None, None).await,
        Verdict::Indeterminate,
        "no row means we never learned the count"
    );
    assert_eq!(
        triple_count_verdict(None, Some(0)).await,
        Verdict::Absent,
        "a row saying zero is the endpoint telling us it holds nothing"
    );
}

/// A declaration of zero against real content is wrong by any tolerance,
/// because a fraction of zero is zero. Asserted because the relative test would
/// otherwise divide by it.
#[tokio::test]
async fn a_declaration_of_zero_against_real_content_is_wrong() {
    assert_eq!(triple_count_verdict(Some(0), Some(4_000)).await, Verdict::DeclaredButWrong);
    assert_eq!(triple_count_verdict(Some(0), Some(0)).await, Verdict::Verified);
}

/// Every count query must union the default and named graphs.
///
/// Measured 2026-09-05 against ontoexplorer's content store: the
/// default-graph-only form answered 0 where the union form answered
/// 12,510,532. Not an undercount, a flat zero, on a real production store, and
/// it would have been published as a confident fact. Named graphs are the
/// normal arrangement in Virtuoso, GraphDB and Blazegraph.
#[test]
fn every_counting_query_looks_in_named_graphs() {
    for d in load_shipped_metrics().iter().filter(|d| d.kind == ProbeKind::Counted) {
        let q = d.query.as_deref().unwrap_or("");
        assert!(
            q.contains("GRAPH ?"),
            "{} must look in named graphs: {q}",
            d.id
        );
        assert!(
            d.tolerance.is_some(),
            "{} must state a tolerance, or an exact match calls a grown dataset wrong",
            d.id
        );
    }
}


/// The numbers reach the graph, not just the grade.
///
/// The first version of the count metrics published only the verdict, so
/// `verified` said a declaration was right and never what it said. A consumer
/// asking how big an endpoint is got a grade rather than a number, which is
/// not what a "number of triples" metric is for.
#[tokio::test]
async fn a_counting_metric_publishes_both_numbers_it_compared() {
    let server = an_endpoint_stating_and_holding(Some(1_000_000), Some(1_020_000)).await;
    let defs = load_shipped_metrics();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, declarations_read: read, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url), &defs, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    let nq = emit_nquads(RunEmission {
        run: &RunId("test".into()),
        generated_at: "2026-08-20T08:00:00Z",
        metric_revision: "test-revision",
        rows: &rows,
        declarations_read: &read,
        not_measured: &[],
        max_cost: Cost::Expensive,
        concurrency: NonZeroUsize::new(1).unwrap(),
        failed_endpoints: 0,
        content_samples: &[],
        content_profiles: &[],
    })
    .unwrap();

    assert!(nq.contains("urn:sparqlwatch:declaredCount"), "the claim must be published");
    assert!(nq.contains("urn:sparqlwatch:observedCount"), "and the count beside it");
    assert!(nq.contains("\"1000000\""), "the declared number itself: {nq}");
    assert!(nq.contains("\"1020000\""), "and the counted one");
}

/// Each number stands alone. An endpoint that declared a size we could not
/// count publishes the claim by itself, which is what `declared-only` means,
/// and publishing a zero beside it would invent a measurement.
#[tokio::test]
async fn a_declared_count_we_could_not_verify_publishes_the_claim_alone() {
    let server = an_endpoint_stating_and_holding(Some(500), None).await;
    let defs = load_shipped_metrics();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url), &defs, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    let row = rows.iter().find(|r| r.metric_id == "triple-count").unwrap();
    assert_eq!(row.declared_count, Some(500));
    assert_eq!(row.observed_count, None, "no count came back, so none is published");
}

/// Only a counting metric carries them. A number on an availability row would
/// be a fact about nothing.
#[tokio::test]
async fn no_other_metric_carries_a_count() {
    let server = an_endpoint_stating_and_holding(Some(10), Some(10)).await;
    let defs = load_shipped_metrics();
    let client =
        std::sync::Arc::new(Client::new(Budget::default(), Politeness::unlimited()).unwrap());
    let url = format!("{}/sparql", server.uri());
    let Sweep { rows, .. } = without_deadlocking(run_sweep(
        std::slice::from_ref(&url), &defs, &[], &client,
        Budget::default(), NonZeroUsize::new(1).unwrap(), &Default::default(), &mut common::discarding(),
    )).await.unwrap();
    let counting: std::collections::BTreeSet<&str> = defs
        .iter()
        .filter(|d| d.kind == ProbeKind::Counted)
        .map(|d| d.id.as_str())
        .collect();
    for r in &rows {
        if !counting.contains(r.metric_id.as_str()) {
            assert_eq!(r.declared_count, None, "{} carries a count", r.metric_id);
            assert_eq!(r.observed_count, None, "{} carries a count", r.metric_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Cadence: the same metrics file, asked at two rhythms
// ---------------------------------------------------------------------------

/// The hourly sweep runs the four hourly metrics and DECLINES the rest by
/// cadence -- it does not omit them.
///
/// That distinction is the whole reason `Cadence` exists rather than a second
/// metrics file. A metric absent from the definitions produces no fact at all,
/// and `endpoint_measurements` reports ONE sweep's facts, so an hourly run
/// carrying three metrics made the other seven VANISH from the endpoint page
/// instead of going stale. Measured on 2026-09-18 before this landed: a 03:00
/// full sweep then a 04:00 three-metric one left seven of ten columns as gaps.
#[tokio::test]
async fn an_hourly_sweep_declines_the_daily_metrics_rather_than_omitting_them() {
    let server = an_endpoint_that_answers_everything().await;
    let defs = load_shipped_metrics();
    let (affordable, too_costly) = within_cost(&defs, Cost::Cheap);
    let (run, out_of_cadence) = within_cadence(&affordable, Cadence::Hourly);

    let hourly: Vec<&str> = run.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        hourly,
        vec!["availability", "cors", "cors-preflight", "service-description"],
        "the hourly set is the four the owner chose on 2026-09-18"
    );
    assert!(
        out_of_cadence.iter().any(|d| d.id == "geo-data"),
        "geo-data is cheap and daily, so only cadence can decline it"
    );

    let declined: Vec<(MetricDef, NotMeasuredReason)> = too_costly
        .into_iter()
        .map(|d| (d, NotMeasuredReason::CostCeiling))
        .chain(out_of_cadence.into_iter().map(|d| (d, NotMeasuredReason::Cadence)))
        .collect();
    let out = sweep_against(&server, &run, &declined).await;

    // Every metric in the file is accounted for: measured, or declined with a
    // reason. Nothing is silently missing.
    let measured: Vec<&str> = out.rows.iter().map(|r| r.metric_id.as_str()).collect();
    for def in &defs {
        let seen = measured.contains(&def.id.as_str())
            || out.not_measured.iter().any(|n| n.metric_id == def.id);
        assert!(seen, "{} is neither measured nor declined", def.id);
    }
}

/// The two decline reasons stay apart, and which one a metric gets is decided
/// by cost FIRST.
///
/// `triple-count` is both too expensive for a cheap ceiling and daily. It must
/// read `cost-ceiling`: that decision would still apply on a daily sweep, and
/// telling an operator "we ask this once a day" about a query we would decline
/// anyway is the wrong account. `emit`'s duplicate-subject guard also refuses
/// two NotMeasured facts for one pair, so exactly one reason exists to give.
#[tokio::test]
async fn cost_and_cadence_are_different_reasons_and_cost_is_decided_first() {
    let server = an_endpoint_that_answers_everything().await;
    let defs = load_shipped_metrics();
    let (affordable, too_costly) = within_cost(&defs, Cost::Cheap);
    let (run, out_of_cadence) = within_cadence(&affordable, Cadence::Hourly);
    let declined: Vec<(MetricDef, NotMeasuredReason)> = too_costly
        .into_iter()
        .map(|d| (d, NotMeasuredReason::CostCeiling))
        .chain(out_of_cadence.into_iter().map(|d| (d, NotMeasuredReason::Cadence)))
        .collect();
    let out = sweep_against(&server, &run, &declined).await;

    let reason = |id: &str| {
        out.not_measured
            .iter()
            .find(|n| n.metric_id == id)
            .map(|n| n.reason.slug())
    };
    assert_eq!(reason("triple-count"), Some("cost-ceiling"), "expensive AND daily -> cost");
    assert_eq!(reason("geo-data"), Some("cadence"), "cheap but daily -> cadence");
    assert_eq!(reason("availability"), None, "an hourly metric is measured, not declined");

    // One fact per (endpoint, metric), never two.
    let mut ids: Vec<&str> = out.not_measured.iter().map(|n| n.metric_id.as_str()).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "a metric was declined twice");
}

/// A daily sweep carries EVERY metric, hourly ones included.
///
/// `Cadence::Daily` is a superset rather than the other half of a partition: a
/// daily run that skipped the hourly metrics would leave its own graph with
/// holes exactly where the hourly sweeps have verdicts, and the endpoint page
/// reports one run's facts. A sweep run with no `--cadence` flag at all is
/// this one, which is why the default is `daily`.
#[tokio::test]
async fn a_daily_sweep_carries_the_hourly_metrics_too() {
    let defs = load_shipped_metrics();
    let (run, declined) = within_cadence(&defs, Cadence::Daily);
    assert!(declined.is_empty(), "a daily sweep declines nothing for cadence");
    assert_eq!(run.len(), defs.len());
    assert_eq!(Cadence::default(), Cadence::Daily, "the safe default is the quiet one");
}
