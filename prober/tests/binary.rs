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
//! Waiting on the child from inside `#[tokio::test]` is safe here: a
//! `wiremock::MockServer` serves from a thread and runtime of its own
//! (`bare_server.rs` spawns both), so it keeps answering while this test waits.
//!
//! The wait is bounded, and that is not decoration. This is the only file that
//! runs the prober as a process, and the branch's most dangerous shape is a
//! sweep that HANGS: `run_sweep` drops the sweep's own sender before its drain
//! loop, and a version that does not never sees the channel close. `Child::wait`
//! and `Child::output` would then block this test forever, and
//! `std::process::Child::drop` does not kill, so a review of this branch found
//! two `sparqlwatch-prober` processes still alive minutes after `cargo test`
//! was killed. So the child is waited on under `common::NO_DEADLOCK`, the same
//! bound every in-process test uses, and killed on the way out either way.

mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// Run `command` to completion under `common::NO_DEADLOCK`, or fail by name.
///
/// `try_wait` in a poll loop rather than `output()`, because `output()` blocks
/// the thread and no timeout can interrupt it. Only stderr is piped, and it is
/// read after the child has exited: the assertions here quote stderr, and a
/// piped stdout would be one more pipe to keep from filling.
async fn ran_without_hanging(command: &mut Command) -> Output {
    ran_within(command, common::NO_DEADLOCK).await
}

/// The same, with the bound named by the caller.
///
/// One test in this file is DELIBERATELY slow: relegating an endpoint needs a
/// sweep whose measured cost exceeds `dormancy::MIN_COST_MS`, and the cost this
/// policy is calibrated against is wall-clock time, so no mock can produce a
/// 30-second cost in less than 30 seconds. That test passes its own bound. Every
/// other caller keeps `common::NO_DEADLOCK`, which is many times longer than
/// anything a local `wiremock` server needs and so is only ever reached by a
/// sweep that is never coming back.
async fn ran_within(command: &mut Command, bound: std::time::Duration) -> Output {
    let mut child = Reaped(
        command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the built binary must be runnable"),
    );
    let status = tokio::time::timeout(bound, async {
        loop {
            if let Some(status) = child.0.try_wait().expect("the child must be waitable") {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(
        "timed out: the prober is never coming back. A sweep whose arrival channel never \
         closes, which is what `run_sweep` dropping the sweep's own sender prevents",
    );
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.0.stderr.take() {
        use std::io::Read;
        pipe.read_to_end(&mut stderr).expect("the child's stderr must be readable");
    }
    Output { status, stdout: Vec::new(), stderr }
}

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
    // A state file of its own per concurrency, because these run in parallel
    // and a shared one would have two sweeps merging into one file. `--state` is
    // required in the sense that matters: the sweep reads the state before it
    // probes anything and fails closed on a missing file, since a state read as
    // empty would re-admit every relegated endpoint.
    let state = dir.join(format!("state-{concurrency}")).join("dormancy.toml");
    std::fs::write(&list, endpoints).unwrap();
    std::fs::write(&defs, METRICS).unwrap();
    sparqlwatch_prober::state_file::init_state(&state).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"));
    command
        .args(["--endpoints", list.to_str().unwrap()])
        .args(["--metrics", defs.to_str().unwrap()])
        .args(["--state", state.to_str().unwrap()])
        .args(["--out", out.to_str().unwrap()])
        .args(["--at", "2026-08-23T12:00:00Z"])
        .args(["--min-gap-ms", "0"])
        .args(["--concurrency", &concurrency.to_string()]);
    let status = ran_without_hanging(&mut command).await;
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

/// A bare `cargo run` in `prober/` has to be the sweeper.
///
/// Two `[[bin]]` targets and no `default-run` key make `cargo run` an error
/// instead of a sweep, and a bare `cargo run` is what `README.md:69`, `:1276`
/// and `src/bin/seed-registry.rs:11` instruct. No test can invoke `cargo run`
/// itself without running cargo inside cargo, so this asserts the manifest key
/// those instructions depend on.
#[test]
fn a_bare_cargo_run_in_this_crate_is_the_sweeper() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains("\ndefault-run = \"sparqlwatch-prober\"\n"),
        "prober/Cargo.toml has two [[bin]] targets, so it must name one as \
         default-run or `cargo run` is an error: {manifest}"
    );
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

/// An excluded host is not contacted by the real binary, and does not appear in
/// the run it writes.
///
/// The one place this can be shown end to end. `registry.rs` proves the rule
/// and `load_endpoints` proves the wiring inside the library, but only a process
/// can show that `main` reads the file it is pointed at and hands the list to
/// the loader: a review of stage 1c-b3 measured what leaving that untested
/// costs, and a `main` that read the flag and passed `&[]` would look exactly
/// like this one.
///
/// Both entries name the SAME mock server, one by `127.0.0.1` and one by
/// `localhost`, so the host string is the only thing that differs and the
/// request count is unambiguous: three requests is one endpoint's worth, six
/// would be both. The excluded name is never resolved, so whether `localhost`
/// resolves on this machine cannot affect the result.
#[tokio::test]
async fn an_excluded_host_is_never_contacted_by_the_binary() {
    let dir = tempdir("excluded");
    let log: Log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sparql"))
        .respond_with(Recording {
            label: "kept",
            delay: std::time::Duration::ZERO,
            log: std::sync::Arc::clone(&log),
        })
        .mount(&server)
        .await;

    let port = server.address().port();
    let kept = format!("http://127.0.0.1:{port}/sparql");
    let excluded = format!("http://localhost:{port}/sparql");
    let list = dir.join("endpoints.toml");
    let defs = dir.join("metrics.toml");
    let exclusions = dir.join("exclusions.toml");
    let out = dir.join("run.nq");
    let state = dir.join("state").join("dormancy.toml");
    std::fs::write(&list, format!("endpoint = [{excluded:?}, {kept:?}]\n")).unwrap();
    std::fs::write(&defs, METRICS).unwrap();
    std::fs::write(
        &exclusions,
        "[[exclusion]]\nhost = \"localhost\"\nreason = \"a person asked, 2026-08-25\"\n",
    )
    .unwrap();
    sparqlwatch_prober::state_file::init_state(&state).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"));
    command
        .args(["--endpoints", list.to_str().unwrap()])
        .args(["--metrics", defs.to_str().unwrap()])
        .args(["--exclusions", exclusions.to_str().unwrap()])
        .args(["--state", state.to_str().unwrap()])
        .args(["--out", out.to_str().unwrap()])
        .args(["--at", "2026-08-25T12:00:00Z"])
        .args(["--min-gap-ms", "0"]);
    let status = ran_without_hanging(&mut command).await;
    assert!(
        status.status.success(),
        "the sweep must exit zero when it failed on no endpoint, got {:?}: {}",
        status.status,
        String::from_utf8_lossy(&status.stderr)
    );

    let arrivals = log.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(
        arrivals.len(),
        3,
        "three requests is one endpoint's worth: the excluded host must not be contacted at \
         all, got {arrivals:?}"
    );

    let nq = std::fs::read_to_string(&out).unwrap();
    assert!(!nq.contains("localhost"), "an excluded host may not appear in a run: {nq}");
    assert!(nq.contains(&kept), "the endpoint that nobody excluded must be measured: {nq}");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A sweep whose exclusion list cannot be read refuses to start, names the
/// path, and writes no run.
///
/// The file is read at run time, so "not where this process was told to look"
/// is a state that exists: a container that forgot to mount it, or a prober
/// started from the wrong working directory with the default relative path.
/// Failing closed makes that a loud stop instead of a sweep of every host that
/// had asked to be left alone.
#[tokio::test]
async fn a_sweep_whose_exclusion_list_cannot_be_read_refuses_to_start() {
    let dir = tempdir("no-list");
    let list = dir.join("endpoints.toml");
    let defs = dir.join("metrics.toml");
    let missing = dir.join("absent-exclusions.toml");
    let out = dir.join("run.nq");
    std::fs::write(&list, "endpoint = [\"https://b/sparql\"]\n").unwrap();
    std::fs::write(&defs, METRICS).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_sparqlwatch-prober"));
    command
        .args(["--endpoints", list.to_str().unwrap()])
        .args(["--metrics", defs.to_str().unwrap()])
        .args(["--exclusions", missing.to_str().unwrap()])
        .args(["--out", out.to_str().unwrap()])
        .args(["--at", "2026-08-25T12:00:00Z"]);
    let status = ran_without_hanging(&mut command).await;

    assert!(
        !status.status.success(),
        "a sweep that cannot read the exclusion list must not report success, exited {:?}",
        status.status
    );
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("absent-exclusions.toml"),
        "the failure must name the path it could not read: {stderr}"
    );
    assert!(!out.exists(), "and no run may be written: {}", out.display());

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
    let dir = tempdir("dormancy");
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
            let status = ran_within(&mut command, SWEEP_BOUND).await;
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
