//! Scaffolding shared by every test file that drives a gated probe.
//!
//! It exists for one reason. The per-host gate is not reentrant, so a probe
//! that acquires it while already holding it waits for itself: the test HANGS
//! rather than failing. That is the least legible failure a suite can produce,
//! and it is precisely the mistake this area of the crate invites, since the
//! gate is taken per outbound request inside a chain walk that several layers
//! of helper sit below.
//!
//! A review of stage 1c-b2 measured what that costs: with one stray `acquire`
//! added to `get_with_body`, `cargo test` sat in `tests/client.rs` for 4
//! minutes 47 seconds having printed nothing, and was killed rather than
//! finishing. `tests/politeness.rs` alone had timeouts, and it runs
//! alphabetically after `client`, so the legible failures it produced were
//! never reached. Hence a helper here, used by every file that touches a
//! `Client`, rather than in one of them.
#![allow(dead_code)]

use sparqlwatch_prober::emit::{RunHeader, RunId};
use sparqlwatch_prober::metrics::Cost;
use sparqlwatch_prober::write::RunWriter;
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

/// Many times longer than anything a local `wiremock` server needs, so this
/// bound is one-sided in the same sense as every timing assertion in
/// `tests/politeness.rs`: it can only be reached by a probe that is never
/// coming back, never by a slow one.
pub const NO_DEADLOCK: Duration = Duration::from_secs(20);

/// Run a gated probe under `NO_DEADLOCK`, so a reentrancy mistake surfaces as
/// a named failure in a bounded time instead of a hung suite.
pub async fn without_deadlocking<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(NO_DEADLOCK, f).await.expect(
        "timed out: a probe that acquires the per-host gate it already holds deadlocks here",
    )
}

/// A run writer whose bytes go nowhere, for the tests that are about what a
/// sweep MEASURES rather than about what reaches disk.
///
/// `run_sweep` writes each endpoint's chunk as it finishes, so it needs a
/// writer whatever the test is asking about. A sink keeps the 38 tests that
/// assert on the returned `Sweep` saying only that, and the handful of tests
/// that are about the file build a writer of their own over a real path or an
/// injected failing sink.
pub fn discarding() -> RunWriter<io::Sink> {
    RunWriter::with_writer(
        io::sink(),
        RunHeader {
            run: &RunId("test".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: "test-revision",
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
        },
    )
    .expect("a header written to a sink cannot fail")
}
