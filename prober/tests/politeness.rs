//! The per-host gate, tested on its own: no `Client`, no HTTP, just the two
//! guarantees it exists to give. Never two requests in flight to one host, and
//! a minimum pause between consecutive requests to one host.
//!
//! Every timing assertion here is one-sided, either "at least the gap" or
//! "comfortably under a bound many times larger than anything real". An upper
//! bound close to the gap would flap on a loaded machine, and a flapping
//! politeness test gets deleted rather than fixed.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use sparqlwatch_prober::politeness::Politeness;

#[tokio::test]
async fn two_requests_to_one_host_are_spaced_by_the_minimum_gap() {
    let p = Politeness::new(Duration::from_millis(300));
    let t0 = Instant::now();
    drop(p.acquire("http://example.org/a").await);
    drop(p.acquire("http://example.org/b").await);
    assert!(t0.elapsed() >= Duration::from_millis(300),
            "the second request to one host waited for the gap");
}

#[tokio::test]
async fn different_hosts_do_not_wait_for_each_other() {
    let p = Politeness::new(Duration::from_secs(30));
    let t0 = Instant::now();
    let (_a, _b) = tokio::join!(p.acquire("http://a.example.org/x"), p.acquire("http://b.example.org/x"));
    assert!(t0.elapsed() < Duration::from_secs(1),
            "a long gap on one host must not serialise unrelated hosts");
}

#[tokio::test]
async fn one_host_never_has_two_requests_in_flight() {
    // The gap alone does not give this: two tasks could both find the gap
    // elapsed and proceed together. The guard has to exclude.
    let p = std::sync::Arc::new(Politeness::new(Duration::ZERO));
    let live = std::sync::Arc::new(AtomicUsize::new(0));
    let peak = std::sync::Arc::new(AtomicUsize::new(0));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (p, live, peak) = (p.clone(), live.clone(), peak.clone());
        set.spawn(async move {
            let _g = p.acquire("http://example.org/x").await;
            let n = live.fetch_add(1, SeqCst) + 1;
            peak.fetch_max(n, SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            live.fetch_sub(1, SeqCst);
        });
    }
    while set.join_next().await.is_some() {}
    assert_eq!(peak.load(SeqCst), 1, "requests to one host must not overlap");
}

/// The test the earlier plan could not write: it detects the map's lock being
/// held while awaiting a per-host lock. A first acquire is uncontended and so
/// cannot show it. Hold one host busy, then time an acquire on a DIFFERENT host.
#[tokio::test]
async fn a_busy_host_does_not_block_the_map() {
    let p = std::sync::Arc::new(Politeness::new(Duration::ZERO));
    let held = p.acquire("http://slow.example.org/x").await;
    let t0 = Instant::now();
    let other = p.acquire("http://fast.example.org/x").await;
    assert!(t0.elapsed() < Duration::from_millis(100),
            "acquiring a free host must not wait behind a busy one");
    drop(other);
    drop(held);
}

#[tokio::test]
async fn the_gap_is_measured_from_release_not_from_acquisition() {
    let p = Politeness::new(Duration::from_millis(200));
    let g = p.acquire("http://example.org/a").await;
    tokio::time::sleep(Duration::from_millis(200)).await; // a slow request
    drop(g);
    let t0 = Instant::now();
    drop(p.acquire("http://example.org/b").await);
    assert!(t0.elapsed() >= Duration::from_millis(150),
            "a slow request must not consume the pause that follows it");
}

// ---------------------------------------------------------------------------
// The gate wired into `Client`: every public probe passes through it exactly
// once, and a `Retry-After` we can afford is waited out.
//
// EVERY test below runs its probes under `without_deadlocking`, and that is
// not belt and braces. The per-host lock is not reentrant, so a gate acquired
// in an inner helper (`get_with_body`, `preflight_once`, `preflight_chain`,
// `fetch_rdf_once`) is taken by a task that already holds it: the task waits
// for itself and the test HANGS rather than failing. Without a timeout
// `cargo test` sits there until somebody kills it, which is the least legible
// failure a suite can produce, and it is the failure the one mistake this
// slice most invites actually produces.
// ---------------------------------------------------------------------------

use std::future::Future;

use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Run a probe under a bound many times larger than anything real, so a
/// deadlock surfaces as a named failure instead of a hung suite. One-sided,
/// like every other timing assertion in this file, so it cannot flap.
async fn without_deadlocking<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f).await.expect(
        "timed out: a probe that acquires the per-host gate it already holds deadlocks here",
    )
}

/// A cap large enough that no test below is testing the cap by accident. The
/// two tests that ARE about the cap state their own.
const CAP: Duration = Duration::from_secs(120);

/// A `SELECT`/`ASK` answer that satisfies every query probe at once: a
/// boolean for `ask`/`ask_literal` and a binding row for `select_iris`.
const SPARQL_JSON: &str = r#"{"head":{"vars":["c","g"]},"boolean":true,
  "results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/C"},
                          "g":{"type":"literal","value":"POINT(0 0)"}}]}}"#;

/// One server that answers every probe: `GET /sparql` for the four query
/// probes and the description fetch, `OPTIONS /sparql` for the preflight.
async fn an_endpoint_that_answers_everything() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "application/sparql-results+json")
            .set_body_string(SPARQL_JSON))
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*"))
        .mount(&server).await;
    server
}

/// A server that answers `GET /sparql` with `status` carrying `Retry-After:
/// value` the first `n` times, and with a 200 SPARQL answer afterwards.
async fn an_endpoint_that_throttles(n: u64, status: u16, value: &str) -> MockServer {
    let server = MockServer::start().await;
    // Lower priority number wins, and `up_to_n_times` stops this mock
    // matching once it has answered n times, so the 200 below takes over.
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(status).insert_header("retry-after", value))
        .up_to_n_times(n)
        .with_priority(1)
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/sparql"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "application/sparql-results+json")
            .set_body_string(SPARQL_JSON))
        .with_priority(2)
        .mount(&server).await;
    server
}

fn client(gap: Duration, cap: Duration) -> Client {
    Client::new(Budget::default(), Politeness::with_retry_after_cap(gap, cap)).unwrap()
}

/// The reentrancy hazard, as a test rather than a hope. A preflight that
/// resolves a redirect chain acquires ONCE. If the gate were acquired per hop
/// this deadlocks and the test times out rather than failing cleanly, so it
/// runs under an explicit timeout to make the failure legible.
#[tokio::test]
async fn a_preflight_resolving_a_redirect_chain_acquires_once() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS")).and(path("/a"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "/b"))
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/b"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*"))
        .mount(&server).await;

    // A gap large enough that a second acquisition would be visible in the
    // elapsed time even if it somehow did not deadlock.
    let c = client(Duration::from_millis(50), CAP);
    let url = format!("{}/a", server.uri());
    let o = without_deadlocking(c.preflight(&url)).await;
    assert_eq!(o.status, Some(204), "the chain resolved to the granting hop");
}

#[tokio::test]
async fn every_public_probe_goes_through_the_gate() {
    // One host, one small gap, six probes. If a probe bypassed the gate the
    // elapsed time would fall below the floor. Asserts a FLOOR, so it cannot
    // flap on a slow machine.
    let server = an_endpoint_that_answers_everything().await;
    let gap = Duration::from_millis(120);
    let c = client(gap, CAP);
    let url = format!("{}/sparql", server.uri());
    let t0 = Instant::now();
    without_deadlocking(async {
        c.ask(&url, "ASK{}").await;
        c.cors(&url, "ASK{}").await;
        c.select_iris(&url, "SELECT ?c WHERE{?s a ?c}", "c").await;
        c.ask_literal(&url, "SELECT ?g WHERE{?s ?p ?g}", "g").await;
        c.fetch_rdf(&url).await;
        c.preflight(&url).await;
    })
    .await;
    assert!(t0.elapsed() >= gap * 5,
            "six gated probes to one host wait five gaps, took {:?}", t0.elapsed());
}

#[tokio::test]
async fn a_retry_after_within_the_cap_is_waited_out_and_the_request_retried() {
    let server = an_endpoint_that_throttles(1, 429, "1").await;
    let c = client(Duration::ZERO, CAP);
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(200),
               "the retry happened and the second answer is what we report");
    assert_eq!(o.boolean, Some(true), "and the retried answer is the one parsed");
}

#[tokio::test]
async fn a_retry_after_beyond_the_cap_is_not_waited_out() {
    // Retry-After: 3600 with a 2s cap. Must return promptly, reporting the 429.
    //
    // Its own timeout rather than `without_deadlocking`, because the failure it
    // catches is not a deadlock: a client that ignored the cap would sit here
    // for an hour, and the message has to name the actual cause.
    let server = an_endpoint_that_throttles(1, 429, "3600").await;
    let c = client(Duration::ZERO, Duration::from_secs(2));
    let o = tokio::time::timeout(
        Duration::from_secs(10),
        c.ask(&format!("{}/sparql", server.uri()), "ASK{}"),
    )
    .await
    .expect("timed out: a Retry-After beyond the cap was waited out; we do not wait an hour");
    assert_eq!(o.status, Some(429), "and we report what the endpoint actually said");
}

#[tokio::test]
async fn a_503_with_retry_after_is_honoured_like_a_429() {
    // The prose covers both statuses, so both need pinning: a 503 with
    // Retry-After is a server telling us it is temporarily down, which is the
    // same instruction a 429 gives for a different reason.
    let server = an_endpoint_that_throttles(1, 503, "1").await;
    let c = client(Duration::ZERO, CAP);
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(200), "the retry happened after the 503 too");
}

#[tokio::test]
async fn an_unparseable_retry_after_is_not_guessed_at() {
    // We do not know how long to wait, so we do not wait and do not retry. The
    // 429 stands, and what it means is `resolve()`'s business, not ours.
    let server = an_endpoint_that_throttles(1, 429, "banana").await;
    let c = client(Duration::ZERO, CAP);
    let t0 = Instant::now();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(429));
    assert!(t0.elapsed() < Duration::from_secs(5), "no delay was invented");
    assert_eq!(server.received_requests().await.unwrap().len(), 1,
               "an unparseable delay is not retried either");
}

#[tokio::test]
async fn an_http_date_retry_after_is_recognised_and_still_not_waited_out() {
    // The date form is distinguished from junk by `parse_retry_after`, but this
    // crate does not parse it, so it must not guess a delay from it. Guessing
    // short is exactly the impoliteness this slice exists to prevent.
    let server = an_endpoint_that_throttles(1, 429, "Wed, 21 Oct 2026 07:28:00 GMT").await;
    let c = client(Duration::ZERO, CAP);
    let t0 = Instant::now();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(429));
    assert!(t0.elapsed() < Duration::from_secs(5), "no delay was guessed from a date");
    assert_eq!(server.received_requests().await.unwrap().len(), 1, "and no retry was made");
}

#[tokio::test]
async fn a_server_that_throttles_twice_is_not_asked_a_third_time() {
    // One retry, not a loop. A server that throttles the retry as well is
    // telling us to come back later, and the request count is the only thing
    // that pins it: a loop would eventually reach the 200 and every
    // status assertion above would still pass.
    let server = an_endpoint_that_throttles(5, 429, "1").await;
    let c = client(Duration::ZERO, CAP);
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(429), "the second throttle is what we report");
    assert_eq!(server.received_requests().await.unwrap().len(), 2,
               "one attempt and exactly one retry");
}
