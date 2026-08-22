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

// The timeout every probe below runs under is shared with the other test files
// that drive a `Client`, because a reentrancy mistake hangs whichever of them
// runs first and `client` sorts before `politeness`. See `tests/common`.
mod common;
use common::without_deadlocking;

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

/// A host that told us to come back later is left alone by EVERY request, not
/// just by the retry of the one it answered. The gap is zero here, so the only
/// thing that can produce a delay is the stand-down.
#[tokio::test]
async fn a_host_that_asked_us_to_come_back_later_is_left_alone_until_then() {
    let p = Politeness::new(Duration::ZERO);
    // The clock starts BEFORE the stand-down is recorded, because the instant
    // it records is `now + 300ms` measured a hair later: timed from after the
    // call, an acquire that waits exactly right can read a few hundred
    // microseconds under the floor.
    let t0 = Instant::now();
    p.stand_down("http://example.org/a", Duration::from_millis(300));
    drop(p.acquire("http://example.org/a").await);
    assert!(t0.elapsed() >= Duration::from_millis(300),
            "the host was deferred, not merely spaced, took {:?}", t0.elapsed());
}

/// The stand-down is a property of the HOST, so it defers a different question
/// to the same server. This is the whole point: it is the next metric's probe,
/// not our own retry, that used to ignore the instruction.
#[tokio::test]
async fn a_stand_down_defers_every_url_on_that_host() {
    let p = Politeness::new(Duration::ZERO);
    let t0 = Instant::now();
    p.stand_down("http://example.org/sparql?query=one", Duration::from_millis(300));
    drop(p.acquire("http://example.org/other").await);
    assert!(t0.elapsed() >= Duration::from_millis(300),
            "a second question to a deferred host waits too, took {:?}", t0.elapsed());
}

#[tokio::test]
async fn a_stand_down_on_one_host_does_not_defer_another() {
    let p = Politeness::new(Duration::ZERO);
    p.stand_down("http://a.example.org/x", Duration::from_secs(30));
    let t0 = Instant::now();
    drop(p.acquire("http://b.example.org/x").await);
    assert!(t0.elapsed() < Duration::from_secs(1),
            "one server asking to be left alone says nothing about another");
}

/// The gate does not second-guess how long the server asked for. An hour is an
/// hour: the metric and endpoint budgets are what cancel the resulting waits,
/// and the metrics then read `indeterminate`, which is exactly true because we
/// never got to ask. A cap here would be a second place deciding how long we
/// wait.
///
/// The assertion is that the acquire is still waiting after a moment, which is
/// one-sided in the same sense as every floor above: 200ms is nowhere near the
/// hour under test, so only a gate that bounded the wait itself can reach it.
#[tokio::test]
async fn an_hour_long_stand_down_is_not_shortened_by_the_gate() {
    let p = Politeness::new(Duration::ZERO);
    p.stand_down("http://example.org/a", Duration::from_secs(3600));
    let waited = tokio::time::timeout(Duration::from_millis(200), p.acquire("http://example.org/a")).await;
    assert!(waited.is_err(),
            "a host that asked for an hour must still be waiting; the budgets cancel this, not the gate");
}

/// A later, shorter instruction does not bring a host back early. A server that
/// said "five minutes" has not withdrawn that by answering something else, and
/// taking the shorter of the two would let one stale response undo a
/// stand-down.
#[tokio::test]
async fn a_shorter_stand_down_does_not_undo_a_longer_one() {
    let p = Politeness::new(Duration::ZERO);
    p.stand_down("http://example.org/a", Duration::from_secs(3600));
    p.stand_down("http://example.org/a", Duration::from_millis(1));
    let waited = tokio::time::timeout(Duration::from_millis(200), p.acquire("http://example.org/a")).await;
    assert!(waited.is_err(), "the longer instruction stands");
}

/// A stand-down recorded while we were already queued for the host binds us
/// too. It has to be read after the host is taken, not before: the request we
/// most need to hold back is the one that was waiting its turn when the server
/// said to go away, and it read the state before that was true.
#[tokio::test]
async fn a_stand_down_recorded_while_we_queued_is_still_honoured() {
    let p = std::sync::Arc::new(Politeness::new(Duration::ZERO));
    let held = p.acquire("http://example.org/a").await;
    let waiter = tokio::spawn({
        let p = p.clone();
        async move {
            let t0 = Instant::now();
            let _g = p.acquire("http://example.org/a").await;
            t0.elapsed()
        }
    });
    // Long enough for the spawned task to be queued on the host we hold.
    tokio::time::sleep(Duration::from_millis(100)).await;
    p.stand_down("http://example.org/a", Duration::from_millis(400));
    drop(held);
    let elapsed = waiter.await.expect("the queued acquire should finish");
    assert!(elapsed >= Duration::from_millis(400),
            "a stand-down recorded while we queued still binds us, took {elapsed:?}");
}

// ---------------------------------------------------------------------------
// The gate wired into `Client`: every outbound request passes through it,
// redirect hops included, and a `Retry-After` we can afford is waited out.
//
// EVERY test below runs its probes under `without_deadlocking`, and that is
// not belt and braces. The per-host lock is not reentrant, so a gate acquired
// in a helper that runs inside a hop's guard (`get_with_body`,
// `preflight_once`, `fetch_rdf_once`) is taken by a task that already holds
// it: the task waits for itself and the test HANGS rather than failing.
// Without a timeout `cargo test` sits there until somebody kills it, which is
// the least legible failure a suite can produce, and it is the failure the one
// mistake this slice most invites actually produces. The helper is shared with
// every other test file that drives a `Client`, for the reason recorded in
// `tests/common`.
// ---------------------------------------------------------------------------

use sparqlwatch_prober::budget::Budget;
use sparqlwatch_prober::client::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// A preflight that resolves a redirect chain gates EVERY hop: two requests,
/// two acquisitions, one gap between them. It used to acquire once for the
/// whole chain, which made the README's unconditional promise false for every
/// hop after the first.
///
/// A floor on the elapsed time is what pins it, and the floor can only be
/// reached if the second hop waited: the mocks are local, so both requests
/// together cost milliseconds. Under an implementation that acquires the gate
/// per hop while already holding it, this deadlocks instead of failing, so it
/// runs under an explicit timeout to keep the failure legible.
#[tokio::test]
async fn a_preflight_resolving_a_redirect_chain_gates_every_hop() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS")).and(path("/a"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "/b"))
        .mount(&server).await;
    Mock::given(method("OPTIONS")).and(path("/b"))
        .respond_with(ResponseTemplate::new(204)
            .insert_header("access-control-allow-origin", "*"))
        .mount(&server).await;

    let gap = Duration::from_millis(400);
    let c = client(gap, CAP);
    let url = format!("{}/a", server.uri());
    let t0 = Instant::now();
    let o = without_deadlocking(c.preflight(&url)).await;
    assert_eq!(o.status, Some(204), "the chain resolved to the granting hop");
    assert_eq!(server.received_requests().await.unwrap().len(), 2,
               "one OPTIONS per hop, and both of them ours to gate");
    assert!(t0.elapsed() >= gap,
            "the second hop waited its own gap, took {:?}", t0.elapsed());
}

/// The failure the reviewer measured, as a test. A probe whose first response
/// is a `301` to the same host makes TWO requests to that host, and the second
/// one is gated and spaced like any other. `reqwest` used to follow that
/// redirect inside one `send()`, so the host saw two requests and the gate saw
/// one.
///
/// A floor, never an upper bound: the two mock responses are local and cost
/// milliseconds, so only a real gap can reach it.
#[tokio::test]
async fn a_probe_redirected_to_the_same_host_gates_both_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/a"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/b"))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/b"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "application/sparql-results+json")
            .set_body_string(SPARQL_JSON))
        .mount(&server).await;

    let gap = Duration::from_millis(400);
    let c = client(gap, CAP);
    let t0 = Instant::now();
    let o = without_deadlocking(c.ask(&format!("{}/a", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(200), "the redirect was followed to its answer");
    assert_eq!(o.boolean, Some(true), "and the answer at the end of it is what we report");
    assert_eq!(server.received_requests().await.unwrap().len(), 2,
               "the host saw two requests, which is what the gate must also see");
    assert!(t0.elapsed() >= gap,
            "the redirected hop waited a gap of its own, took {:?}", t0.elapsed());
}

/// A `301` to a DIFFERENT host gates on the host we are about to touch, not on
/// the one that sent us there. Two assertions, because they fail under
/// different mistakes:
///
/// 1. The walk into B waits out B's own gap, which is due because we probed B
///    a moment ago. An implementation that follows the hop ungated (reqwest's,
///    or one acquisition for the whole chain) returns in microseconds: that is
///    exactly the 508µs the reviewer measured against a 3s gap.
/// 2. A later probe of B waits, which can only happen if the hop stamped B's
///    release. An implementation that reacquired the ORIGINAL host per hop
///    would pass the first assertion and fail this one.
#[tokio::test]
async fn a_probe_redirected_to_another_host_gates_the_new_host() {
    let a = MockServer::start().await;
    let b = MockServer::start().await;
    Mock::given(method("GET")).and(path("/b"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "application/sparql-results+json")
            .set_body_string(SPARQL_JSON))
        .mount(&b).await;
    Mock::given(method("GET")).and(path("/a"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", format!("{}/b", b.uri())))
        .mount(&a).await;

    let gap = Duration::from_millis(400);
    let c = client(gap, CAP);
    let a_url = format!("{}/a", a.uri());
    let b_url = format!("{}/b", b.uri());

    // Touch B first, so B's gate is the one that owes a pause.
    without_deadlocking(c.ask(&b_url, "ASK{}")).await;

    // The floor is a little under the gap on purpose: the gap is measured from
    // B's release, which happened a hair before this clock started.
    let floor = gap * 3 / 4;
    let t0 = Instant::now();
    let o = without_deadlocking(c.ask(&a_url, "ASK{}")).await;
    assert_eq!(o.status, Some(200), "the cross-host redirect was followed to its answer");
    assert!(t0.elapsed() >= floor,
            "the hop into B waited for B's gap, took {:?}", t0.elapsed());

    let t1 = Instant::now();
    without_deadlocking(c.ask(&b_url, "ASK{}")).await;
    assert!(t1.elapsed() >= floor,
            "the hop into B stamped B's release, so this waited too, took {:?}", t1.elapsed());
    assert_eq!(a.received_requests().await.unwrap().len(), 1, "one request to the front-end");
    assert_eq!(b.received_requests().await.unwrap().len(), 3,
               "and three to the host it redirects to, every one of them gated");
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

/// The validation exists in `main.rs` and is unit-tested there, but a unit
/// test cannot see whether `main` actually calls it: deleting the call would
/// leave every one of those tests green. So this runs the real binary, which is
/// the only thing that can tell.
///
/// It also pins the ORDER. The refusal has to come before any probing, and in
/// fact before the endpoint and metric files are even read: an operator who
/// mistyped a gap should be told so immediately, not after a sweep of
/// strangers' servers has published a run full of `indeterminate`.
#[test]
fn the_binary_refuses_a_gap_that_cannot_fit_before_it_probes_anything() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"))
        .args([
            "--at", "2026-01-01T00:00:00Z",
            "--min-gap-ms", "70000",
            // Files that do not exist, so a run that got as far as reading them
            // would fail for the wrong reason and this test would notice.
            "--endpoints", "no-such-endpoints.toml",
            "--metrics", "no-such-metrics.toml",
            "--out", "/dev/null",
        ])
        .output()
        .expect("the prober binary should be runnable");
    assert!(!out.status.success(), "a gap that cannot fit is a configuration error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--min-gap-ms"),
            "the operator is told which flag is wrong, got: {stderr}");
    assert!(stderr.contains("metric budget"),
            "and which relationship it breaks, got: {stderr}");
    assert!(!stderr.contains("no-such-endpoints.toml"),
            "the gap is refused before the endpoint list is even read, got: {stderr}");
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

/// Fix 2's missing test: a `Retry-After` we said we would honour has to cost us
/// the wait. Every other retry test here asserts the retry HAPPENED, which a
/// client that retried instantly would also satisfy, and instantly is precisely
/// the impoliteness the whole slice exists to remove.
///
/// A floor, and the gap is zero, so the only thing that can reach it is the
/// honoured delay itself.
#[tokio::test]
async fn an_honoured_retry_after_is_really_waited_out_before_the_retry() {
    let server = an_endpoint_that_throttles(1, 429, "1").await;
    let c = client(Duration::ZERO, CAP);
    let t0 = Instant::now();
    let o = without_deadlocking(c.ask(&format!("{}/sparql", server.uri()), "ASK{}")).await;
    assert_eq!(o.status, Some(200), "the retry reached the answer");
    assert!(t0.elapsed() >= Duration::from_millis(900),
            "the second-long delay was waited out, not skipped, took {:?}", t0.elapsed());
    assert_eq!(server.received_requests().await.unwrap().len(), 2, "one attempt, one retry");
}

/// The finding this fix closes: a throttle used to bind our own retry and
/// nothing else, so the next metric's probe knocked on the same host after the
/// ordinary gap. Both requests here are throttled, so the second stand-down
/// outlives the first probe, and the gap is zero, so nothing but the
/// stand-down can delay the probe that follows.
#[tokio::test]
async fn a_throttled_host_defers_the_next_probe_and_not_just_our_retry() {
    let server = an_endpoint_that_throttles(2, 429, "1").await;
    let c = client(Duration::ZERO, CAP);
    let url = format!("{}/sparql", server.uri());
    let first = without_deadlocking(c.ask(&url, "ASK{}")).await;
    assert_eq!(first.status, Some(429), "both attempts were throttled");

    let t0 = Instant::now();
    let second = without_deadlocking(c.cors(&url, "ASK{}")).await;
    assert_eq!(second.status, Some(200), "and the next metric eventually got its answer");
    assert!(t0.elapsed() >= Duration::from_millis(750),
            "the next metric waited out the throttle instead of knocking after the gap, took {:?}",
            t0.elapsed());
}

/// A delay beyond the cap still defers the host. We decline to wait for our own
/// retry, because that wait would burn the metric budget for one question, but
/// declining to wait is not permission to ask something else: the server told
/// us to go away either way.
#[tokio::test]
async fn a_beyond_cap_retry_after_still_defers_the_host() {
    let server = an_endpoint_that_throttles(1, 429, "1").await;
    // A zero cap refuses to wait out any delay at all, which is what makes the
    // one second here beyond the cap without costing the suite a real wait.
    let c = client(Duration::ZERO, Duration::ZERO);
    let url = format!("{}/sparql", server.uri());
    let t0 = Instant::now();
    let first = without_deadlocking(c.ask(&url, "ASK{}")).await;
    assert_eq!(first.status, Some(429), "the throttle is reported as observed");
    assert!(t0.elapsed() < Duration::from_secs(5), "and no wait was taken for our own retry");
    assert_eq!(server.received_requests().await.unwrap().len(), 1, "beyond the cap means no retry");

    let t1 = Instant::now();
    without_deadlocking(c.cors(&url, "ASK{}")).await;
    assert!(t1.elapsed() >= Duration::from_millis(750),
            "the host was still deferred for the next probe, took {:?}", t1.elapsed());
}
