//! Scaffolding shared by every test file that drives a gated probe, and by the
//! two that spawn the prober as a process.
//!
//! It exists mainly for one reason. The per-host gate is not reentrant, so a probe
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
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Many times longer than anything a local `wiremock` server needs, so within
/// one test target this bound is one-sided in the same sense as every timing
/// assertion in `tests/politeness.rs`: it is reached by a probe that is never
/// coming back and not by a slow one.
///
/// **Within one test target is the whole of the caveat**, and an earlier
/// version of this comment left it out and was wrong for it. This is a
/// WALL-CLOCK bound, and libtest runs one target's tests in parallel threads,
/// so a test that holds the machine for tens of seconds starves its neighbours
/// and their perfectly healthy probes blow the bound. `tests/binary.rs` did
/// exactly that while it still held the relegation sweep, which cannot finish
/// in under 30 s by construction: a bare `cargo test` failed 4 of that file's 6
/// tests, on this machine and more reliably on CI's smaller one, and every one
/// of those failures reported a deadlock in code that has none.
///
/// The fix was to give the slow sweep a target of its own,
/// `tests/dormancy_sweep.rs`, since a target gets its own process and its own
/// scheduling. It is a rule and not a one-off: **no target that shares this
/// bound may hold a test whose own runtime approaches it.**
pub const NO_DEADLOCK: Duration = Duration::from_secs(20);

/// Run something that can hang under `NO_DEADLOCK`, so it surfaces as a named
/// failure in a bounded time instead of a hung suite.
///
/// Two callers, two hangs. In-process it wraps a gated probe, where the hang is
/// the reentrancy mistake above. `ran_without_hanging` below wraps the wait on
/// a spawned prober, where the hang is a sweep whose arrival channel never
/// closes, which is what `run_sweep` dropping the sweep's own sender prevents.
pub async fn without_deadlocking<T>(f: impl Future<Output = T>) -> T {
    within(NO_DEADLOCK, f).await
}

/// The same, with the bound named by the caller.
///
/// It exists so the MESSAGE lives in one place. `tests/dormancy_sweep.rs` has
/// the one test that cannot fit inside `NO_DEADLOCK`: relegating an endpoint
/// needs a sweep whose measured cost exceeds `dormancy::MIN_COST_MS`, and that
/// cost is summed wall-clock time, so no mock can produce a 30 second cost in
/// less than 30 seconds. Passing the bound in keeps that one exception from
/// restating the diagnosis below, which is the thing a reader of a hung suite
/// actually needs and so is the thing that must not drift into two versions.
///
/// Every other caller goes through `without_deadlocking` and keeps
/// `NO_DEADLOCK`, under the condition that constant now states: one target, and
/// nothing deliberately slow inside it.
pub async fn within<T>(bound: Duration, f: impl Future<Output = T>) -> T {
    tokio::time::timeout(bound, f).await.expect(
        "timed out. Either something here is never coming back, or something sharing this test \
         target is starving it: a probe acquiring the per-host gate it already holds, a sweep \
         whose arrival channel never closes, or a deliberately slow test holding the machine \
         while this one's bound ran out",
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
            // Nothing declined. These 38 tests are about what a sweep MEASURES,
            // and the sweeps they drive are handed their endpoint list directly
            // rather than through `dormancy::plan_sweep`, so there is nothing
            // for a dormancy section to hold. It is still written, carrying its
            // zero count, into the sink these bytes go to.
            dormant: &[],
        },
    )
    .expect("a header written to a sink cannot fail")
}

// --- Driving the prober as a process ----------------------------------------
//
// Two targets spawn the built binary: `tests/binary.rs`, for the flags only
// `main.rs` connects to a sweep, and `tests/dormancy_sweep.rs`, for the one
// relegation sweep that is deliberately slower than `NO_DEADLOCK`. The spawn
// helper lives here rather than in either of them because the kill-on-drop and
// the poll loop are the subtle parts, and a second copy of them is how one
// target keeps a hung prober alive after the suite has gone.

/// A spawned child that is killed when it goes out of scope.
///
/// `std::process::Child::drop` deliberately does not kill, so without this a
/// test that panics or times out while the prober is hung leaves the process
/// running after the suite has gone.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run `command` to completion under `NO_DEADLOCK`, or fail by name.
///
/// `try_wait` in a poll loop rather than `output()`, because `output()` blocks
/// the thread and no timeout can interrupt it. Only stderr is piped, and it is
/// read after the child has exited: the assertions here quote stderr, and a
/// piped stdout would be one more pipe to keep from filling.
pub async fn ran_without_hanging(command: &mut Command) -> Output {
    ran_within(command, NO_DEADLOCK).await
}

/// The same, with the bound named by the caller.
///
/// The relegation sweep in `tests/dormancy_sweep.rs` is DELIBERATELY slow:
/// relegating an endpoint needs a sweep whose measured cost exceeds
/// `dormancy::MIN_COST_MS`, and the cost this policy is calibrated against is
/// wall-clock time, so no mock can produce a 30-second cost in less than 30
/// seconds. That test passes its own bound and is alone in its target. Every
/// other caller keeps `NO_DEADLOCK`.
///
/// Delegates to `within` rather than calling `tokio::time::timeout` here, so the
/// message a reader of a hung suite sees stays written once. An inlined copy of
/// it was the whole cost of this exception, and it would have left the
/// arrival-channel hang documented in two files that can drift.
pub async fn ran_within(command: &mut Command, bound: Duration) -> Output {
    let mut child = Reaped(
        command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the built binary must be runnable"),
    );
    let status = within(bound, async {
        loop {
            if let Some(status) = child.0.try_wait().expect("the child must be waitable") {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.0.stderr.take() {
        use std::io::Read;
        pipe.read_to_end(&mut stderr).expect("the child's stderr must be readable");
    }
    Output { status, stdout: Vec::new(), stderr }
}
