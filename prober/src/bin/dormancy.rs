//! The operator's override: wake an endpoint the machine put to sleep, sleep
//! one deliberately, see what the state says, bootstrap the file, and drop
//! entries for endpoints that have left the registry.
//!
//! `dormancy.rs` holds the policy and touches no disk; `state_file.rs` holds
//! the lock and the atomic replace and is the only code that opens the file.
//! This binary is the shell around both: it turns five subcommands into one
//! call each and prints what happened. Nothing here decides anything, which is
//! why it is short, and every rule it appears to enforce is really enforced one
//! layer down where a unit test can reach it.
//!
//! **A third binary and not a flag on the prober.** The reason
//! `prober/Cargo.toml` gives for `seed-registry` is the reason here too: a
//! plain sweep must not be one flag away from mutating decisions a person took.
//! A `--wake` on the sweeper would sit in the same argv as a cron job's, and the
//! first accident would be a scheduled run that quietly lifted a hold.
//!
//! **Every mutation that reads before it writes goes through
//! `state_file::merge_state`**, so none of them can race a sweep. The race is
//! concrete: a sweep reads the state at 19:45 and finishes at 21:11, and a
//! `sleep` at 20:10 sits inside that window. What makes it safe is that
//! `merge_state` takes the lock, re-reads the file INSIDE it, and applies the
//! closure to what is on disk at that moment, so this binary never holds a
//! `State` it might write back. There is no `--force`: a refusal that says which
//! process holds the lock is the whole answer, and an override would be a way to
//! lose a sweep's ninety minutes of strikes.
//!
//! `init` is the one write that does NOT take the lock, and it is not an
//! exception to the rule so much as the one case the rule is not about. It
//! reads nothing, so there is no snapshot to lose: it either creates the file or
//! finds one there. Its exclusion is `O_EXCL`, which is stronger than the lock
//! for the one race an init can lose, and taking a lock as well would only add
//! a file to clean up on the path that refuses. `state_file::init_state`
//! documents that, and it is why `init` is absent from
//! `dormancy_cli.rs`'s `every_mutation_goes_through_the_lock_so_it_cannot_race_a_sweep`.
//!
//! **`list` is the one subcommand that takes no lock**, because it writes
//! nothing and an operator whose sweep is mid-flight is exactly the operator
//! who needs to see the state. It also reads no clock, so it prints a hold's
//! expiry rather than judging whether the grace has run out; the field that
//! records a grace the machine has ALREADY taken back is `lapsed_hold`, and it
//! is printed too.
//!
//! **Nothing here reads a clock at all.** `--at` is required on both writers
//! and has no default, for the reason `main.rs` gives and one more: the lock
//! file records the process's argv, and `merge_state` is handed no instant of
//! its own, so argv is the only place a run's identity is available to the
//! message a later run prints about a stale lock. A default would leave that
//! message with nothing to name.

use clap::{Parser, Subcommand};
use sparqlwatch_prober::dormancy::{self, EndpointState, Hold, State, Thresholds};
use sparqlwatch_prober::registry;
use sparqlwatch_prober::state_file::{self, DEFAULT_STATE};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "dormancy",
    about = "Overrule the dormancy policy: wake, sleep, inspect, bootstrap, prune"
)]
struct Args {
    /// The dormancy state to read and write. Global, so it may be given before
    /// or after the subcommand, and defaulted to the same path the prober
    /// defaults to: two tools with two defaults for one file is a way to write
    /// a hold into a file nothing reads.
    ///
    /// A path relative to the WORKING DIRECTORY, like `--endpoints` and
    /// `--exclusions`. Run from anywhere but `prober/` and this default names a
    /// file that is not there, and the error says which path it tried.
    #[arg(long, global = true, default_value = DEFAULT_STATE)]
    state: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write an empty state file, refusing one that already exists.
    ///
    /// The ONLY thing that creates the file, and a fresh checkout needs it once
    /// before it can sweep at all: `prober/state/` is git-ignored, and a sweep
    /// that created its own state could not tell "first ever run" from "the
    /// volume holding the state did not get mounted".
    Init,
    /// Print what the state says, dormant endpoints first.
    List,
    /// Put an endpoint back in every sweep, with a grace period during which
    /// the machine may not relegate it again.
    Wake {
        /// The endpoint url, exactly as `endpoints.toml` spells it.
        ///
        /// Leading and trailing whitespace is TRIMMED, not refused, and nothing
        /// else about the string is checked: only a url that is blank once
        /// trimmed is refused, because it is about no endpoint at all. So this
        /// is not the flag that catches a mis-paste. A url differing from the
        /// registry's by anything but surrounding whitespace is accepted and
        /// written, and it then compares equal to nothing the sweep probes: the
        /// file says the endpoint is held while every sweep probes it. What
        /// catches that is reading back what was written, which is why this
        /// command prints the entry it left rather than reporting success.
        url: String,
        /// Why. Required and refused when blank, on the rule
        /// `registry/exclusions.toml` sets for its own entries: a decision that
        /// cannot say why it was taken is one nobody can review in a year.
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        reason: String,
        /// Never lapse. The hold gets no expiry, so the machine cannot take
        /// this endpoint back on its own and only another `dormancy` command
        /// changes it.
        #[arg(long)]
        pin: bool,
        /// How long the grace lasts, in whole days. Refused together with
        /// `--pin`, which is the wake that has no expiry at all: a pair of flags
        /// that contradict each other is one this command refuses rather than
        /// resolves quietly, because silently discarding the number an operator
        /// typed leaves them believing a grace was set. clap does not count a
        /// defaulted value as present, so the conflict fires only when both are
        /// really typed.
        ///
        /// The flag lives here and nowhere else because THIS is the only caller
        /// of `dormancy::wake`, which is the only code that reads the number:
        /// the expiry is computed once, at wake time, and stored in the file as
        /// an instant. A `--dormant-grace-days` on the sweeper could be parsed
        /// and threaded through the whole policy without changing anything, so
        /// it was removed rather than left to mislead.
        #[arg(long, default_value_t = DEFAULT_GRACE, conflicts_with = "pin")]
        grace_days: NonZeroU64,
        /// The instant this decision was taken, exactly `YYYY-MM-DDTHH:MM:SSZ`.
        /// Required and never read from the clock. See the module header.
        #[arg(long)]
        at: String,
    },
    /// Take an endpoint out of every sweep, permanently until a person wakes
    /// it, because an operator may sleep an endpoint for reasons no verdict
    /// expresses: a request from its admin, a cost nobody wants to pay, a
    /// machine that is being migrated.
    Sleep {
        /// The endpoint url, exactly as `endpoints.toml` spells it. Trimmed and
        /// not otherwise checked, as on `wake`: only a blank url is refused, and
        /// the entry this command prints is how a mis-paste is caught.
        url: String,
        /// Why. Required and refused when blank, on the rule
        /// `registry/exclusions.toml` sets for its own entries: a decision that
        /// cannot say why it was taken is one nobody can review in a year. It
        /// matters most here, because this is the direction that takes an
        /// endpoint out of every sweep until a person puts it back, and the
        /// reason is all the next operator has to decide whether that is safe.
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        reason: String,
        /// The instant this decision was taken, exactly `YYYY-MM-DDTHH:MM:SSZ`:
        /// UTC, no offset, no fractional second, no leap second. Required and
        /// never read from the clock. It becomes this endpoint's
        /// `dormant_since` if it has none, which the run graphs then publish,
        /// so it is a decision and not a side effect of when the terminal was
        /// free. See the module header for the other reason it has no default.
        #[arg(long)]
        at: String,
    },
    /// Drop the entries for endpoints the registry no longer lists.
    ///
    /// A held entry is KEPT whatever the registry says, in both directions: an
    /// operator who slept an endpoint and then dropped it from the list has
    /// recorded a decision, and losing it means the endpoint silently returns
    /// to full sweeps if the url ever comes back.
    Prune {
        /// The endpoint list to compare against. Read through
        /// `registry::load_endpoints`, so what counts as listed here is exactly
        /// what a sweep would probe.
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        endpoints: String,
        /// The hosts somebody asked this project not to probe. Needed because
        /// `load_endpoints` subtracts them, so this flag decides the answer for
        /// an excluded entry: excluded and unheld is prunable, because a sweep
        /// would never probe it again; excluded and HELD is kept, because a
        /// person's decision outranks the registry either way. Same default as
        /// the prober's, and a list that cannot be read stops the command.
        #[arg(long, default_value = registry::DEFAULT_EXCLUSIONS)]
        exclusions: String,
        /// Print what would be dropped and write nothing. No lock is taken, so
        /// this is safe to run against a state a sweep is part-way through
        /// writing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// The default grace, taken from the policy module rather than restated, so the
/// number this flag offers and the number the tests calibrate against cannot
/// drift apart. Unwrap on a literal const: unreachable unless somebody edits
/// `DEFAULT_GRACE_DAYS` to zero.
const DEFAULT_GRACE: NonZeroU64 =
    match NonZeroU64::new(sparqlwatch_prober::dormancy::DEFAULT_GRACE_DAYS) {
        Some(days) => days,
        None => panic!("DEFAULT_GRACE_DAYS is a grace period and zero days is not one"),
    };

fn main() -> anyhow::Result<()> {
    // So `load_endpoints`'s warnings about dropped entries reach the operator
    // running a prune: they say which urls the comparison did not include, and
    // that is what makes a surprising drop list readable.
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let state = PathBuf::from(&args.state);
    match args.command {
        Command::Init => init(&state),
        Command::List => list(&state),
        Command::Wake { url, reason, pin, grace_days, at } => {
            wake(&state, &url, &reason, &at, pin, grace_days)
        }
        Command::Sleep { url, reason, at } => sleep(&state, &url, &reason, &at),
        Command::Prune { endpoints, exclusions, dry_run } => {
            prune(&state, &endpoints, &exclusions, dry_run)
        }
    }
}

fn init(path: &Path) -> anyhow::Result<()> {
    state_file::init_state(path)?;
    println!(
        "wrote an empty dormancy state at {}. Nothing is dormant and nobody holds anything, \
         so the next sweep probes every endpoint the registry lists.",
        path.display()
    );
    Ok(())
}

/// Put an endpoint back in every sweep.
///
/// The dry run against an EMPTY state before the real merge is the argument
/// check, and it is deliberately not a second copy of the rules. `--at`'s
/// grammar lives in `dormancy::parse_instant` and `--reason`'s non-blankness in
/// `dormancy::checked_reason`, both private to that module and both reached by
/// `dormancy::wake` before it touches anything; `main.rs`'s `validate_instant`
/// is private to the sweeper. Rather than write a third copy of the grammar,
/// this applies the very function that will apply the arguments and throws the
/// result away.
///
/// What that buys is the ORDER. Without it the first thing to fail on a bad
/// `--at` would be `merge_state`, which takes the lock and reads the state
/// first, so a fresh deployment would hear "run dormancy init" about a `--at`
/// it typed wrong: a true sentence about the wrong problem. It costs one clone
/// of an empty state, and it cannot drift from the real check because it IS the
/// real check.
fn wake(
    path: &Path,
    url: &str,
    reason: &str,
    at: &str,
    pin: bool,
    grace_days: NonZeroU64,
) -> anyhow::Result<()> {
    let thresholds = Thresholds { grace_days, ..Thresholds::default() };
    dormancy::wake(&State::empty(), url, reason, at, pin, &thresholds)?;
    let written = state_file::merge_state(path, |on_disk| {
        dormancy::wake(on_disk, url, reason, at, pin, &thresholds)
    })?;
    report(path, &written, url);
    Ok(())
}

/// Take an endpoint out of every sweep. Same dry-run precheck as `wake`, for
/// the same reason.
fn sleep(path: &Path, url: &str, reason: &str, at: &str) -> anyhow::Result<()> {
    dormancy::sleep(&State::empty(), url, reason, at)?;
    let written =
        state_file::merge_state(path, |on_disk| dormancy::sleep(on_disk, url, reason, at))?;
    report(path, &written, url);
    Ok(())
}

/// What one mutation left behind, read back out of what was actually written
/// rather than out of the arguments.
///
/// The entry is the record, and the argument is not: `checked_url` TRIMS
/// surrounding whitespace, so an operator who pasted a url with a trailing
/// space has a hold on a string they did not type. Printing the entry is what
/// shows them the url now in the file. It is also the only check on a url that
/// differs from the registry's in some way trimming does not fix, since nothing
/// here compares the url against the endpoint list.
fn report(path: &Path, written: &State, url: &str) {
    match written.get(url.trim()) {
        Some(entry) => {
            println!("wrote {}, which now says:", path.display());
            println!("{}  {}", mark(entry), entry.url);
            for line in describe(entry) {
                println!("{line}");
            }
        }
        // Unreachable: `wake` and `sleep` both upsert the entry, so a state
        // returned by either holds it. Reported rather than unwrapped, because
        // a panic in the one tool an operator reaches for at 2 a.m. is a worse
        // answer than a sentence.
        None => println!(
            "{} was written, but it holds no entry for {url}, which should be impossible: \
             read the file before relying on this command",
            path.display()
        ),
    }
}

/// Print what the state says.
///
/// Dormant first, then by url. The dormant ones are what an operator opened
/// this for and the awake ones are the context; inside each group the url
/// order is the file's own, so a listing and a `git diff` of the file read the
/// same way.
fn list(path: &Path) -> anyhow::Result<()> {
    let state = state_file::read_state(path)?;
    if state.endpoint.is_empty() {
        // One line, and it names the file. An empty state prints nothing at all
        // otherwise, which reads like a command that failed, and the commonest
        // cause of a genuinely empty state is a `--state` pointing somewhere
        // nobody has swept.
        println!(
            "the dormancy state at {} is empty: no endpoint is dormant and nobody has placed \
             a hold, so every endpoint the registry lists is in every sweep.",
            path.display()
        );
        return Ok(());
    }

    let dormant = state.endpoint.iter().filter(|entry| entry.dormant_since.is_some()).count();
    println!("state:         {}", path.display());
    println!("version:       {}", state.version);
    println!("updated_at:    {}", or_none(&state.updated_at));
    // Printed beside `updated_at` and not instead of it. The two are equal
    // after every sweep and differ only between a sweep and an operator
    // command, and this is the one that predicts the next sweep's rotation:
    // it is the divisor of the cadence slice, so an operator asking why nine of
    // fifty-seven dormant endpoints were probed is asking about this field.
    println!("last_sweep_at: {}", or_none(&state.last_sweep_at));
    println!("endpoints:     {}, of which {dormant} dormant", state.endpoint.len());
    println!();

    let mut entries: Vec<&EndpointState> = state.endpoint.iter().collect();
    entries.sort_by_key(|entry| (entry.dormant_since.is_none(), entry.url.clone()));
    for entry in entries {
        println!("{}  {}", mark(entry), entry.url);
        for line in describe(entry) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Whether this endpoint is in the rotation, in a word and in a fixed width so
/// a listing of fifty-seven lines up.
///
/// `dormant_since` and not the hold, because that field is what both a machine
/// relegation and an operator's `sleep` set, and it is what the run graphs
/// publish. A live `Awake` hold clears it, so a woken endpoint reads as awake
/// here on the same instant it re-enters the sweep.
fn mark(entry: &EndpointState) -> &'static str {
    match entry.dormant_since.is_some() {
        true => "dormant",
        false => "awake  ",
    }
}

/// The indented detail lines for one entry, shared by `list` and by the report
/// one mutation prints, so the two cannot describe the same entry differently.
///
/// Every `Option` is genuinely absent for a real endpoint: a url the registry
/// lists that no sweep has reached has no `last_probed`, and an endpoint nobody
/// has held has no hold. So each one is either printed or replaced by the
/// sentence that says it is missing, never left off silently.
fn describe(entry: &EndpointState) -> Vec<String> {
    let mut lines = Vec::new();
    let probed = match (&entry.last_probed, entry.last_cost_ms) {
        (Some(at), Some(cost)) => format!("last probed {at}, costing {cost} ms"),
        (Some(at), None) => format!("last probed {at}"),
        (None, _) => "never probed".to_string(),
    };
    lines.push(format!("    strikes {}, {probed}", entry.strikes));
    if let Some(since) = &entry.dormant_since {
        lines.push(format!("    dormant since {since}"));
    }
    match &entry.hold {
        // No expiry is printed as a judgement about the endpoint's future, not
        // as a missing field: a pin is the wake that only another command can
        // undo.
        Some(Hold::Awake { reason, until: None }) => {
            lines.push(format!("    hold: awake, pinned, no expiry, reason {reason:?}"));
        }
        // The expiry, not whether it has passed. This command reads no clock,
        // so it cannot say; `lapsed_hold` below is the record of a grace the
        // machine has already taken back.
        Some(Hold::Awake { reason, until: Some(until) }) => {
            lines.push(format!("    hold: awake until {until}, reason {reason:?}"));
        }
        Some(Hold::Dormant { reason }) => {
            lines.push(format!(
                "    hold: dormant until a person wakes it, reason {reason:?}"
            ));
        }
        None => {}
    }
    if let Some(reason) = &entry.lapsed_hold {
        // The only thing that can answer "why was this endpoint in last week's
        // sweep and not in this one".
        lines.push(format!("    lapsed hold, reason {reason:?}"));
    }
    lines
}

fn or_none(instant: &Option<String>) -> &str {
    instant.as_deref().unwrap_or("(never written)")
}

/// Drop the entries for endpoints the registry no longer lists.
///
/// The endpoint list and the exclusion list are read BEFORE the lock is taken,
/// so a missing or malformed one refuses without leaving a lock file behind for
/// the next sweep to trip over.
///
/// `dormancy::prunable` returns its dropped list ALONGSIDE the new state, and
/// `merge_state`'s closure is typed to return only a `State`, so the list comes
/// back out through a `&mut` binding the closure captures. That is deliberate
/// rather than awkward: the alternative is a closure that returns a pair and a
/// `merge_state` generic over what it carries, and the reason the closure shape
/// is fixed is that it must be impossible to hand `merge_state` a `State` read
/// earlier. One captured binding is a smaller price than loosening that.
fn prune(
    path: &Path,
    endpoints_path: &str,
    exclusions_path: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let excluded = registry::read_exclusions(Path::new(exclusions_path))?;
    let text = std::fs::read_to_string(endpoints_path).map_err(|error| {
        anyhow::anyhow!(
            "the endpoint list at {endpoints_path} could not be read ({error}), and a prune \
             compares the state against it: read as empty it would drop every entry nobody \
             is holding. A relative path is resolved against the working directory."
        )
    })?;
    let endpoints = registry::load_endpoints(&text, &excluded)?;
    if endpoints.is_empty() {
        // A warning and not a refusal. An empty list legitimately means every
        // unheld entry is stale, which is a prune's whole job; what it more
        // often means is a `--endpoints` or `--exclusions` pointing at the
        // wrong file, and the operator has to be told which one this was.
        tracing::warn!(
            endpoints = %endpoints_path,
            exclusions = %exclusions_path,
            "the endpoint list loaded as empty, so every entry without a hold is prunable: \
             check that these are the files you meant, and use --dry-run first"
        );
    }

    if dry_run {
        let (dropped, _) = dormancy::prunable(&state_file::read_state(path)?, &endpoints);
        announce(path, &dropped, true);
        return Ok(());
    }
    let mut dropped: Vec<String> = Vec::new();
    state_file::merge_state(path, |on_disk| {
        let (pruned, kept) = dormancy::prunable(on_disk, &endpoints);
        dropped = pruned;
        Ok(kept)
    })?;
    announce(path, &dropped, false);
    Ok(())
}

/// Every url a prune dropped, or would have. Named one per line, because this
/// is the only record of the drop: the entries are gone from the file and the
/// run graphs never carried them.
fn announce(path: &Path, dropped: &[String], dry_run: bool) {
    let verb = match dry_run {
        true => "would drop",
        false => "dropped",
    };
    if dropped.is_empty() {
        println!(
            "nothing to prune in {}: every entry names an endpoint the registry still lists, \
             or carries a hold.",
            path.display()
        );
        return;
    }
    let plural = match dropped.len() {
        1 => "entry",
        _ => "entries",
    };
    println!("{verb} {} {plural} from {}:", dropped.len(), path.display());
    for url in dropped {
        println!("    {url}");
    }
    // Said last, after the list, because that is the order the reader needs it
    // in: the urls are what they came for and this is the reassurance about
    // them, not a preamble to them.
    if dry_run {
        println!("Nothing was written.");
    }
}
