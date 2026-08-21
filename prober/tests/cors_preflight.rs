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
use sparqlwatch_prober::metrics::{MetricDef, ProbeKind};
use sparqlwatch_prober::observe::{BodyKind, Observation};
use sparqlwatch_prober::resolve::{resolve, Declared};
use sparqlwatch_prober::verdict::Verdict;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    }
}

/// Probe `url` with the preflight and resolve the result, returning both so a
/// test can assert the verdict and the evidence it came from.
async fn preflight_and_resolve(url: &str) -> (Verdict, Observation) {
    let c = Client::new(Budget::default()).unwrap();
    let o = c.preflight(url).await;
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
    // `UndeclaredButVerified`, not `Verified`: no term in the
    // service-description vocabulary can declare CORS, and `verified` means
    // confirmed AND declared. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::UndeclaredButVerified);
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
    // `UndeclaredButVerified`, not `Verified`: no term in the
    // service-description vocabulary can declare CORS, and `verified` means
    // confirmed AND declared. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::UndeclaredButVerified);
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
    // `UndeclaredButVerified`, not `Verified`: no term in the
    // service-description vocabulary can declare CORS, and `verified` means
    // confirmed AND declared. The `cors` metric settles the same way.
    assert_eq!(v, Verdict::UndeclaredButVerified);
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

#[tokio::test]
async fn a_redirected_preflight_is_absent_even_when_the_target_would_grant() {
    // A browser fails a redirected preflight, so the verdict is `Absent`. And
    // the client must not follow it: a 303 rewrites the OPTIONS into a GET, so
    // an endpoint that refuses OPTIONS but sets ACAO on a simple GET would
    // otherwise publish `Verified` -- precisely the endpoint this metric
    // exists to catch.
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .and(path("/sparql"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "/granted"))
        .mount(&server)
        .await;
    // Answers anything, any method, with a full grant.
    Mock::given(path("/granted"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("access-control-allow-origin", "*")
                .insert_header("access-control-allow-methods", "GET, POST, OPTIONS"),
        )
        .mount(&server)
        .await;

    let (v, o) = preflight_and_resolve(&format!("{}/sparql", server.uri())).await;
    assert_eq!(v, Verdict::Absent);
    assert_eq!(o.status, Some(303), "the redirect itself is the observation, not what it points at");
    assert_eq!(o.allow_origin, None, "the grant at /granted must not have been read");

    let seen = server.received_requests().await.unwrap();
    assert!(
        seen.iter().all(|r| r.url.path() != "/granted"),
        "the preflight client followed the redirect: {:?}",
        seen.iter().map(|r| r.url.path().to_string()).collect::<Vec<_>>()
    );
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
    assert_eq!(v, Verdict::UndeclaredButVerified, "the mock only answers a real preflight");

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
