//! The one sweep in this suite that is deliberately slower than
//! `common::NO_DEADLOCK`, in a target of its own.
//!
//! It is here rather than in `tests/binary.rs`, whose module doc explains why
//! this crate runs the binary as a process at all, for one reason: libtest runs
//! a target's tests in parallel threads, and this one cannot finish in under
//! 30 s by construction, for the reason its own doc comment gives. While
//! this sweep held the machine, the four neighbours it had in `binary.rs` each
//! spawned a prober and two `wiremock` servers and blew the 20 s bound they
//! share, so a bare `cargo test` reported four deadlocks in code that has none
//! and cargo stopped at 5 of 15 targets. A target is a process with its own
//! scheduling, which is what this file buys, and it costs nothing else: no other
//! test's bound moved and no new bound was needed.
//!
//! Everything about the wait itself, the kill-on-drop and the timeout message,
//! is `common`'s. See `common::NO_DEADLOCK`, which now states the condition
//! under which it is one-sided.

mod common;

use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Where the mock records its arrivals, so the assertion that the second sweep
/// sends NOTHING is about requests that reached a server rather than about what
/// the prober says it did.
type Log = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;

/// A fresh directory under the target dir cargo already owns, named for this
/// process, so two runs of the suite cannot collide. One test in this target, so
/// unlike `binary.rs`'s helper it needs no per-test name.
fn tempdir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("dormancy-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The whole point of the dormancy stage, driven through the process.
///
/// Three things this task decides and nothing else can see, because
/// `end_to_end.rs` never invokes a process and so cannot reach any of them:
///
/// 1. `run_sweep` is handed `&plan.probe` and not the full endpoint list. An
///    implementer can satisfy every other instruction in this stage, publish a
///    correct dormancy section, and still probe all 543 endpoints; the only
///    thing that catches it is a sweep that must send NO requests and does.
/// 2. The write-back happens. `merge_state` runs after the footer and before the
///    failed-endpoint bail, so a strike a sweep measured reaches the disk.
/// 3. The run graph says what the sweep declined to ask, in the section this
///    stage adds.
///
/// It is deliberately slow, and the slowness is not incidental. The policy is
/// cost-weighted and the cost is summed wall-clock `elapsed_ms`, with a floor of
/// one `Budget::request` under the threshold flag (`dormancy::MIN_COST_MS`), so
/// a sweep cannot measure an expensive endpoint in less time than the threshold
/// itself. Two counted requests at `SILENT_REPLY` each is the cheapest shape
/// that clears 30 s: a `FetchWellKnown` metric, whose row carries the
/// description fetch's own elapsed time, plus one query metric. `--min-gap-ms 0`
/// keeps the pause out of it, since the gap is not what is being measured.
///
/// `--dormant-strikes 1` rather than the shipped 2, for the same reason: two
/// striking sweeps would cost twice as long and would show nothing the first one
/// does not. That the second strike is required at the default is
/// `dormancy.rs`'s own test.
#[tokio::test]
async fn a_slow_silent_endpoint_is_relegated_then_not_probed() {
    let dir = tempdir();
    let server = MockServer::start().await;
    let log: Log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(SlowAndSilent { log: std::sync::Arc::clone(&log) })
        .mount(&server)
        .await;

    let endpoint = format!("{}/sparql", server.uri());
    let list = dir.join("endpoints.toml");
    let defs = dir.join("metrics.toml");
    let state = dir.join("state").join("dormancy.toml");
    std::fs::write(&list, format!("endpoint = [{endpoint:?}]\n")).unwrap();
    std::fs::write(&defs, TWO_COUNTED_METRICS).unwrap();
    // The one thing that may create the state file, exactly as a fresh
    // deployment does it: a sweep that created its own could not tell "first
    // ever run" from "the volume did not get mounted".
    sparqlwatch_prober::state_file::init_state(&state).unwrap();

    let sweep = |at: &'static str, out: std::path::PathBuf| {
        let list = list.clone();
        let defs = defs.clone();
        let state = state.clone();
        async move {
            let mut command = Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"));
            command
                .args(["--endpoints", list.to_str().unwrap()])
                .args(["--metrics", defs.to_str().unwrap()])
                .args(["--state", state.to_str().unwrap()])
                .args(["--out", out.to_str().unwrap()])
                .args(["--at", at])
                .args(["--min-gap-ms", "0"])
                .args(["--dormant-cost-ms", "30000"])
                .args(["--dormant-strikes", "1"]);
            let status = common::ran_within(&mut command, SWEEP_BOUND).await;
            assert!(
                status.status.success(),
                "the sweep must exit zero when it failed on no endpoint, got {:?}: {}",
                status.status,
                String::from_utf8_lossy(&status.stderr)
            );
            std::fs::read_to_string(&out).unwrap()
        }
    };

    // Sweep one probes it, because the state has never seen it, and declines
    // nothing. The zero count is published all the same.
    let first = sweep(RELEGATING_AT, dir.join("run-1.nq")).await;
    let after_first = log.lock().unwrap_or_else(|p| p.into_inner()).len();
    assert_eq!(after_first, 2, "the description fetch and the one query metric");
    assert!(
        first.contains(
            "<urn:sparqlwatch:dormantCount> \"0\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "a sweep that declined nothing still says so as a fact: {first}"
    );
    assert!(
        !first.contains("urn:sparqlwatch:dormantEndpoint"),
        "and names no endpoint in its dormancy section: {first}"
    );
    assert!(first.contains("<http://www.w3.org/ns/dqv#computedOn>"), "it measured it: {first}");

    // The write-back reached the disk, which is what makes sweep two's decision
    // possible at all.
    let written = std::fs::read_to_string(&state).unwrap();
    assert!(
        written.contains(&format!("dormant_since = \"{RELEGATING_AT}\"")),
        "the strike and the relegation have to survive the process: {written}"
    );

    // Sweep two, a day later. The cadence is seven days, so the endpoint is
    // relegated and not due: `run_sweep` must never see it.
    let second = sweep("2026-08-25T12:00:00Z", dir.join("run-2.nq")).await;
    assert_eq!(
        log.lock().unwrap_or_else(|p| p.into_inner()).len(),
        after_first,
        "THE LINE THIS WHOLE STAGE IS FOR: a sweep handed `&plan.probe` sends no request to a \
         relegated endpoint. A sweep handed the full endpoint list sends two more."
    );
    assert!(
        second.contains(&format!("<urn:sparqlwatch:dormantEndpoint> <{endpoint}>")),
        "the run names what it declined to ask: {second}"
    );
    assert!(
        second.contains("<urn:sparqlwatch:dormancyReason> \"automatic\""),
        "and why, in the slug SkipReason publishes: {second}"
    );
    assert!(
        second.contains(&format!(
            "<urn:sparqlwatch:dormantSince> \"{RELEGATING_AT}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"
        )),
        "carrying the instant it was relegated, not this run's: {second}"
    );
    assert!(
        second.contains(
            "<urn:sparqlwatch:dormantCount> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "and the count that closes the section: {second}"
    );
    assert!(
        !second.contains("<http://www.w3.org/ns/dqv#computedOn>"),
        "dormancy is not a verdict and a declined endpoint gets no measurement: {second}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The instant sweep one runs at, and so the instant the relegation is dated.
/// One constant because the run graph sweep two writes has to carry exactly it.
const RELEGATING_AT: &str = "2026-08-24T12:00:00Z";

/// Half of one `dormancy::MIN_COST_MS` plus a margin, so two counted requests
/// sum to 31.2 s and clear the 30 s threshold that `--dormant-cost-ms` may not
/// be set below. Served by the mock, so it cannot arrive early on a fast
/// machine, and comfortably inside the 30 s request budget and 60 s metric
/// budget so neither cancels it.
const SILENT_REPLY: std::time::Duration = std::time::Duration::from_millis(15_600);

/// Room for two `SILENT_REPLY` replies, the process start-up, and a wide margin.
/// A bound and not a prediction: it can only be reached by a sweep that is never
/// coming back.
const SWEEP_BOUND: std::time::Duration = std::time::Duration::from_secs(90);

/// Two metrics whose rows BOTH carry an elapsed time, which is what makes this
/// endpoint's measured cost equal to the sweep's wall clock.
///
/// The `FetchWellKnown` metric is the reason for the pairing: the description is
/// fetched once per endpoint whatever the definitions say, and its elapsed time
/// reaches a row only through a metric of that kind (`lib.rs`'s `fetch_elapsed`).
/// Without it the fetch costs 15.6 s of wall clock that the policy never counts,
/// and the sweep would have to run a third request to reach the threshold.
const TWO_COUNTED_METRICS: &str = r#"
[[metric]]
id = "service-description"
label = "Service description informativeness"
dimension = "documentation"
kind = "FetchWellKnown"
cost = "cheap"

[[metric]]
id = "availability"
label = "Answers a trivial query"
dimension = "availability"
kind = "Liveness"
cost = "cheap"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"
"#;

/// A mock that records the arrival and then answers 500 after `SILENT_REPLY`.
///
/// Expensive AND silent, which is what the policy requires of both conditions:
/// three endpoints in the survey behind it answer in 61 to 89 s, so a
/// cost-only rule would relegate working endpoints. A 500 with no RDF body
/// resolves to `indeterminate` for the fetch and for the query, and neither is
/// `Verified` or `UndeclaredButVerified`, so the sweep is not positive and the
/// strike stands.
struct SlowAndSilent {
    log: Log,
}

impl wiremock::Respond for SlowAndSilent {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        self.log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push("silent");
        ResponseTemplate::new(500).set_delay(SILENT_REPLY)
    }
}
