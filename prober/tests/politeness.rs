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
