//! The binary itself, run as a process against mock endpoints.
//!
//! Every other test in this suite calls the library. That leaves `main.rs`
//! untested, and `main.rs` is the only place a parsed flag is connected to
//! `run_sweep` and the only place `Sweep::failed_endpoints` is connected to
//! `RunEmission`. A review of stage 1c-b3 measured what that costs: passing
//! `NonZeroUsize::new(1).unwrap()` to `run_sweep` instead of
//! `args.concurrency`, and hardcoding the emitted `concurrency` and
//! `failed_endpoints`, each left the whole suite green. An earlier stage
//! recorded the same gap about the flags reaching `Client::new`.
//!
//! `env!("CARGO_BIN_EXE_sparqlwatch-prober")` is the built binary's path,
//! which cargo sets for integration targets, so this needs no dependency
//! beyond what the suite already has.
//!
//! Blocking on the child from inside `#[tokio::test]` is safe here: a
//! `wiremock::MockServer` serves from a thread and runtime of its own
//! (`bare_server.rs` spawns both), so it keeps answering while this thread
//! waits.

use std::path::Path;
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A mock that records that a request reached this host and then answers after
/// a fixed server-side delay.
///
/// The delay is what makes the overlap assertions below structural rather than
/// a guess about machine speed: a request answered after `delay` was in flight
/// for at least that long, and the delay is served by the mock, so it cannot
/// arrive early on a fast machine.
struct Recording {
    label: &'static str,
    delay: std::time::Duration,
    log: Log,
}

impl wiremock::Respond for Recording {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        // A poisoned log means an assertion already panicked while holding it,
        // and the test is failing either way; recovering the inner vec keeps
        // the failure the assertion rather than a second panic in here.
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(self.label);
        ResponseTemplate::new(200)
            .insert_header("access-control-allow-origin", "*")
            // One body that satisfies both metrics `METRICS` defines: a
            // `boolean` for the liveness probe and one IRI binding for the
            // enumerating one.
            .set_body_string(
                r#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://example.org/C"}}]},"boolean":true}"#,
            )
            .set_delay(self.delay)
    }
}

type Log = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;

/// Two metrics that run and one the default ceiling declines, so a sweep over
/// them publishes a measurement, a content sample and a `NotMeasured` fact.
/// Three requests per endpoint follow: the queryless description fetch, plus
/// one query per metric that runs.
const METRICS: &str = r#"
[[metric]]
id = "availability"
label = "Answers a trivial query"
dimension = "availability"
kind = "Liveness"
cost = "cheap"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"

[[metric]]
id = "has-classes"
label = "Holds typed resources"
dimension = "content"
kind = "SelectIris"
var = "c"
cost = "cheap"
query = "SELECT ?c WHERE { ?s a ?c } LIMIT 1"

[[metric]]
id = "classes"
label = "Distinct classes"
dimension = "content"
kind = "SelectIris"
var = "c"
cost = "expensive"
sample_limit = 200
query = "SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200"
"#;

/// Long enough that the fast host's whole group lands inside one of the slow
/// host's replies, and short enough to keep the sweep to a couple of seconds.
const SLOW_REPLY: std::time::Duration = std::time::Duration::from_millis(500);

/// One sweep of the binary over two hosts, one slow and one immediate.
///
/// Returns the emitted document and the order requests reached the two hosts.
/// `--min-gap-ms 0` because the property under test is which HOSTS run at
/// once; the per-host gate is not a gap and stays in force at zero, and every
/// other test of the gap itself drives `politeness` directly.
async fn sweep(dir: &Path, concurrency: u32) -> (String, Vec<&'static str>) {
    let log: Log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let slow = MockServer::start().await;
    let fast = MockServer::start().await;
    for (server, label, delay) in [
        (&slow, "slow", SLOW_REPLY),
        (&fast, "fast", std::time::Duration::ZERO),
    ] {
        Mock::given(method("GET"))
            .and(path("/sparql"))
            .respond_with(Recording { label, delay, log: std::sync::Arc::clone(&log) })
            .mount(server)
            .await;
    }

    // The slow host is FIRST, so at a concurrency of one the run is not merely
    // un-overlapped: the fast host waits for the whole slow group.
    let endpoints = format!(
        "endpoint = [\"{}/sparql\", \"{}/sparql\"]\n",
        slow.uri(),
        fast.uri()
    );
    let out = dir.join(format!("run-{concurrency}.nq"));
    let list = dir.join(format!("endpoints-{concurrency}.toml"));
    let defs = dir.join("metrics.toml");
    std::fs::write(&list, endpoints).unwrap();
    std::fs::write(&defs, METRICS).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"))
        .args(["--endpoints", list.to_str().unwrap()])
        .args(["--metrics", defs.to_str().unwrap()])
        .args(["--out", out.to_str().unwrap()])
        .args(["--at", "2026-08-23T12:00:00Z"])
        .args(["--min-gap-ms", "0"])
        .args(["--concurrency", &concurrency.to_string()])
        .output()
        .expect("the built binary must be runnable");
    assert!(
        status.status.success(),
        "the sweep must exit zero when it failed on no endpoint, got {:?}: {}",
        status.status,
        String::from_utf8_lossy(&status.stderr)
    );

    let arrivals = log.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(arrivals.len(), 6, "three requests per endpoint: {arrivals:?}");
    (std::fs::read_to_string(&out).unwrap(), arrivals)
}

/// Whether the two hosts' requests are interleaved rather than one host's
/// group running to completion before the other's begins.
fn overlapped(arrivals: &[&str]) -> bool {
    let first_of = |l: &str| arrivals.iter().position(|a| *a == l).unwrap();
    let last_of = |l: &str| arrivals.iter().rposition(|a| *a == l).unwrap();
    first_of("fast") < last_of("slow") && first_of("slow") < last_of("fast")
}

#[tokio::test]
async fn the_concurrency_flag_reaches_the_sweep_and_the_graph() {
    let dir = tempdir("concurrency");

    let (two, arrivals) = sweep(&dir, 2).await;
    assert!(
        overlapped(&arrivals),
        "at --concurrency 2 the two hosts must be in flight together, got {arrivals:?}"
    );
    assert!(
        two.contains(
            "<urn:sparqlwatch:concurrency> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "the run must publish the concurrency it was given: {two}"
    );

    let (one, arrivals) = sweep(&dir, 1).await;
    assert!(
        !overlapped(&arrivals),
        "at --concurrency 1 one host's group must finish before the other starts, \
         got {arrivals:?}"
    );
    assert!(
        one.contains(
            "<urn:sparqlwatch:concurrency> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "the run must publish the concurrency it was given: {one}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_published_failure_count_describes_the_run_that_published_it() {
    // A sweep that failed on nothing, and the emitted count says so as a fact
    // rather than by omission (`emit.rs` publishes it at zero deliberately).
    // The evidence beside it is asserted too, because a hardcoded zero and an
    // honest zero are the same three characters: every endpoint carries the
    // measurements the run made, nothing carries `prober-failed`, and the
    // process exited zero, which `main.rs` only does when the count is zero.
    //
    // What this cannot catch, precisely: a hardcoded count that happens to
    // equal the truth. Any OTHER constant is caught, because the exit status
    // reads the local rather than the emitted field, so an emitted `1` on a
    // complete run exits zero and reds the assertion below. A run with a
    // genuinely non-zero count is not reachable through the binary at all:
    // the only definition shape that panics a probing task is one
    // `load_metrics` refuses, so it cannot come from a `--metrics` file. That
    // direction is covered where it is reachable, by
    // `a_panicked_group_publishes_prober_failed_for_every_endpoint_it_held` in
    // tests/end_to_end.rs, which calls `run_sweep` directly.
    let dir = tempdir("failures");
    let (nq, _) = sweep(&dir, 2).await;

    assert!(
        nq.contains(
            "<urn:sparqlwatch:failedEndpoints> \"0\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "a complete run has to say so as a fact: {nq}"
    );
    assert!(!nq.contains("prober-failed"), "nothing failed in this run: {nq}");

    // Two endpoints, two metrics that ran, so two measurements each, and the
    // one the ceiling declined recorded as a `cost-ceiling` fact each.
    assert_eq!(nq.matches("<http://www.w3.org/ns/dqv#computedOn>").count(), 4);
    assert_eq!(nq.matches("\"cost-ceiling\"").count(), 2);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A fresh directory under the target dir cargo already owns, named for the
/// caller and for this process, so neither two tests in this file nor two runs
/// of the suite can collide. They would: the tests here run in parallel and
/// each writes an endpoint list naming its own mock servers, so a shared name
/// points one test's binary at the other's hosts.
fn tempdir(named: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("binary-{named}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
