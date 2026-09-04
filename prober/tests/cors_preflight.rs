//! The CORS preflight probe: what a browser actually does before a
//! cross-origin SPARQL query.
//!
//! The `cors` metric observes an `access-control-allow-origin` header on a
//! simple GET, which is what a `curl` user sees. A real cross-origin query is
//! preflighted with `OPTIONS`, so an endpoint that sets the header on GET and
//! refuses `OPTIONS` passes that metric and still fails in a browser. These
//! tests pin the second fact, and in particular pin that the status gate is
//! consulted BEFORE any header: a `405` carrying `access-control-allow-origin:
//! *` is an absence, not a grant.

use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::{Client, ORIGIN};
use sparqlwatch_prober::politeness::Politeness;
use sparqlwatch_prober::metrics::{Cost, MetricDef, ProbeKind};
use sparqlwatch_prober::observe::{BodyKind, Observation};
use sparqlwatch_prober::resolve::{resolve, Declared};
use sparqlwatch_prober::verdict::Verdict;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
mod common;
use common::without_deadlocking;

/// The shipped `cors-preflight` metric's shape. No `declared_by`: no
/// declaration in any service-description vocabulary speaks for a CORS
/// policy, so `Declared::claimed` is always false for this kind.
fn preflight_def() -> MetricDef {
    MetricDef {
        id: "cors-preflight".into(),
        label: "Answers a CORS preflight for a cross-origin GET".into(),
        dimension: "interoperability".into(),
        kind: ProbeKind::CorsPreflight,
        query: None,
        expect: None,
        var: None,
        declared_by: None,
        graded: false,
        cost: Cost::Cheap,
        sample_limit: None,
        sample_prefix: None,
    }
}

/// Probe `url` with the preflight and resolve the result, returning both so a
/// test can assert the verdict and the evidence it came from.
async fn preflight_and_resolve(url: &str) -> (Verdict, Observation) {
    let c = Client::new(Budget::default(), Politeness::unlimited()).unwrap();
    let o = without_deadlocking(c.preflight(url)).await;
    let v = resolve(&preflight_def(), Declared { claimed: false }, Ok(&o));
    (v, o)
}

/// Mount one response for `OPTIONS /sparql` and return the server.
async fn preflight_answering(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS")).and(path("/sparql")).respond_with(template).mount(&server).await;
    server
}

#[tokio::test]
async fn a_wildcard_grant_listing_get_is_confirmed() {
    let server = preflight_answering(
        ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "GET, POST, OPTIONS")
            .insert_header("access-control-allow-headers", "content-type"),
    )
    .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    // `Verified`: the probe confirmed it, and the declared/observed axis
    // applies only where a declaration is possible. No term in the
    // service-description vocabulary can declare CORS, so `cors-preflight`
    // carries no `declared_by`. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::Verified);
    assert_eq!(o.status, Some(204));
    // The header's VALUE is recorded, not merely its presence: presence is not
    // a grant, and the resolver has to be able to tell the two apart.
    assert_eq!(o.allow_origin.as_deref(), Some("*"));
    assert_eq!(o.allow_methods.as_deref(), Some("GET, POST, OPTIONS"));
    assert_eq!(o.allow_headers.as_deref(), Some("content-type"));
    // No body is read or classified on a preflight.
    assert_eq!(o.body_kind, BodyKind::None);
    assert!(o.body.is_none());
}

#[tokio::test]
async fn a_grant_with_no_methods_header_is_confirmed() {
    // `access-control-allow-methods` is optional for a simple method: a
    // preflight that answered without it refused nothing.
    let server =
        preflight_answering(ResponseTemplate::new(200).insert_header("access-control-allow-origin", "*")).await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    // `Verified`: the probe confirmed it, and the declared/observed axis
    // applies only where a declaration is possible. No term in the
    // service-description vocabulary can declare CORS, so `cors-preflight`
    // carries no `declared_by`. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::Verified);
    assert_eq!(o.allow_methods, None);
}

#[tokio::test]
async fn an_exact_echo_of_our_own_origin_is_a_grant_to_us() {
    let server = preflight_answering(
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", ORIGIN)
            .insert_header("access-control-allow-methods", "GET"),
    )
    .await;

    let (v, _) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    // `Verified`: the probe confirmed it, and the declared/observed axis
    // applies only where a declaration is possible. No term in the
    // service-description vocabulary can declare CORS, so `cors-preflight`
    // carries no `declared_by`. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::Verified);
}

#[tokio::test]
async fn a_405_with_no_cors_headers_is_absent() {
    // Fetch requires the preflight to answer with an ok status, so a refused
    // OPTIONS is the endpoint answering the question we asked: no.
    let server = preflight_answering(ResponseTemplate::new(405)).await;
    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
    assert_eq!(o.status, Some(405));
}

#[tokio::test]
async fn a_405_carrying_an_allow_origin_header_is_still_absent() {
    // THE case a header-first implementation gets wrong, and the one revision 1
    // of the plan missed. Plenty of servers attach a blanket
    // `access-control-allow-origin: *` in a front-end filter and still refuse
    // OPTIONS at the handler. A browser's preflight fails, so this is an
    // absence, and the status has to be consulted before any header for that to
    // come out right.
    let server = preflight_answering(
        ResponseTemplate::new(405)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "GET, POST"),
    )
    .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent, "a 405 fails the preflight whatever headers ride along");
    assert_eq!(o.allow_origin.as_deref(), Some("*"), "the header was really there; the status still wins");
}

#[tokio::test]
async fn a_501_is_absent() {
    // Not implemented is the other way a server says it does not do OPTIONS.
    let server = preflight_answering(ResponseTemplate::new(501)).await;
    let (v, _) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
}

/// C1. A 3xx used to be minted as `Absent`, so an `http://` registry URL whose
/// service answers a preflight perfectly one hop away published "does not
/// answer a browser preflight" -- in the same run whose `cors` row followed
/// that same redirect and reported the capability present. The redirect is a
/// chain to resolve, not an answer: read `Location`, re-issue the `OPTIONS`
/// there, and resolve on what the end of the chain says.
#[tokio::test]
async fn a_303_to_a_granting_path_reaches_the_grant_instead_of_publishing_absent() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "/granted"))
        .mount(&server)
        .await;
    // Answers a genuine preflight only. A `303` followed by reqwest would
    // rewrite the OPTIONS into a GET, which finds no mock here, gets
    // wiremock's 404 and resolves to `Indeterminate` -- so this mock is also
    // what pins that the method is never rewritten.
    Mock::given(method("OPTIONS"))
        .and(path("/granted"))
        .and(header_exists("origin"))
        .and(header("access-control-request-method", "GET"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"),
        )
        .mount(&server)
        .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Verified, "the preflight was answered at the end of the chain");
    assert_eq!(o.status, Some(204), "the verdict is drawn from the final response, not the redirect");
    assert_eq!(o.allow_origin.as_deref(), Some("*"));

    let seen = server.received_requests().await.unwrap();
    let hops: Vec<(String, String)> =
        seen.iter().map(|r| (r.method.to_string(), r.url.path().to_string())).collect();
    assert_eq!(
        hops,
        vec![
            ("OPTIONS".to_string(), "/sparql".to_string()),
            ("OPTIONS".to_string(), "/granted".to_string()),
        ],
        "the chain must be re-issued as OPTIONS, one deliberate hop at a time"
    );
}

/// Ruling F survives the fix: we resolve the chain ourselves precisely so the
/// method is never rewritten. An endpoint that redirects and then refuses
/// `OPTIONS` while granting CORS on a simple GET is the endpoint this metric
/// exists to catch, and it must still come out `absent`.
#[tokio::test]
async fn a_redirect_to_a_service_that_refuses_options_is_absent_not_a_grant() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/real"))
        .mount(&server)
        .await;
    Mock::given(method("OPTIONS"))
        .and(path("/real"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    // The blanket header a front-end filter puts on a simple GET. Reachable
    // only by rewriting the method, which is what must not happen.
    Mock::given(method("GET"))
        .and(path("/real"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET"),
        )
        .mount(&server)
        .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent, "the service at the end of the chain refuses OPTIONS");
    assert_eq!(o.status, Some(405));
    assert_eq!(o.allow_origin, None, "the simple-GET grant must not have been read");

    let seen = server.received_requests().await.unwrap();
    assert!(
        seen.iter().all(|r| r.method == wiremock::http::Method::OPTIONS),
        "the preflight was rewritten into another method: {:?}",
        seen.iter().map(|r| r.method.to_string()).collect::<Vec<_>>()
    );
}

/// A 3xx we cannot resolve is not an absence. `absent` on this metric asserts
/// the endpoint answered the preflight and refused us; here we never reached a
/// preflight answer at all.
#[tokio::test]
async fn a_redirect_with_no_usable_location_is_indeterminate() {
    for template in [
        ResponseTemplate::new(302),                                  // no Location at all
        ResponseTemplate::new(302).insert_header("location", "   "), // blank Location
    ] {
        let server = preflight_answering(template).await;
        let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
        assert_eq!(v, Verdict::Indeterminate, "an unresolvable redirect is not an answer");
        assert_eq!(o.status, Some(302), "the unresolved redirect is what we observed");
        assert_eq!(server.received_requests().await.unwrap().len(), 1, "there was nowhere to go");
    }
}

#[tokio::test]
async fn a_redirect_loop_is_indeterminate_and_stops() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/b"))
        .mount(&server)
        .await;
    Mock::given(method("OPTIONS"))
        .and(path("/b"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/a"))
        .mount(&server)
        .await;

    let (v, o) = preflight_and_resolve(&format!("{}/a", server.uri())).await;
    assert_eq!(v, Verdict::Indeterminate);
    assert_eq!(o.status, Some(307));
    // /a, then /b, then /a again is already seen and the chain stops. The
    // point of the cycle check is that a stranger's server does not get an
    // unbounded number of our requests.
    assert_eq!(server.received_requests().await.unwrap().len(), 2, "the loop was not cut");
}

#[tokio::test]
async fn a_chain_longer_than_the_hop_bound_is_indeterminate() {
    let server = MockServer::start().await;
    // Seven redirects and then a grant: more hops than the bound allows, so
    // the grant must never be reached and the verdict must not be `absent`.
    for i in 0..7 {
        Mock::given(method("OPTIONS"))
            .and(path(format!("/h{i}")))
            .respond_with(ResponseTemplate::new(303).insert_header("location", format!("/h{}", i + 1)))
            .mount(&server)
            .await;
    }
    Mock::given(method("OPTIONS"))
        .and(path("/h7"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET"),
        )
        .mount(&server)
        .await;

    let (v, o) = preflight_and_resolve(&format!("{}/h0", server.uri())).await;
    assert_eq!(v, Verdict::Indeterminate, "we did not reach the end of the chain");
    assert_eq!(o.status, Some(303));

    let seen = server.received_requests().await.unwrap();
    let paths: Vec<String> = seen.iter().map(|r| r.url.path().to_string()).collect();
    // The first request plus five resolved hops. The bound is a bound on
    // requests we send to somebody else's server, so it is asserted exactly.
    assert_eq!(paths, vec!["/h0", "/h1", "/h2", "/h3", "/h4", "/h5"], "the hop bound was not honoured");
}

#[tokio::test]
async fn a_throttle_or_a_server_error_is_indeterminate() {
    // These describe our request or the server's state, not its CORS policy.
    for status in [429, 500, 503] {
        let server = preflight_answering(ResponseTemplate::new(status)).await;
        let (v, _) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
        assert_eq!(v, Verdict::Indeterminate, "status {status}");
    }
}

#[tokio::test]
async fn a_2xx_with_no_allow_origin_is_absent() {
    // The endpoint answered the preflight and granted nothing.
    let server = preflight_answering(ResponseTemplate::new(204)).await;
    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
    assert_eq!(o.allow_origin, None);
}

#[tokio::test]
async fn a_grant_to_somebody_else_is_absent_not_verified() {
    // Presence of `access-control-allow-origin` is not a grant. A value that
    // is neither `*` nor our own origin is a grant to somebody else, and
    // reporting it as ours would publish `verified` for an endpoint that would
    // refuse us in a browser.
    let server = preflight_answering(
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "https://example.com")
            .insert_header("access-control-allow-methods", "GET, POST"),
    )
    .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
    assert_eq!(o.allow_origin.as_deref(), Some("https://example.com"));
    assert!(o.cors, "the header is present, which is exactly why presence cannot be the test");
}

#[tokio::test]
async fn a_2xx_granting_only_post_is_absent() {
    // The server stated a method list and the GET we would send is not in it.
    let server = preflight_answering(
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            .insert_header("access-control-allow-methods", "POST"),
    )
    .await;

    let (v, _) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
}

#[tokio::test]
async fn the_request_really_is_a_preflight() {
    // Without `Access-Control-Request-Method` this is not a preflight at all,
    // and a correct server may ignore it, which would make every verdict above
    // meaningless. The mock matches only a genuine preflight, so a plain
    // OPTIONS (or a GET) finds no mock, gets wiremock's 404, and resolves to
    // Indeterminate instead of Verified.
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/sparql"))
        .and(header_exists("origin"))
        .and(header("access-control-request-method", "GET"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET"),
        )
        .mount(&server)
        .await;

    let (v, _) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Verified, "the mock only answers a real preflight");

    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, wiremock::http::Method::OPTIONS);
    assert_eq!(
        seen[0].headers.get("origin").map(|v| v.to_str().unwrap()),
        Some(ORIGIN),
        "the origin we announce has to be the one the resolver compares against"
    );
    assert_eq!(
        seen[0].headers.get("access-control-request-headers").map(|v| v.to_str().unwrap()),
        Some("content-type"),
        "a SPARQL POST/GET from the editor carries content-type, so the preflight must ask about it"
    );
}
