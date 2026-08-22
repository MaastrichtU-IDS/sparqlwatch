//! Per-host politeness: a server identity to serialise requests against, a
//! reader for the `Retry-After` response header, and the gate that actually
//! holds a host and spaces our requests to it.
//!
//! The pure functions come first and are tested on their own, so the identity
//! and header decisions can be read without any locking in the way. The gate
//! below is the only stateful thing here, and the two guarantees it gives
//! (never two requests in flight to one host, and a minimum pause between
//! consecutive requests to one host) are what stand between this crate and an
//! operator who blocks us.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The politeness identity of a URL: lowercased host plus port, ignoring
/// scheme, path, query and userinfo. Two URLs with the same key are one
/// server and must never be probed concurrently.
///
/// This answers a different question from `declare::same_endpoint`, which
/// asks "is this the same SPARQL *service*" and normalises a default port
/// away for that purpose. Here the question is "is this the same *server* I
/// must not hammer", so a non-default port makes a different key: two
/// engines commonly share a host on different ports, and serialising them
/// together would halve our throughput for no politeness gain. A
/// scheme-default port, by contrast, is the same server written two ways
/// (`http://x:80/` and `http://x/`), and giving it two buckets would let two
/// requests to one host run at once, which is the one thing this key exists
/// to prevent. Because the default port depends on the scheme, the scheme is
/// not thrown away before that decision is made, even though it plays no
/// part in the final key.
///
/// No URL crate is a dependency, so this is hand-rolled: lowercase, strip a
/// scheme if present, take everything up to the first `/`, `?` or `#` as the
/// authority, drop anything up to and including the last `@` (userinfo), and
/// strip a port that is the default for whichever scheme was seen. A string
/// with no recognisable scheme, delimiter or `@` falls straight through this
/// same pipeline and comes out as its own trimmed, lowercased self: the
/// registry is seeded from a real-world dump that certainly contains junk,
/// and collapsing every unparseable string into one constant bucket would
/// serialise unrelated hosts behind each other.
///
/// The lowercasing happens BEFORE the scheme is matched, not after, because
/// schemes are case-insensitive (RFC 3986 section 3.1) and a real-world dump
/// of 548 URLs is exactly where `HTTP://` turns up. Matching case-sensitively
/// stripped nothing from such a URL, and the first `/` of its `//` then made
/// the authority `"http:"`: one shared bucket for every mixed-case URL in the
/// registry, serialising unrelated hosts behind each other, and a second
/// bucket for a host that already had one, which is the direction that lets
/// two requests to one host overlap.
///
/// The authority ends at the first `/`, `?` or `#`, whichever comes first, and
/// not at the `/` alone: `http://example.org?query=x` has no path at all, so
/// cutting on `/` only carried the query into the key and gave one host as
/// many buckets as it had query strings.
pub fn host_key(url: &str) -> String {
    let s = url.trim().to_ascii_lowercase();
    let s = s.as_str();
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        (Some("https"), r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (Some("http"), r)
    } else {
        (None, s)
    };
    let authority = match rest.find(['/', '?', '#']) {
        Some(i) => &rest[..i],
        None => rest,
    };
    // Userinfo is credentials, not identity: everything up to and including
    // the last `@`. A raw `@` cannot appear in a host, so the last one
    // delimits it.
    let authority = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = match scheme {
        Some("https") => authority.strip_suffix(":443").unwrap_or(authority),
        Some("http") => authority.strip_suffix(":80").unwrap_or(authority),
        _ => authority,
    };
    if host.is_empty() {
        return s.to_string();
    }
    host.to_string()
}

/// The three shapes a `Retry-After` header value can take. Not an `Option`,
/// because Task 3 treats the three cases differently and collapsing them
/// would lose the distinction it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfter {
    Seconds(Duration),
    HttpDate,
    Unparseable,
}

/// Read a `Retry-After` header value.
///
/// The delta-seconds form (RFC 9110) is a non-negative integer number of
/// seconds; a leading `-` or a decimal point makes it malformed rather than
/// immediate or roundable, so it is parsed as `i64` and explicitly guarded
/// against a negative value rather than cast straight to `u64`, which would
/// silently wrap a negative number into some enormous duration.
///
/// The HTTP-date form (`Wed, 21 Oct 2026 07:28:00 GMT`) is recognised, not
/// parsed: this crate does not honour it (see Task 3 for what it does
/// instead), but it must still be distinguished from junk, since a server
/// that answered precisely deserves different treatment from one that sent
/// nonsense. The recognition is a sniff, not a parser: anything that is not
/// an integer, contains a comma, and ends in `GMT` is called a date.
/// Everything else is unparseable.
pub fn parse_retry_after(value: &str) -> RetryAfter {
    let trimmed = value.trim();
    if let Ok(n) = trimmed.parse::<i64>() {
        if n < 0 {
            return RetryAfter::Unparseable;
        }
        return RetryAfter::Seconds(Duration::from_secs(n as u64));
    }
    if trimmed.contains(',') && trimmed.ends_with("GMT") {
        return RetryAfter::HttpDate;
    }
    RetryAfter::Unparseable
}

/// Whether a requested delay is small enough to wait out within the current
/// endpoint budget, given as `cap`. Kept pure and separate from any actual
/// waiting so the cap decision is testable on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Honour {
    Wait(Duration),
    TooLong,
}

/// A server asking for a delay beyond the cap has told us to go away for
/// this sweep; waiting it out would blow the endpoint budget and hold a slot
/// for nothing. The cap itself is honoured, not refused, so the comparison
/// is strictly greater-than: a request exactly at the cap still waits.
pub fn honour(requested: Duration, cap: Duration) -> Honour {
    if requested > cap {
        Honour::TooLong
    } else {
        Honour::Wait(requested)
    }
}


/// The default minimum pause between consecutive requests to one host.
///
/// Two seconds is the number the design doc argues for and the one a sweep
/// runs with unless `--min-gap-ms` says otherwise. It lives here beside the
/// cap so the two halves of "how polite are we" cannot drift apart, and so a
/// reader of this module sees both without going to `main.rs`.
pub const DEFAULT_MIN_GAP: Duration = Duration::from_secs(2);

/// The default cap on a `Retry-After` we are willing to wait out.
///
/// Twenty seconds, not two minutes: the wait happens inside the 60s metric
/// budget alongside a request that may itself take 30s, and `cap + request
/// budget` has to stay under the metric budget or tokio cancels the honoured
/// wait and reports `Indeterminate` after burning the whole budget for
/// nothing. 20 + 30 = 50 < 60. Raising the cap means moving a budget, which
/// is a deliberate decision rather than a side effect of one.
pub const DEFAULT_RETRY_AFTER_CAP: Duration = Duration::from_secs(20);

/// The gate every probe passes through, and the two settings that decide how
/// we treat somebody else's server: the minimum pause between consecutive
/// requests to one host, and the longest `Retry-After` we will wait out.
///
/// Both live on one type on purpose. An earlier draft had the gap in a gate
/// and the cap in no type at all, so the two halves of "how polite are we"
/// could drift apart and a caller could set one without noticing the other
/// existed.
///
/// The gate itself gives two guarantees, and the second does not imply the
/// first: a gap alone would let two tasks both observe it elapsed and proceed
/// together, so exclusion is a lock, not a calculation.
///
/// 1. Never two requests in flight to one host.
/// 2. At least `min_gap` between one request's release and the next one's
///    start on that host.
pub struct Politeness {
    /// Host key to per-host state. A `std::sync::Mutex` on purpose: its guard
    /// is not `Send`, so holding it across an `await` fails to compile in a
    /// future that must be `Send`. That makes "the map lock is held while
    /// waiting for a host", which would silently serialise the whole sweep
    /// behind one slow server, a build error rather than a stall no test would
    /// catch. Hold it only long enough to clone the `Arc` out.
    ///
    /// The map only grows, one entry per distinct host key. With a registry of
    /// a few hundred endpoints that is a few hundred small entries for the
    /// life of a sweep, so there is nothing to evict and no eviction to get
    /// wrong.
    hosts: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Instant>>>>>,
    /// The pause between consecutive requests to one host, measured from
    /// release.
    min_gap: Duration,
    /// The longest `Retry-After` we will wait out. Read by the client, which
    /// is what actually honours a header (see `honour` above); the gate itself
    /// never consumes it.
    retry_after_cap: Duration,
}

impl Politeness {
    /// A gate with the given gap and the default `Retry-After` cap.
    pub fn new(min_gap: Duration) -> Politeness {
        Politeness::with_retry_after_cap(min_gap, DEFAULT_RETRY_AFTER_CAP)
    }

    /// A gate with both settings stated. This is what `main.rs` uses, so the
    /// two numbers a sweep runs with both come from flags a reader can see
    /// rather than one of them from a constant in here.
    pub fn with_retry_after_cap(min_gap: Duration, retry_after_cap: Duration) -> Politeness {
        Politeness {
            hosts: std::sync::Mutex::new(HashMap::new()),
            min_gap,
            retry_after_cap,
        }
    }

    /// No gap and no willingness to wait out a `Retry-After`. **For tests
    /// only**, and it must never appear in a sweep: a constructor whose
    /// default is impoliteness, in a crate whose whole thesis is being a
    /// tolerable guest, is the silent default this project refuses everywhere
    /// else wearing different clothes. Tests use it because 2 seconds per
    /// request across the suite would take minutes and somebody would
    /// eventually delete the politeness rather than the slowness.
    pub fn unlimited() -> Politeness {
        Politeness::with_retry_after_cap(Duration::ZERO, Duration::ZERO)
    }

    /// The minimum pause between consecutive requests to one host.
    pub fn min_gap(&self) -> Duration {
        self.min_gap
    }

    /// The longest `Retry-After` a caller should wait out. Pair it with
    /// `honour` to decide.
    pub fn retry_after_cap(&self) -> Duration {
        self.retry_after_cap
    }

    /// Wait until this URL's host is free **and** the minimum gap since that
    /// host's last release has elapsed, then take it. The returned guard holds
    /// the host until it is dropped, and dropping it stamps the release time.
    ///
    /// The gap is measured from **release**, not from acquisition, so a slow
    /// request does not eat the pause that follows it: a 30-second query
    /// followed immediately by another request is exactly the case the pause
    /// exists for, and measuring from the start would let it through.
    ///
    /// The sleep happens while holding the per-host lock, which is what makes
    /// the two guarantees one thing: a waiter cannot slip in during another
    /// waiter's pause.
    pub async fn acquire(&self, url: &str) -> HostGuard {
        let key = host_key(url);
        // Cloning the `Arc` out is the entire critical section for the map
        // lock. Everything that waits happens after the guard is dropped.
        let host = {
            // A poisoned map lock means a panic happened elsewhere while
            // holding it. The guarded work is a `HashMap` lookup with no user
            // code in it, so there is nothing here for a panic to have left
            // inconsistent, and refusing every later acquire would turn one
            // panic into a sweep that probes nothing.
            let mut hosts = self.hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::clone(
                hosts
                    .entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))),
            )
        };
        let guard = host.lock_owned().await;
        if let Some(released) = *guard {
            // `checked_sub` rather than a subtraction: `Duration` subtraction
            // panics on underflow, and a release older than the gap is the
            // common case, not an error.
            if let Some(remaining) = self.min_gap.checked_sub(released.elapsed()) {
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining).await;
                }
            }
        }
        HostGuard { guard }
    }
}

/// Exclusive use of one host. Drop it when the request is done.
///
/// The guard is `OwnedMutexGuard`, not a borrowed `MutexGuard`, and that is
/// forced rather than stylistic: a guard that owned the `Arc` and borrowed a
/// `MutexGuard` from it would be a self-reference, and `acquire` would fail to
/// compile with `E0515: cannot return value referencing local variable`.
/// `lock_owned` consumes the `Arc` and hands back a guard with no borrow left
/// to outlive.
pub struct HostGuard {
    guard: tokio::sync::OwnedMutexGuard<Option<Instant>>,
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        // A synchronous write through a guard we already hold, so no await is
        // needed and `Drop` can do it. Stamping here rather than in `acquire`
        // is what makes the gap a pause *between* requests: stamped at
        // acquisition, a request that took longer than the gap would leave no
        // pause at all.
        *self.guard = Some(Instant::now());
    }
}

/// A compile-time assertion that `Politeness::acquire`'s future is `Send`.
///
/// This is what makes "the map lock is held across the await" a BUILD error
/// rather than a stall no test would catch: a `std::sync::MutexGuard` is not
/// `Send`, so a future holding one across an await point cannot satisfy this
/// bound.
///
/// It lives in `src/` deliberately. Until a later stage spawns the sweep,
/// nothing else in the crate requires this future to be `Send`, so the bound was
/// supplied only by one integration test's `JoinSet::spawn`. A guarantee that
/// disappears when somebody deletes a test is not a structural guarantee, which
/// is the whole reason the map lock is a `std::sync::Mutex` in the first place.
///
/// Never called. Its only job is to exist and be type-checked.
#[allow(dead_code)]
fn _acquire_future_is_send() {
    fn assert_send<T: Send>(_: T) {}
    let politeness = Politeness::unlimited();
    assert_send(politeness.acquire("http://example.org/sparql"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_key_is_the_server_not_the_url() {
        for (a, b) in [
            ("http://example.org/sparql", "https://example.org/other"),
            ("http://Example.ORG/a", "http://example.org/b"),
            ("http://user:pw@example.org/a", "http://example.org/b"),
            ("http://example.org/a?query=x", "http://example.org/b"),
            // A scheme is case-insensitive (RFC 3986 section 3.1), and a
            // real-world dump contains oddly-cased URLs. Case-sensitive
            // matching left this URL unstripped and keyed it as "http:".
            ("HTTP://Example.ORG/a", "http://example.org/a"),
            ("HTTPS://Example.ORG/a", "https://example.org/a"),
            ("HtTp://example.org/a", "http://example.org/a"),
            // No path at all, so the authority ends at the `?` or the `#`.
            // Cutting on `/` alone gave one host three buckets, and three
            // buckets means three requests to it can overlap.
            ("http://example.org?query=x", "http://example.org/a"),
            ("http://example.org#frag", "http://example.org/a"),
            ("http://example.org?query=x", "http://example.org#frag"),
            // The two defects together, which is the shape a registry dump
            // actually contains.
            ("HTTP://Example.ORG?query=x", "http://example.org/a"),
            ("HTTP://Example.ORG:80?query=x", "http://example.org/a"),
        ] {
            assert_eq!(host_key(a), host_key(b), "{a} and {b} are one server");
        }
        assert_eq!(host_key("HTTP://Example.ORG/a"), "example.org");
        assert_eq!(host_key("HTTPS://other.example/b"), "other.example");
        assert_eq!(host_key("http://example.org?query=x"), "example.org");
        assert_eq!(host_key("http://example.org#frag"), "example.org");
        assert_eq!(host_key("http://example.org/a"), "example.org");
        assert_ne!(host_key("http://a.example.org/x"), host_key("http://b.example.org/x"));
        // Unrelated hosts must not share a bucket just because their scheme is
        // shouted. Every uppercase-scheme URL used to key as "http:", which
        // serialised the whole registry behind whichever of them came first.
        assert_ne!(host_key("HTTP://a.example.org/x"), host_key("HTTP://b.example.org/x"),
                   "an uppercase scheme must not collapse two hosts into one bucket");
        assert_ne!(host_key("HTTP://Example.ORG/a"), host_key("HTTPS://other.example/b"));
        assert_ne!(host_key("HTTP://a.example.org?query=x"), host_key("HTTP://b.example.org?query=x"));
        // A port is part of the server: two engines commonly share a host, and
        // serialising them together would halve our throughput for no politeness
        // gain. Note this differs from `declare::same_endpoint`, which normalises a
        // default port away because it is answering a different question (is this
        // the same SERVICE), and say so in a comment.
        assert_ne!(host_key("http://example.org:7878/x"), host_key("http://example.org:7879/x"));

        // But a DEFAULT port is the same server written two ways, and giving it two
        // buckets would let two requests to one host run concurrently, which is the
        // one thing this key exists to prevent.
        assert_eq!(host_key("http://example.org:80/x"), host_key("http://example.org/x"));
        assert_eq!(host_key("https://example.org:443/x"), host_key("https://example.org/x"));
        // And the default depends on the scheme, so :443 on http is NOT default.
        assert_ne!(host_key("http://example.org:443/x"), host_key("http://example.org/x"));
    }

    #[test]
    fn a_url_we_cannot_parse_still_gets_a_key() {
        // The registry is seeded from a real-world dump. A key that panics or
        // collapses every junk URL into one bucket would either crash the sweep or
        // serialise unrelated hosts behind each other.
        let a = host_key("not a url at all");
        let b = host_key("also not a url");
        assert!(!a.is_empty());
        assert_ne!(a, b, "distinct junk must not share a bucket");
    }

    #[test]
    fn retry_after_reads_delta_seconds() {
        assert_eq!(parse_retry_after("120"), RetryAfter::Seconds(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("  30 "), RetryAfter::Seconds(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("0"), RetryAfter::Seconds(Duration::ZERO));
    }

    #[test]
    fn retry_after_distinguishes_a_date_from_junk() {
        // We do not parse the HTTP-date form (see Task 3 for what we do instead),
        // but we must not mistake it for junk: a server that answered precisely
        // deserves a different response from one that sent nonsense.
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), RetryAfter::HttpDate);
        assert_eq!(parse_retry_after("banana"), RetryAfter::Unparseable);
        assert_eq!(parse_retry_after(""), RetryAfter::Unparseable);
        assert_eq!(parse_retry_after("-5"), RetryAfter::Unparseable,
                   "a negative delay is malformed, not immediate");
        assert_eq!(parse_retry_after("12.5"), RetryAfter::Unparseable,
                   "delta-seconds is an integer; a float is malformed, not rounded");
    }

    #[test]
    fn a_delay_beyond_the_cap_is_not_waited_out() {
        // A server asking us back in an hour has told us to go away for this sweep.
        // Waiting would blow the endpoint budget and hold a slot for nothing.
        assert_eq!(honour(Duration::from_secs(3600), Duration::from_secs(120)), Honour::TooLong);
        assert_eq!(honour(Duration::from_secs(5), Duration::from_secs(120)),
                   Honour::Wait(Duration::from_secs(5)));
        assert_eq!(honour(Duration::from_secs(120), Duration::from_secs(120)),
                   Honour::Wait(Duration::from_secs(120)), "the cap itself is honoured, not refused");
    }
}
