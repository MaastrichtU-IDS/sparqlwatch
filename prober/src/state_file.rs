//! The dormancy state on disk: reading it, replacing it, and the one lock that
//! keeps two writers from losing each other's work.
//!
//! `dormancy.rs` is pure policy and opens no file. This module is the ONLY code
//! that touches the state file, so every rule about how it is read and written
//! lives here: fail closed on a file that is missing or corrupt, re-read inside
//! the lock before applying any policy closure, and write through a partial
//! file that is renamed into place.
//!
//! The shape of every write is `merge_state(path, closure)`, where the closure
//! is one of `dormancy::update`, `wake`, `sleep` or a prune. There is
//! deliberately no `write_state` in the public surface: a caller holding a
//! `State` it read some minutes ago cannot be given a way to put it back,
//! because that is the lost update this module exists to prevent.

use crate::dormancy::State;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where the dormancy state lives, as the default for every binary's `--state`.
///
/// A path relative to the WORKING DIRECTORY, like `--endpoints`'s
/// `endpoints.toml` and `--exclusions`'s `registry/exclusions.toml`, so the
/// tool keeps one convention rather than growing a second one for this file.
/// The trap that comes with that is the same one `registry::DEFAULT_EXCLUSIONS`
/// documents: run from anywhere but `prober/`, this default names a file that
/// is not there, and `read_state` then fails and says which path it tried.
///
/// A directory of its own rather than `dormancy.toml` beside the registry,
/// because this is the one file here that a sweep WRITES. `prober/.gitignore`
/// ignores `state/`, so machine-written state cannot arrive in a commit by
/// accident, and an operator asked to mount a volume has one directory to
/// mount rather than one file to remember.
pub const DEFAULT_STATE: &str = "state/dormancy.toml";

/// The lock is a sibling of the state file, and every error about it names it.
pub const LOCK_SUFFIX: &str = ".lock";

/// The partial file a merge writes before renaming it onto the state file, also
/// a sibling. Not public: nothing outside this module has a reason to name it,
/// and unlike `write.rs`'s partial run file it never holds anything an operator
/// would want to keep.
const PARTIAL_SUFFIX: &str = ".partial";

/// A path with `suffix` appended to its FILE NAME, so the result is a sibling of
/// `path` and lands in the same directory and so on the same filesystem. That
/// last part is what makes the rename in `write_state` atomic. Same idiom as
/// `write::partial_path`, and it works on a path with no extension and on one
/// with several.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The lock beside the state file at `path`. Public because the `dormancy`
/// binary's messages and an operator's `rm` both need to name it.
pub fn lock_path(path: &Path) -> PathBuf {
    sibling(path, LOCK_SUFFIX)
}

/// The partial file beside the state file at `path`. Private, like the suffix
/// it appends and for the same reason: nothing outside this module names it,
/// because a leftover partial is truncated by the next merge rather than
/// reported to anybody.
fn partial_path(path: &Path) -> PathBuf {
    sibling(path, PARTIAL_SUFFIX)
}

/// The dormancy state at `path`, or an error naming the path.
///
/// **Fails closed**, and that is the property the whole policy rests on. A
/// state file that cannot be read is not an empty state: it is a process that
/// does not know which endpoints were relegated or which ones an operator held.
/// Read as empty, this sweep would re-admit all 57 relegated endpoints at
/// roughly 210 s each, which is over three hours of probing nobody asked for,
/// and it would forget every hold placed by hand. `registry::read_exclusions`
/// fails closed for the neighbouring reason and this follows it.
///
/// It re-validates nothing. `State::parse` already refuses an unknown version,
/// a malformed instant naming its field, an unknown field, a bad url and a
/// blank hold reason; this function reads bytes, hands them to `parse`, and
/// wraps whatever comes back with the path, because a message about a field is
/// no use to an operator who does not know which file it is in.
///
/// The message names the path that was tried and says that a relative path is
/// resolved against the working directory, because those are the two mistakes
/// this shape of default invites and an operator cannot fix what the error does
/// not name.
///
/// A MISSING file gets one sentence more, naming `dormancy init`: without it a
/// fresh deployment reads like a broken one. A file that exists and cannot be
/// read gets a different sentence, and the difference matters. `init_state`
/// refuses a file that exists, so offering it for a directory in the way, a
/// mode 000 file or bytes that are not UTF-8 would send the operator to a
/// command whose refusal says "already exists" and says nothing at all about
/// what is actually there.
pub fn read_state(path: &Path) -> anyhow::Result<State> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        // The half of the message that holds whatever went wrong: what a failed
        // read costs, and the working-directory rule.
        let common = format!(
            "the dormancy state at {} could not be read ({error}), so nothing here knows \
             which endpoints were relegated or which ones an operator held, and treating it \
             as an empty state would re-probe every relegated endpoint and discard every \
             hold. A relative path is resolved against the working directory: pass --state an \
             absolute path, or run from the directory holding {}.",
            path.display(),
            DEFAULT_STATE
        );
        // Branched on the kind, the way `init_state` branches on it below.
        match error.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!(
                "{common} On a deployment that has never had a state file, `dormancy init \
                 --state {}` writes an empty one, and that is the only thing that may \
                 create it.",
                path.display()
            ),
            _ => anyhow::anyhow!(
                "{common} Something IS at that path, so this is not a deployment waiting \
                 to be initialised: check what is there, whether it is a file at all, and \
                 what this process is allowed to read."
            ),
        }
    })?;
    State::parse(&text)
        .map_err(|error| anyhow::anyhow!("the dormancy state at {}: {error}", path.display()))
}

/// Write an empty state file at `path`, refusing to overwrite one that exists.
///
/// The bootstrap `read_state`'s error names, and the ONLY thing that creates
/// the file. A sweep does not create it, because a sweep that creates its own
/// state file cannot tell "first ever run" from "the volume holding the state
/// did not get mounted", and the second of those is the case that quietly
/// re-admits every relegated endpoint.
///
/// The parent directory is created, because `state/` is git-ignored and so does
/// not exist in a fresh checkout, and an init that fails on that would need a
/// `mkdir` nobody documented.
///
/// `create_new`, so an existing file is REFUSED and not truncated: it holds
/// strike counts a sweep spent 90 minutes measuring and holds a person placed
/// by hand, and none of that is this command's to discard. No lock is taken
/// around it, and none is needed: `create_new` is itself the exclusion for the
/// one race an init can lose, and a lock would only add a file to clean up on
/// the path that refuses.
pub fn init_state(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| {
                anyhow::anyhow!(
                    "cannot create the directory {} for the dormancy state: {error}",
                    parent.display()
                )
            })?;
        }
    }
    let body = State::empty().render();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(
                "the dormancy state at {} already exists, and init writes an empty one, so \
                 this would discard every strike the sweeps have measured and every hold an \
                 operator placed. Nothing was written. If the file is genuinely to be \
                 rebuilt from nothing, move it aside first and keep it until the next sweep \
                 has run.",
                path.display()
            ),
            _ => anyhow::anyhow!(
                "cannot create the dormancy state at {}: {error}. A relative path is \
                 resolved against the working directory: pass --state an absolute path, or \
                 run from the directory that is to hold {}",
                path.display(),
                DEFAULT_STATE
            ),
        })?;
    file.write_all(body.as_bytes())
        .map_err(|error| anyhow::anyhow!("cannot write {}: {error}", path.display()))?;
    // Durable before the command reports success, so a machine that loses power
    // right after `dormancy init` returns does not come back up with a
    // zero-length state file that every later run refuses to parse.
    file.sync_all()
        .map_err(|error| anyhow::anyhow!("cannot flush {} to disk: {error}", path.display()))?;
    Ok(())
}

/// Take the lock and give it straight back, so a stale one is found at startup.
///
/// Called beside `read_state`, before any probing. An `O_EXCL` lock never
/// blocks, so a lock left behind is not a deadlock, but it is also not
/// something the process recovers from on its own: without this check a prober
/// killed mid-merge would let the next sweep probe 543 endpoints for 90
/// minutes and only then discover that it cannot write, losing every strike to
/// an error that says "remove it by hand". The same error at t=0 costs a rerun.
///
/// It is not a reservation. Between this check and the merge at the end of the
/// sweep another writer may take the lock, and that is fine: `merge_state`
/// refuses rather than overwriting, which is the outcome this check exists to
/// make rare rather than to make impossible. Holding the lock for 90 minutes
/// instead would lock out every operator command for the length of a sweep.
pub fn check_lock(path: &Path) -> anyhow::Result<()> {
    Lock::take(path)?.release()
}

/// Apply `f` to the state on disk and replace the file with the result,
/// returning what was written.
///
/// The closure is `dormancy::update`, `wake`, `sleep` or a prune. What matters
/// is the order: the lock is taken FIRST, the state is read INSIDE it, and the
/// closure sees what is on disk at that moment rather than a snapshot the
/// caller read earlier. The race is concrete and it is the reason this function
/// takes a closure rather than a `State`: a sweep reads state at 19:45 and
/// finishes at 21:11, an operator runs `dormancy sleep` at 20:10, and a
/// write-back of the 19:45 snapshot destroys that hold silently. A closure that
/// needs facts from the sweep closes over those facts; it must not close over a
/// `State`.
///
/// A failing closure writes nothing and releases the lock. The write itself
/// goes to `<path>.partial` and is renamed inside the lock, so a reader never
/// sees a half-written state file and a crash mid-write leaves the previous
/// state intact.
pub fn merge_state<F>(path: &Path, f: F) -> anyhow::Result<State>
where
    F: FnOnce(&State) -> anyhow::Result<State>,
{
    let lock = Lock::take(path)?;
    // Inside the lock, and not hoisted above it however tempting the symmetry
    // with `check_lock` looks: a read before the lock is exactly the snapshot
    // whose write-back this function exists to prevent.
    let current = read_state(path)?;
    // Every `?` from here on returns through the guard's `Drop`, which releases
    // the lock. That includes a closure that fails, which is the common case:
    // `update` refuses two outcomes for one url, and refusing must not leave
    // the file locked against the operator who has to fix it.
    let next = f(&current)?;
    write_state(path, &next)?;
    // Explicit, so a failure to remove the lock is REPORTED rather than
    // swallowed by a destructor: a lock nobody can remove stops the next sweep,
    // and the operator needs to hear about it from the run that left it.
    lock.release()?;
    Ok(next)
}

/// Render `state` into `<path>.partial` and rename it onto `path`.
///
/// Called only with the lock held, which is what makes the partial file safe to
/// truncate. `write.rs` uses `create_new` for its partial and refuses one that
/// exists, because that file may hold 500 endpoints of a crashed run; this
/// partial holds a state the caller has just recomputed and that the next sweep
/// would recompute again, so refusing forever on a leftover would turn a crash
/// into an outage that needs a hand-run `rm`. Truncating is right here and
/// `create_new` is right there, and the difference is what the partial is worth.
fn write_state(path: &Path, state: &State) -> anyhow::Result<()> {
    let partial = partial_path(path);
    let body = state.render();
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(body.as_bytes())?;
        // What this buys and what it does not, because the difference is easy
        // to overstate. It buys the partial file's BYTES: after it returns, the
        // content the rename is about to publish is on the disk and not only in
        // the page cache. It does not buy the DIRECTORY ENTRY the rename then
        // creates, which would need the parent directory fsynced too, and that
        // is deliberately not done here. So a power cut in the wrong
        // microsecond costs this sweep's strikes and nothing more: the previous
        // state file survives whole, and the recovery is a re-run of the same
        // --at, which the policy makes exact rather than approximate.
        file.sync_all()
    })();
    if let Err(error) = written {
        // Best effort: the error being reported is the one that matters, and a
        // partial left behind is truncated by the next merge rather than
        // refused by it.
        let _ = std::fs::remove_file(&partial);
        return Err(anyhow::anyhow!("cannot write {}: {error}", partial.display()));
    }
    std::fs::rename(&partial, path).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        anyhow::anyhow!(
            "cannot rename {} onto {}: {error}. The dormancy state is unchanged, so this \
             sweep's strikes are lost but nothing recorded earlier is",
            partial.display(),
            path.display()
        )
    })
}

/// The exclusive lock on one state file, held for the length of a merge.
///
/// An `O_EXCL` create, so taking it never blocks and never waits: a second
/// writer is refused with a message naming the first, which is the right answer
/// for a tool whose writers are one nightly sweep and a human at a terminal.
///
/// The guard is DISARMABLE, and that is not a nicety. The obvious alternative,
/// remove the lock explicitly at the end of the merge and let `Drop` cover the
/// error paths, is a double release: this process removes the lock, another
/// writer creates it, this process's `Drop` removes THAT one, and two writers
/// then hold the lock at once, which is precisely the lost update `merge_state`
/// exists to prevent. So `release` clears `armed` and `Drop` does nothing
/// unless the guard is still armed.
///
/// The guard is also constructed only AFTER `create_new` succeeds. A guard
/// built before the attempt, or built on the refusal path, would remove a lock
/// this process does not hold as soon as it went out of scope, which is the
/// same double release arriving one step earlier.
struct Lock {
    path: PathBuf,
    /// Whether this guard still owns the lock file. `false` after `release`,
    /// which is the whole mechanism: `Drop` reads this and nothing else.
    armed: bool,
}

impl Lock {
    /// Create the lock beside the state file at `path`, or report who holds it.
    fn take(path: &Path) -> anyhow::Result<Lock> {
        let lock = lock_path(path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => held(path, &lock),
                _ => anyhow::anyhow!(
                    "cannot create the lock {} for the dormancy state: {error}. The state \
                     file's directory has to exist and be writable; `dormancy init --state \
                     {}` creates it.",
                    lock.display(),
                    path.display()
                ),
            })?;
        // The guard exists from here on, so every path below releases the lock.
        let guard = Lock { path: lock, armed: true };
        // Written after the guard, so a body that cannot be written still
        // releases the lock rather than leaving one nobody can attribute. A
        // failure here is reported rather than ignored: an empty lock body
        // makes the message a later run prints useless.
        file.write_all(lock_body(path).as_bytes()).map_err(|error| {
            anyhow::anyhow!("cannot write the lock {}: {error}", guard.path.display())
        })?;
        Ok(guard)
    }

    /// Remove the lock file and disarm the guard, so its `Drop` is a no-op.
    ///
    /// Takes `self`, so a released guard cannot be released twice and cannot be
    /// used to release a lock somebody else has since taken. The flag is
    /// cleared BEFORE the removal: if the removal fails, the error is returned
    /// and `Drop` does not retry it, because a retry that succeeded would
    /// remove whatever file is there by then, which after a caller has handled
    /// the error may be another writer's lock.
    fn release(mut self) -> anyhow::Result<()> {
        self.armed = false;
        std::fs::remove_file(&self.path).map_err(|error| {
            anyhow::anyhow!(
                "cannot remove the lock {}: {error}. The dormancy state itself is written; \
                 remove the lock by hand, or the next sweep will refuse to write.",
                self.path.display()
            )
        })
    }
}

impl Drop for Lock {
    /// Releases the lock on every path out of a merge that is not the happy one.
    ///
    /// Best effort and silent, because a destructor has nowhere to report to
    /// and the error that is unwinding past it is the one worth printing. The
    /// cost of a failure here is a stale lock, which `check_lock` finds at the
    /// start of the next run.
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What goes in the lock file, so a stale one can be attributed.
///
/// The pid and the full command line. The command line is where `--at` is: it
/// is a required argument of every binary that writes this file, and
/// `merge_state` is handed no instant of its own, so argv is the only place a
/// run's identity is available to this module. A message that can say "left by
/// pid 4242 running --at 2026-08-26T19:45:00Z" tells an operator whether the
/// process is still going and which run's work is at stake; "the file is
/// locked" tells them nothing.
///
/// Plain lines, not TOML. Nothing parses this: it is quoted verbatim into an
/// error message, and a body a human wrote by hand while debugging has to
/// survive that too.
fn lock_body(path: &Path) -> String {
    // `args_os`, not `args`: `args` PANICS on an argument that is not valid
    // Unicode, and it would do it here, after the lock file exists. The armed
    // guard makes that safe in the sense that the lock is still released while
    // the panic unwinds, but a backtrace out of the one module whose job is to
    // fail in sentences is not an outcome worth keeping. Lossy is right for a
    // string that is only ever read by a person.
    let command: Vec<String> =
        std::env::args_os().map(|arg| arg.to_string_lossy().into_owned()).collect();
    format!(
        "sparqlwatch dormancy state lock\nstate: {}\npid: {}\ncommand: {}\n",
        path.display(),
        std::process::id(),
        command.join(" ")
    )
}

/// The refusal when the lock is already held, quoting the lock body.
///
/// The body is read here and not at `take` time, so the read only happens on
/// the path that needs it. A body that cannot be read is not an error: the
/// lock's existence is the fact that matters, and a message that failed because
/// the process holding the lock released it mid-read would be a refusal
/// explaining the wrong problem.
fn held(path: &Path, lock: &Path) -> anyhow::Error {
    // Capped, because this is going into a one-line error and the file is not
    // guaranteed to be one this build wrote.
    let quoted = match std::fs::read_to_string(lock) {
        Ok(text) => {
            let flat = text.trim().replace('\n', "; ");
            match flat.char_indices().nth(400) {
                Some((cut, _)) => format!("{}...", &flat[..cut]),
                None => flat,
            }
        }
        Err(error) => format!("(the lock could not be read: {error})"),
    };
    anyhow::anyhow!(
        "the dormancy state at {} is locked by {}, so another sweep or `dormancy` command is \
         part-way through writing it, and two writers here mean one of them loses its \
         strikes or its hold. Nothing was written. The lock says: {quoted}. If that process \
         is still running, let it finish and run again. If it is not, it was killed \
         mid-write: `rm {}` and run again, which is safe because the state file itself is \
         only ever replaced by a rename.",
        path.display(),
        lock.display(),
        lock.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dormancy::{self, Hold, Outcome, Thresholds};

    /// A directory of this test's own, so the tests in this module can run in
    /// parallel over one temp directory without one's lock file being another's
    /// stale lock.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("sparqlwatch-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn refused<T>(result: anyhow::Result<T>, why: &str) -> anyhow::Error {
        match result {
            Ok(_) => panic!("{why}"),
            Err(error) => error,
        }
    }

    fn expensive(url: &str) -> Outcome {
        Outcome { url: url.to_string(), cost_ms: 210_000, positive: false }
    }

    #[test]
    fn a_missing_file_is_an_error_that_names_the_path_and_the_bootstrap() {
        let dir = scratch("missing");
        let path = dir.join("dormancy.toml");

        let error = refused(
            read_state(&path),
            "a missing state file read as empty re-admits every relegated endpoint",
        );
        let message = error.to_string();
        assert!(
            message.contains(path.to_str().unwrap()),
            "the error names the path it tried: {message}"
        );
        assert!(
            message.contains("working directory"),
            "and the one mistake this shape of default invites: {message}"
        );
        assert!(
            message.contains("dormancy init"),
            "and the command that creates the file: {message}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_file_stops_the_sweep_and_names_the_path() {
        let dir = scratch("corrupt");
        let path = dir.join("dormancy.toml");
        std::fs::write(&path, "version = 1\n[[endpoint]]\nurl = \"http://a.example/s\"\nstrikes = \"two\"\n")
            .unwrap();

        let error = refused(read_state(&path), "a state file that does not parse is not an empty one");
        let message = error.to_string();
        assert!(
            message.contains(path.to_str().unwrap()),
            "the error names the file an operator has to fix: {message}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `dormancy init` is advice for a file that has never existed. Offered for
    /// a file that exists and cannot be read, it sends the operator to a command
    /// that refuses with "already exists", which tells them nothing about the
    /// mode 000 file or the directory actually in the way.
    #[test]
    fn a_path_that_exists_but_cannot_be_read_does_not_offer_the_bootstrap() {
        let dir = scratch("unreadable");
        let path = dir.join("dormancy.toml");
        // A directory where the state file belongs: portable, and one of the
        // real ways a deployment's mount goes wrong.
        std::fs::create_dir_all(&path).unwrap();

        let error = refused(read_state(&path), "a directory is not a state file");
        let message = error.to_string();
        assert!(
            message.contains(path.to_str().unwrap()),
            "the error names the path: {message}"
        );
        assert!(
            message.contains("working directory"),
            "and still names the mistake this shape of default invites: {message}"
        );
        assert!(
            !message.contains("dormancy init"),
            "but does not send the operator to a command that will refuse: {message}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn init_creates_an_empty_state_and_refuses_to_overwrite_one() {
        let dir = scratch("init");
        let path = dir.join("nested").join("dormancy.toml");

        init_state(&path).unwrap();
        assert_eq!(
            read_state(&path).unwrap(),
            State::empty(),
            "init writes a state this build reads back as empty"
        );

        // Something worth losing, so the refusal below is about real content.
        merge_state(&path, |state| {
            dormancy::sleep(state, "http://a.example/s", "migrating", "2026-08-26T20:10:00Z")
        })
        .unwrap();
        let held = std::fs::read(&path).unwrap();

        let error = refused(init_state(&path), "init must not discard a state file that exists");
        let message = error.to_string();
        assert!(
            message.contains(path.to_str().unwrap()),
            "the refusal names the file it would have overwritten: {message}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            held,
            "and the hold an operator placed is still there"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The race as it happens on the real timeline: a sweep reads state at
    /// 19:45 and finishes at 21:11, and an operator sleeps an endpoint at 20:10
    /// in between. A write-back of the 19:45 snapshot destroys that hold, which
    /// is why `merge_state` re-reads inside the lock and hands the closure what
    /// is on disk THEN.
    #[test]
    fn a_merge_applies_to_what_is_on_disk_now_not_to_an_earlier_snapshot() {
        let dir = scratch("race");
        let path = dir.join("dormancy.toml");
        let a = "http://a.example/sparql";
        let b = "http://b.example/sparql";
        let endpoints = vec![a.to_string(), b.to_string()];
        let thresholds = Thresholds::default();
        init_state(&path).unwrap();

        // 19:45, the sweep starts and reads state to plan against.
        let snapshot = read_state(&path).unwrap();

        // 20:10, an operator sleeps b while the sweep is still probing.
        merge_state(&path, |state| dormancy::sleep(state, b, "admin asked", "2026-08-26T20:10:00Z"))
            .unwrap();

        // 21:11, the sweep writes its outcomes. b was in the plan and answered
        // nothing cheap; a struck.
        let merged = merge_state(&path, |state| {
            dormancy::update(state, &endpoints, &[expensive(a)], "2026-08-26T21:11:00Z", &thresholds)
        })
        .unwrap();

        for state in [&merged, &read_state(&path).unwrap()] {
            assert_eq!(
                state.get(b).unwrap().hold,
                Some(Hold::Dormant { reason: "admin asked".to_string() }),
                "the hold set at 20:10 survives the 21:11 write-back"
            );
            assert_eq!(
                state.get(a).unwrap().strikes,
                1,
                "and the sweep's own strike is not lost to keeping it"
            );
        }

        // What the snapshot path would have written, which is the defect.
        let from_snapshot = dormancy::update(
            &snapshot,
            &endpoints,
            &[expensive(a)],
            "2026-08-26T21:11:00Z",
            &thresholds,
        )
        .unwrap();
        assert!(
            from_snapshot.get(b).is_none(),
            "the 19:45 snapshot knows nothing of the hold, so applying the closure to it \
             is the lost update this test exists to rule out"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_merge_leaves_no_partial_and_no_lock_behind() {
        let dir = scratch("clean");
        let path = dir.join("dormancy.toml");
        init_state(&path).unwrap();

        merge_state(&path, |state| {
            dormancy::sleep(state, "http://a.example/s", "migrating", "2026-08-26T20:10:00Z")
        })
        .unwrap();

        assert!(!partial_path(&path).exists(), "the partial file was renamed, not left");
        assert!(!lock_path(&path).exists(), "and the lock was released");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["dormancy.toml".to_string()], "nothing else beside it either");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_held_lock_is_an_error_that_says_how_to_clear_it() {
        let dir = scratch("held");
        let path = dir.join("dormancy.toml");
        init_state(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let lock = lock_path(&path);
        std::fs::write(&lock, "pid = 4242\ncommand = prober --at 2026-08-26T19:45:00Z\n").unwrap();

        let error = refused(
            merge_state(&path, |state| Ok(state.clone())),
            "two writers at once is the lost update the lock exists to prevent",
        );
        let message = error.to_string();
        assert!(message.contains(lock.to_str().unwrap()), "the error names the lock: {message}");
        assert!(
            message.contains("4242"),
            "and repeats what the lock says, so the message can name the run that took it: \
             {message}"
        );
        assert!(
            message.contains("rm ") || message.contains("remove"),
            "and says how to clear it: {message}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "and wrote nothing");
        assert!(lock.exists(), "and left the other writer's lock alone");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An `O_EXCL` lock never blocks, so a lock left by a prober killed
    /// mid-merge is not a deadlock but it is also not recoverable on its own.
    /// Found at startup, it costs a rerun; found at the write, it costs the
    /// 90-minute sweep that has just finished.
    #[test]
    fn a_stale_lock_is_reported_before_the_sweep_probes_anything() {
        let dir = scratch("stale");
        let path = dir.join("dormancy.toml");
        init_state(&path).unwrap();

        check_lock(&path).unwrap();
        assert!(!lock_path(&path).exists(), "a clean check takes the lock and gives it back");

        // What `check_lock` writes while it holds the lock, which is the only
        // thing that makes the report below name a run rather than a file. The
        // two tests that quote a lock body hand-write their own, so nothing
        // else here would notice `lock_body` returning an empty string.
        let taken = Lock::take(&path).unwrap();
        let written = std::fs::read_to_string(lock_path(&path)).unwrap();
        assert!(
            written.contains(&std::process::id().to_string()),
            "the lock body names the process holding it: {written}"
        );
        assert!(
            written.contains("command: "),
            "and the command line, which is where --at is: {written}"
        );
        taken.release().unwrap();

        std::fs::write(lock_path(&path), "pid = 4242\n").unwrap();
        let error = refused(check_lock(&path), "a stale lock has to be reported at startup");
        assert!(
            error.to_string().contains("4242"),
            "and the report names the run that left it: {error}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_closure_that_fails_leaves_the_file_untouched_and_releases_the_lock() {
        let dir = scratch("closure");
        let path = dir.join("dormancy.toml");
        init_state(&path).unwrap();
        let before = std::fs::read(&path).unwrap();

        let error = refused(
            merge_state(&path, |_| anyhow::bail!("the policy refused this write")),
            "a closure that fails must not produce a write",
        );
        assert!(
            error.to_string().contains("the policy refused this write"),
            "the closure's own error reaches the caller: {error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file is byte for byte unchanged");
        assert!(!partial_path(&path).exists(), "no partial file is left behind");
        assert!(!lock_path(&path).exists(), "and the lock is released");

        // Which is the property that matters: the next write can proceed.
        merge_state(&path, |state| {
            dormancy::sleep(state, "http://a.example/s", "migrating", "2026-08-26T20:10:00Z")
        })
        .unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The double release, which is a lost update with extra steps: this
    /// process removes the lock, another writer creates it, and this process's
    /// `Drop` removes THAT one, leaving two writers holding the lock at once.
    /// A released guard must therefore be disarmed, not merely emptied.
    #[test]
    fn an_explicit_release_does_not_delete_a_lock_taken_by_someone_else() {
        let dir = scratch("release");
        let path = dir.join("dormancy.toml");
        let lock = lock_path(&path);
        init_state(&path).unwrap();

        let guard = Lock::take(&path).unwrap();
        assert!(lock.exists(), "taking the lock creates the file");
        guard.release().unwrap();
        assert!(!lock.exists(), "and releasing it removes the file");

        // Another writer takes the lock in the window after the release.
        let fresh = Lock::take(&path).unwrap();

        // `release` consumes the guard, so the released guard's `Drop` has
        // already run by the line above; the state it leaves behind is what has
        // to be inert, and this is that state. A guard whose `Drop` still
        // removed a file would take the fresh lock with it.
        let released = Lock { path: lock.clone(), armed: false };
        drop(released);
        assert!(lock.exists(), "the other writer's lock survives a released guard's Drop");

        // And an armed guard going out of scope does clean up after itself,
        // which is what covers `merge_state`'s error paths.
        drop(fresh);
        assert!(!lock.exists(), "an armed guard releases the lock when it is dropped");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
