//! The operator's override, run as a process against a real state file.
//!
//! `src/bin/dormancy.rs` is thin by design: `dormancy::wake`, `sleep` and
//! `prunable` hold the policy and are unit-tested where they live, and
//! `state_file::merge_state` holds the lock and the atomic replace and is
//! unit-tested where it lives. What only a process can answer is whether the
//! binary WIRES them: whether `--reason` is really refused when blank rather
//! than trimmed into nothing and written, whether every mutation really goes
//! through the lock rather than around it, whether `--dry-run` really writes
//! nothing, and whether what `list` prints is any use to the person reading it.
//!
//! `prober/tests/seed_registry.rs` is the precedent and this follows its shape:
//! `env!("CARGO_BIN_EXE_dormancy")` is the built binary's path, which cargo
//! sets for integration targets, so running the real process needs no
//! dependency beyond what the suite already has.
//!
//! No timeout guard, unlike `binary.rs`. That file bounds its wait because a
//! sweep can hang on a network read; this binary reads two small files and
//! writes one, with no socket and no runtime anywhere in it.

use sparqlwatch_prober::dormancy::{EndpointState, Hold, State};
use sparqlwatch_prober::state_file::{init_state, lock_path, read_state};
use std::path::{Path, PathBuf};
use std::process::Output;

/// A fresh directory under the target dir cargo already owns, named for the
/// caller and this process, so two tests in this file cannot overwrite each
/// other's state. Same helper as `seed_registry.rs`, and for the same reason:
/// these tests run in parallel threads and share one state file name.
fn tempdir(named: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("dormancy-{named}-{}", std::process::id()));
    // Removed first, so a re-run of one test in the same process id does not
    // meet the previous run's state file and get `init`'s refusal instead of
    // the behaviour under test.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the target tmpdir must be writable");
    dir
}

/// The state path inside `dir`, one level down, so `init` has a directory to
/// create: `state/` is git-ignored and a fresh checkout does not have it.
fn state_path(dir: &Path) -> PathBuf {
    dir.join("state/dormancy.toml")
}

/// Run the binary with `args` and return what it did. Never asserts success:
/// most of these tests are about a refusal.
fn dormancy(args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_dormancy"))
        .args(args)
        .output()
        .expect("the built dormancy binary must be runnable")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A state file holding exactly `endpoints`, written through `State::render` so
/// the fixture is in the format the binary reads rather than in a hand-typed
/// approximation of it.
fn plant(path: &Path, endpoints: Vec<EndpointState>) {
    std::fs::create_dir_all(path.parent().expect("the state path has a parent")).unwrap();
    let state = State {
        updated_at: Some("2026-08-20T12:00:00Z".to_string()),
        last_sweep_at: Some("2026-08-20T12:00:00Z".to_string()),
        endpoint: endpoints,
        ..State::empty()
    };
    std::fs::write(path, state.render()).unwrap();
    // Planted fixtures go through the very reader the binary uses, so a fixture
    // this build cannot parse fails here rather than as a confusing refusal
    // from the process under test.
    read_state(path).expect("a planted fixture must be readable");
}

/// An endpoint list file naming `endpoints`, for `prune`.
fn endpoints_file(dir: &Path, endpoints: &[&str]) -> PathBuf {
    let path = dir.join("endpoints.toml");
    let quoted: Vec<String> = endpoints.iter().map(|url| format!("  {url:?},")).collect();
    std::fs::write(&path, format!("endpoint = [\n{}\n]\n", quoted.join("\n"))).unwrap();
    path
}

/// An exclusion list file naming `hosts`. Written per test rather than reusing
/// the shipped one, because two of these tests are about what an exclusion does
/// to a prune and the shipped list excludes nothing they could name.
fn exclusions_file(dir: &Path, hosts: &[&str]) -> PathBuf {
    let path = dir.join("exclusions.toml");
    let body: String = hosts
        .iter()
        .map(|host| format!("[[exclusion]]\nhost = {host:?}\nreason = \"a test asked\"\n"))
        .collect();
    std::fs::write(&path, body).unwrap();
    path
}

/// Three urls for the fixtures. NOT under `.example`: `load_endpoints` applies
/// `without_reserved_names`, which drops every RFC 2606 documentation name, so
/// `a.example` would arrive at `prunable` as an unlisted url whatever
/// `endpoints.toml` says. That made an earlier draft of the prune tests pass
/// for the wrong reason and made the exclusion test vacuous, since every url in
/// it was being dropped before the exclusion list was consulted.
const A: &str = "https://alpha.sparqlwatch-fixture.org/sparql";
const B: &str = "https://beta.sparqlwatch-fixture.org/sparql";
const C: &str = "https://gamma.sparqlwatch-fixture.org/sparql";
const AT: &str = "2026-08-26T19:45:00Z";

// ---------------------------------------------------------------------------
// `--reason`
// ---------------------------------------------------------------------------

/// A hold with no reason is refused by the parser, so `--help` and the tool
/// agree that it is not optional.
#[test]
fn a_wake_without_a_reason_is_refused() {
    let dir = tempdir("wake-no-reason");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&["wake", A, "--state", state.to_str().unwrap(), "--at", AT]);

    assert!(!out.status.success(), "a wake with no reason must not succeed: {}", stdout(&out));
    assert!(
        stderr(&out).contains("--reason"),
        "the refusal must name the flag that is missing: {}",
        stderr(&out)
    );
    assert_eq!(
        read_state(&state).unwrap().endpoint,
        Vec::new(),
        "nothing may be written by a command that was refused"
    );
}

/// And a reason of nothing but spaces is refused too.
///
/// This is the one clap cannot catch: `NonEmptyStringValueParser` sees a
/// three-character string and passes it. The rule lives in
/// `dormancy::checked_reason`, and this test is what says the binary reaches it
/// BEFORE writing, rather than trimming a blank reason into the file where the
/// next read refuses it.
#[test]
fn a_wake_with_a_blank_reason_is_refused() {
    let dir = tempdir("wake-blank-reason");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&[
        "wake", A, "--reason", "   ", "--state", state.to_str().unwrap(), "--at", AT,
    ]);

    assert!(!out.status.success(), "a blank reason must not succeed: {}", stdout(&out));
    assert!(
        stderr(&out).contains("reason"),
        "the refusal must say what was wrong: {}",
        stderr(&out)
    );
    assert_eq!(
        read_state(&state).unwrap().endpoint,
        Vec::new(),
        "a blank reason must leave the file as it was"
    );
}

/// The same rule on `sleep`, which is the direction that takes an endpoint out
/// of every sweep and so is the one where an unexplained decision costs most.
#[test]
fn a_sleep_with_a_blank_reason_is_refused() {
    let dir = tempdir("sleep-blank-reason");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&[
        "sleep", A, "--reason", "\t \n", "--state", state.to_str().unwrap(), "--at", AT,
    ]);

    assert!(!out.status.success(), "a blank reason must not succeed: {}", stdout(&out));
    assert_eq!(read_state(&state).unwrap().endpoint, Vec::new());
}

// ---------------------------------------------------------------------------
// `wake` and `sleep`
// ---------------------------------------------------------------------------

/// An operator may know something before the machine does: an endpoint's admin
/// can say it is fixed before any sweep has seen it answer. So a wake against a
/// url the state has never heard of records the hold rather than refusing.
#[test]
fn waking_an_endpoint_the_state_does_not_know_still_records_the_hold() {
    let dir = tempdir("wake-unknown");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&[
        "wake", A, "--reason", "admin says it is fixed", "--state",
        state.to_str().unwrap(), "--at", AT,
    ]);

    assert!(out.status.success(), "the wake must succeed: {}", stderr(&out));
    // What the operator over ssh actually sees. Asserted because it is the only
    // confirmation they get and because it is the only check on a url that was
    // trimmed or mistyped: `checked_url` trims surrounding whitespace and
    // compares nothing against the registry, so the printed entry is where a
    // paste that went wrong becomes visible. Delete the print and this reds.
    let said = stdout(&out);
    assert!(said.contains(A), "the entry that was written must be printed: {said}");
    assert!(
        said.contains("admin says it is fixed"),
        "with the reason now in the file, so a wrong one is seen at once: {said}"
    );
    assert!(said.contains("2026-09-02T19:45:00Z"), "and the expiry computed for it: {said}");

    let written = read_state(&state).unwrap();
    let entry = written.get(A).expect("the url the state had never seen must now have an entry");
    assert_eq!(
        entry.hold,
        Some(Hold::Awake {
            reason: "admin says it is fixed".to_string(),
            // `--at` plus the default seven grace days, time of day preserved.
            until: Some("2026-09-02T19:45:00Z".to_string()),
        })
    );
    assert_eq!(written.updated_at.as_deref(), Some(AT));
    assert_eq!(
        written.last_sweep_at.as_deref(), None,
        "a wake is not a sweep: counting it as one would shrink the next sweep's slice"
    );
}

/// `--pin` is the wake that never lapses.
#[test]
fn a_pinned_wake_records_a_hold_with_no_expiry() {
    let dir = tempdir("wake-pin");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&[
        "wake", A, "--reason", "watched by hand", "--pin", "--state",
        state.to_str().unwrap(), "--at", AT,
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        read_state(&state).unwrap().get(A).unwrap().hold,
        Some(Hold::Awake { reason: "watched by hand".to_string(), until: None })
    );
}

/// `--pin` and `--grace-days` contradict each other, so passing both is refused
/// rather than resolved.
///
/// Resolving it quietly is the failure this closes: `--pin` won, the hold was
/// written with no expiry, and the operator who typed `--grace-days 30` was told
/// nothing and would have believed a thirty day grace was in place. clap does
/// not count a defaulted value as present, so a plain `--pin` still works, which
/// is the other half of what this asserts.
#[test]
fn pin_and_an_explicit_grace_contradict_each_other_and_are_refused() {
    let dir = tempdir("wake-pin-grace");
    let state = state_path(&dir);
    init_state(&state).unwrap();
    let state_arg = state.to_str().unwrap();

    let both = dormancy(&[
        "wake", A, "--reason", "watched by hand", "--pin", "--grace-days", "30",
        "--state", state_arg, "--at", AT,
    ]);
    assert!(!both.status.success(), "--pin with an explicit --grace-days must be refused");
    let said = stderr(&both);
    assert!(said.contains("--pin"), "the refusal must name both flags: {said}");
    assert!(said.contains("--grace-days"), "the refusal must name both flags: {said}");
    assert_eq!(
        read_state(&state).unwrap().endpoint,
        Vec::new(),
        "and write nothing at all"
    );

    // The default must not count as present, or `--pin` alone would refuse too.
    let pinned =
        dormancy(&["wake", A, "--reason", "watched by hand", "--pin", "--state", state_arg, "--at", AT]);
    assert!(pinned.status.success(), "--pin alone must still work: {}", stderr(&pinned));
    // And an explicit --grace-days without --pin.
    let graced = dormancy(&[
        "wake", B, "--reason", "two days is enough", "--grace-days", "2",
        "--state", state_arg, "--at", AT,
    ]);
    assert!(graced.status.success(), "{}", stderr(&graced));
    assert_eq!(
        read_state(&state).unwrap().get(B).unwrap().hold,
        Some(Hold::Awake {
            reason: "two days is enough".to_string(),
            until: Some("2026-08-28T19:45:00Z".to_string()),
        }),
        "and --grace-days must reach the expiry, since nothing else can set it"
    );
}

/// The last command wins, and it wins by REPLACING the hold rather than adding
/// a second one. An entry carrying both would have to be resolved by whichever
/// arm of `hold_effect` happened to be read first.
#[test]
fn a_wake_then_a_sleep_leaves_only_the_sleep() {
    let dir = tempdir("wake-then-sleep");
    let state = state_path(&dir);
    init_state(&state).unwrap();
    let state_arg = state.to_str().unwrap();

    let woken = dormancy(&["wake", A, "--reason", "admin says fixed", "--state", state_arg, "--at", AT]);
    assert!(woken.status.success(), "{}", stderr(&woken));
    let slept = dormancy(&[
        "sleep", A, "--reason", "admin changed their mind", "--state", state_arg,
        "--at", "2026-08-26T20:00:00Z",
    ]);
    assert!(slept.status.success(), "{}", stderr(&slept));

    let written = read_state(&state).unwrap();
    assert_eq!(written.endpoint.len(), 1, "one url is one entry");
    let entry = written.get(A).unwrap();
    assert_eq!(
        entry.hold,
        Some(Hold::Dormant { reason: "admin changed their mind".to_string() }),
        "the sleep must replace the wake, not sit beside it"
    );
    assert_eq!(
        entry.dormant_since.as_deref(),
        Some("2026-08-26T20:00:00Z"),
        "the sleep is what relegated it, so that is the instant a run graph publishes"
    );
}

/// A sleep on a url no sweep has probed produces `dormant_since` with no
/// `last_probed`, which is the ONE combination `State::parse` treats specially.
///
/// If that exemption were not there, or if this binary wrote the entry some
/// other way, one `dormancy sleep` on a new url would brick every subsequent
/// sweep and every subsequent `dormancy` command until a human hand-edited the
/// file. `read_state` is the exact call the prober makes before it probes
/// anything, so this is that check.
#[test]
fn sleeping_a_new_url_leaves_a_state_the_prober_can_still_read() {
    let dir = tempdir("sleep-new-url");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&[
        "sleep", A, "--reason", "its admin asked", "--state", state.to_str().unwrap(),
        "--at", AT,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains(A), "the entry written must be printed: {said}");
    assert!(said.contains("its admin asked"), "with its reason: {said}");
    assert!(
        said.contains("dormant"),
        "and the direction, since a sleep and a wake print the same shape: {said}"
    );

    let written = read_state(&state).expect("the prober must still be able to read this file");
    let entry = written.get(A).unwrap();
    assert_eq!(entry.dormant_since.as_deref(), Some(AT));
    assert_eq!(entry.last_probed, None, "nothing has probed it, so nothing may claim to have");
    // And the binary can read it back too, which is the other half of not
    // bricking the deployment.
    let listed = dormancy(&["list", "--state", state.to_str().unwrap()]);
    assert!(listed.status.success(), "{}", stderr(&listed));
}

// ---------------------------------------------------------------------------
// `--at`
// ---------------------------------------------------------------------------

/// `--at` is required on both writers and has no default.
///
/// The lock file records the process's ARGV, and `merge_state` is handed no
/// instant of its own, so argv is the only place a run's identity is available
/// to the message a later run prints about a stale lock. A default would leave
/// that message with nothing to name.
#[test]
fn at_is_required_on_every_writer_so_a_stale_lock_can_name_the_run() {
    let dir = tempdir("at-required");
    let state = state_path(&dir);
    init_state(&state).unwrap();
    let state_arg = state.to_str().unwrap();

    for verb in ["wake", "sleep"] {
        let out = dormancy(&[verb, A, "--reason", "because", "--state", state_arg]);
        assert!(!out.status.success(), "{verb} without --at must be refused");
        assert!(
            stderr(&out).contains("--at"),
            "the refusal must name the flag: {}",
            stderr(&out)
        );
    }
}

/// A malformed `--at` is refused BEFORE the state file is read, so the operator
/// hears about the flag they typed rather than about a file they have not
/// created yet.
///
/// The `--state` here deliberately does not exist. If the argument check ran
/// after the read, this would come back as `read_state`'s "run dormancy init"
/// message, which is a true sentence about the wrong problem.
#[test]
fn a_malformed_at_is_refused_before_the_state_file_is_read() {
    let dir = tempdir("at-malformed");
    let missing = state_path(&dir);
    let missing_arg = missing.to_str().unwrap();

    // An offset, a fractional second, a leap second and a date that never
    // existed: the four spellings the narrowed grammar exists to refuse.
    for bad in [
        "2026-08-26T21:45:00+02:00",
        "2026-08-26T19:45:00.000Z",
        "2026-12-31T23:59:60Z",
        "2026-02-30T19:45:00Z",
        "banana",
    ] {
        let out = dormancy(&["wake", A, "--reason", "because", "--state", missing_arg, "--at", bad]);
        assert!(!out.status.success(), "--at {bad:?} must be refused");
        let said = stderr(&out);
        assert!(said.contains("--at"), "the refusal must name --at, got: {said}");
        assert!(
            !said.contains("dormancy init"),
            "a bad --at must not be reported as a missing state file, got: {said}"
        );
    }
    assert!(!missing.exists(), "a refused command must not create the state file");
}

// ---------------------------------------------------------------------------
// `init`
// ---------------------------------------------------------------------------

/// `init` creates the file and the directory above it, and a sweep can read
/// what it wrote. This is the command a fresh checkout needs before it can
/// sweep at all.
#[test]
fn init_writes_a_state_a_sweep_can_read() {
    let dir = tempdir("init");
    let state = state_path(&dir);

    let out = dormancy(&["init", "--state", state.to_str().unwrap()]);

    assert!(out.status.success(), "{}", stderr(&out));
    // Naming the path it wrote, because `--state` has a default relative to the
    // working directory and the commonest way to get this wrong is to
    // bootstrap a file somewhere nothing will ever look. A silent success also
    // reads like a no-op.
    let said = stdout(&out);
    assert!(
        said.contains(state.to_str().unwrap()),
        "init must say which file it created: {said}"
    );
    let written = read_state(&state).expect("init's output must be readable");
    assert_eq!(written, State::empty());
}

/// And it refuses a file that exists rather than truncating one holding strike
/// counts a sweep spent ninety minutes measuring.
#[test]
fn init_refuses_a_state_that_already_exists_and_changes_nothing() {
    let dir = tempdir("init-twice");
    let state = state_path(&dir);
    plant(&state, vec![held(A, "keep me")]);
    let before = std::fs::read(&state).unwrap();

    let out = dormancy(&["init", "--state", state.to_str().unwrap()]);

    assert!(!out.status.success(), "a second init must be refused");
    assert!(
        stderr(&out).contains("already exists"),
        "the refusal must say what it found: {}",
        stderr(&out)
    );
    assert_eq!(std::fs::read(&state).unwrap(), before, "and it must not touch the file");
}

// ---------------------------------------------------------------------------
// `list`
// ---------------------------------------------------------------------------

/// An empty state is a fact worth stating. Printing nothing at all reads like a
/// broken command, and the operator's next move would be to check whether the
/// tool ran.
#[test]
fn list_over_an_empty_state_says_so_rather_than_printing_nothing() {
    let dir = tempdir("list-empty");
    let state = state_path(&dir);
    init_state(&state).unwrap();

    let out = dormancy(&["list", "--state", state.to_str().unwrap()]);

    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    let lines: Vec<&str> = said.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "an empty state prints exactly one line, got {lines:?}");
    assert!(lines[0].contains("empty"), "and it says so in a word: {said}");
    assert!(
        lines[0].contains(state.to_str().unwrap()),
        "naming the file, because the commonest cause of an empty state is the wrong \
         --state: {said}"
    );
}

/// A lapsed hold is the record that a person intervened and that the machine
/// has since taken back over, and it is the only thing that can answer "why was
/// this endpoint in last week's sweep and not in this one".
#[test]
fn list_reports_a_lapsed_hold_so_an_operator_can_see_a_person_intervened() {
    let dir = tempdir("list-lapsed");
    let state = state_path(&dir);
    plant(
        &state,
        vec![EndpointState {
            url: A.to_string(),
            strikes: 2,
            last_probed: Some("2026-08-20T12:00:00Z".to_string()),
            dormant_since: Some("2026-08-20T12:00:00Z".to_string()),
            lapsed_hold: Some("admin promised a fix, 2026-08-01".to_string()),
            ..Default::default()
        }],
    );

    let out = dormancy(&["list", "--state", state.to_str().unwrap()]);

    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(
        said.contains("admin promised a fix, 2026-08-01"),
        "the lapsed hold's reason must be printed verbatim: {said}"
    );
    assert!(said.contains("lapsed"), "and be labelled as lapsed rather than live: {said}");
}

/// `list` prints `last_sweep_at` and not only `updated_at`.
///
/// The two are equal after every sweep and differ only between a sweep and an
/// operator command, and `last_sweep_at` is the one that predicts the next
/// sweep's rotation size: it is the divisor of the cadence slice, so an
/// operator asking why nine of fifty-seven endpoints were probed is asking
/// about this field and not the other.
#[test]
fn list_prints_last_sweep_at_and_not_only_updated_at() {
    let dir = tempdir("list-sweep-at");
    let state = state_path(&dir);
    plant(&state, vec![held(A, "a reason")]);
    // A hold placed after the sweep, so the two instants differ and a listing
    // that printed only one of them could not be mistaken for printing both.
    let placed = dormancy(&[
        "sleep", B, "--reason", "later", "--state", state.to_str().unwrap(), "--at", AT,
    ]);
    assert!(placed.status.success(), "{}", stderr(&placed));

    let said = stdout(&dormancy(&["list", "--state", state.to_str().unwrap()]));

    assert!(said.contains("last_sweep_at"), "the field must be named: {said}");
    assert!(said.contains("2026-08-20T12:00:00Z"), "with the sweep's instant: {said}");
    assert!(said.contains("updated_at"), "and updated_at beside it: {said}");
    assert!(said.contains(AT), "with the command's instant: {said}");
}

/// Dormant endpoints first, then by url. The dormant ones are what an operator
/// opened this for; the awake ones are context.
#[test]
fn list_puts_dormant_endpoints_first_then_orders_by_url() {
    let dir = tempdir("list-order");
    let state = state_path(&dir);
    plant(
        &state,
        vec![
            EndpointState { url: A.to_string(), strikes: 1, ..Default::default() },
            EndpointState {
                url: C.to_string(),
                strikes: 2,
                last_probed: Some("2026-08-20T12:00:00Z".to_string()),
                dormant_since: Some("2026-08-20T12:00:00Z".to_string()),
                ..Default::default()
            },
            EndpointState {
                url: B.to_string(),
                strikes: 2,
                last_probed: Some("2026-08-19T12:00:00Z".to_string()),
                dormant_since: Some("2026-08-19T12:00:00Z".to_string()),
                ..Default::default()
            },
        ],
    );

    let said = stdout(&dormancy(&["list", "--state", state.to_str().unwrap()]));

    let at = |url: &str| said.find(url).unwrap_or_else(|| panic!("{url} must be listed: {said}"));
    assert!(at(B) < at(C), "dormant urls in url order: {said}");
    assert!(at(C) < at(A), "and every dormant url before the awake one: {said}");
    assert!(said.contains("strikes"), "a strike count is what says how close a relegation is");
}

// ---------------------------------------------------------------------------
// `prune`
// ---------------------------------------------------------------------------

/// An entry carrying a hold, for the fixtures that need one.
fn held(url: &str, reason: &str) -> EndpointState {
    EndpointState {
        url: url.to_string(),
        hold: Some(Hold::Dormant { reason: reason.to_string() }),
        dormant_since: Some("2026-08-20T12:00:00Z".to_string()),
        last_probed: Some("2026-08-20T12:00:00Z".to_string()),
        ..Default::default()
    }
}

/// A held entry is kept whatever the registry says, and every url dropped is
/// named on the way out.
///
/// An operator who slept an endpoint and then dropped it from the list has
/// recorded a decision; losing it means the endpoint silently returns to full
/// sweeps if the url ever comes back.
#[test]
fn prune_drops_only_unheld_entries_and_says_what_it_dropped() {
    let dir = tempdir("prune");
    let state = state_path(&dir);
    plant(
        &state,
        vec![
            // Listed and unheld: kept because it is listed.
            EndpointState { url: A.to_string(), strikes: 1, ..Default::default() },
            // Unlisted and unheld: the one case that is prunable.
            EndpointState { url: B.to_string(), strikes: 1, ..Default::default() },
            // Unlisted but held: kept because somebody decided something.
            held(C, "its admin asked, 2026-08-01"),
        ],
    );
    let endpoints = endpoints_file(&dir, &[A]);
    let exclusions = exclusions_file(&dir, &[]);

    let out = dormancy(&[
        "prune",
        "--endpoints", endpoints.to_str().unwrap(),
        "--exclusions", exclusions.to_str().unwrap(),
        "--state", state.to_str().unwrap(),
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains(B), "the dropped url must be printed: {said}");
    assert!(!said.contains(C), "and a kept one must not be: {said}");

    let written = read_state(&state).unwrap();
    let kept: Vec<&str> = written.endpoint.iter().map(|e| e.url.as_str()).collect();
    assert_eq!(kept, vec![A, C], "only the unlisted unheld entry may go");
    assert_eq!(
        written.last_sweep_at.as_deref(),
        Some("2026-08-20T12:00:00Z"),
        "a prune must not reset the slice's divisor"
    );
}

/// `--dry-run` prints and writes nothing, byte for byte.
#[test]
fn prune_dry_run_writes_nothing() {
    let dir = tempdir("prune-dry");
    let state = state_path(&dir);
    plant(
        &state,
        vec![
            EndpointState { url: A.to_string(), strikes: 1, ..Default::default() },
            EndpointState { url: B.to_string(), strikes: 1, ..Default::default() },
        ],
    );
    let before = std::fs::read(&state).unwrap();
    let endpoints = endpoints_file(&dir, &[A]);
    let exclusions = exclusions_file(&dir, &[]);

    let out = dormancy(&[
        "prune",
        "--endpoints", endpoints.to_str().unwrap(),
        "--exclusions", exclusions.to_str().unwrap(),
        "--state", state.to_str().unwrap(),
        "--dry-run",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains(B), "a dry run still says what it would drop");
    assert_eq!(std::fs::read(&state).unwrap(), before, "and writes nothing at all");
    assert!(
        !lock_path(&state).exists(),
        "nor may it leave a lock behind for the next sweep to trip over"
    );
}

/// An excluded url is not in what `load_endpoints` returns, so an excluded
/// entry with no hold IS prunable and an excluded entry WITH a hold is not.
///
/// This is the question `--exclusions` decides, and it is why the flag is here
/// rather than left to a default: run without the list, every excluded url
/// looks listed, and the state keeps entries for hosts this project has been
/// asked to leave alone. Run with it, the hold is the only thing that saves
/// such an entry, which is the same rule as everywhere else: a decision a
/// person took outranks the registry in both directions.
#[test]
fn prune_asks_the_exclusion_list_and_a_hold_still_outranks_it() {
    let dir = tempdir("prune-excluded");
    let state = state_path(&dir);
    plant(
        &state,
        vec![
            EndpointState { url: A.to_string(), strikes: 1, ..Default::default() },
            held(B, "asked to be left alone, 2026-08-02"),
        ],
    );
    // BOTH urls are in the endpoint list, and both hosts are excluded, so the
    // loaded list is empty and the hold is the only difference between them.
    let endpoints = endpoints_file(&dir, &[A, B]);
    let exclusions = exclusions_file(&dir, &["alpha.sparqlwatch-fixture.org", "beta.sparqlwatch-fixture.org"]);

    let out = dormancy(&[
        "prune",
        "--endpoints", endpoints.to_str().unwrap(),
        "--exclusions", exclusions.to_str().unwrap(),
        "--state", state.to_str().unwrap(),
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    let kept: Vec<String> =
        read_state(&state).unwrap().endpoint.iter().map(|e| e.url.clone()).collect();
    assert_eq!(kept, vec![B.to_string()], "the excluded unheld entry goes, the held one stays");
}

// ---------------------------------------------------------------------------
// The lock
// ---------------------------------------------------------------------------

/// Every mutation goes through `merge_state`, so none can race a sweep.
///
/// The race is concrete: a sweep reads state at 19:45 and finishes at 21:11,
/// and an operator's `sleep` at 20:10 must not be destroyed by the sweep's
/// write-back nor destroy the sweep's strikes. A planted lock stands in for the
/// sweep, and a mutation that could reach the file around `merge_state` would
/// not notice it.
///
/// **`init` is deliberately not in the list, and its absence is not an
/// oversight.** It reads nothing before it writes, so it has no snapshot to
/// lose, which is the only thing the lock protects. Its exclusion is `O_EXCL`
/// through `create_new`, which is stronger than the lock for the one race an
/// init can lose, and `init_refuses_a_state_that_already_exists_and_changes_nothing`
/// is where that is asserted. Adding it here would assert the opposite of what
/// `state_file::init_state` documents.
#[test]
fn every_mutation_goes_through_the_lock_so_it_cannot_race_a_sweep() {
    let dir = tempdir("lock");
    let state = state_path(&dir);
    plant(&state, vec![EndpointState { url: A.to_string(), strikes: 1, ..Default::default() }]);
    let before = std::fs::read(&state).unwrap();
    let endpoints = endpoints_file(&dir, &[]);
    let exclusions = exclusions_file(&dir, &[]);
    let lock = lock_path(&state);
    std::fs::write(
        &lock,
        "sparqlwatch dormancy state lock\npid: 4242\ncommand: prober --at 2026-08-26T19:45:00Z\n",
    )
    .unwrap();
    let state_arg = state.to_str().unwrap();

    let mutations: Vec<Vec<&str>> = vec![
        vec!["wake", A, "--reason", "because", "--state", state_arg, "--at", AT],
        vec!["sleep", A, "--reason", "because", "--state", state_arg, "--at", AT],
        vec![
            "prune",
            "--endpoints", endpoints.to_str().unwrap(),
            "--exclusions", exclusions.to_str().unwrap(),
            "--state", state_arg,
        ],
    ];
    for args in &mutations {
        let out = dormancy(args);
        assert!(!out.status.success(), "{args:?} must refuse while the state is locked");
        let said = stderr(&out);
        assert!(said.contains("locked"), "the refusal must say the file is locked: {said}");
        assert!(said.contains("4242"), "and attribute the lock it found: {said}");
        assert_eq!(std::fs::read(&state).unwrap(), before, "{args:?} must have written nothing");
        assert!(lock.exists(), "{args:?} must not remove a lock it does not hold");
    }

    // And `list`, which writes nothing, is readable through a lock: an operator
    // whose sweep is mid-flight still needs to see what the state says.
    let listed = dormancy(&["list", "--state", state_arg]);
    assert!(listed.status.success(), "list must not need the lock: {}", stderr(&listed));
}
