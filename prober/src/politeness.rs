//! Per-host politeness: a server identity to serialise requests against, a
//! reader for the `Retry-After` response header, and the gate that actually
//! holds a host and spaces our requests to it.
//!
//! The pure functions come first and are tested on their own, so the identity
//! and header decisions can be read without any locking in the way. The gate
//! below is the only stateful thing here, and the guarantees it gives (never
//! two requests in flight to one host, a minimum pause between consecutive
//! requests to one host, and nothing at all to a host before the instant it
//! asked us to come back at) are what stand between this crate and an
//! operator who blocks us.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The parse `host_key` performs, exposed because `registry` needs the same
/// answer to a different question.
pub struct Authority {
    /// The scheme actually recognised, `None` for a string carrying none. Kept
    /// because which default port `host_key` may strip depends on it.
    pub scheme: Option<&'static str>,
    /// The userinfo component, without its trailing `@`, or `None` when the
    /// authority has none. Userinfo is credentials, not identity: `host_key`
    /// drops it so two requests to one host carrying different credentials
    /// still serialise behind each other, and `registry::without_credentials`
    /// refuses such an endpoint outright, because the endpoint string is
    /// published verbatim inside every subject `emit::subject_iri` builds.
    pub userinfo: Option<String>,
    /// The authority with any userinfo removed, port still attached.
    pub host_port: String,
}

/// Split a URL into the scheme, userinfo and host of its authority.
///
/// No URL crate is a dependency, so this is hand-rolled: lowercase, strip a
/// scheme if present, take everything up to the first `/`, `?` or `#` as the
/// authority, then split off anything up to and including the last `@`
/// (userinfo). A string with no recognisable scheme, delimiter or `@` falls
/// straight through this same pipeline and comes out as its own trimmed,
/// lowercased self in `host_port`: the registry is seeded from a real-world
/// dump that certainly contains junk, and collapsing every unparseable string
/// into one constant bucket would serialise unrelated hosts behind each other.
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
/// many buckets as it had query strings. It is also what keeps
/// `http://a.example/sparql?contact=x@y.example` free of userinfo: the `@`
/// there sits in the query, past where the authority ended.
pub fn authority(url: &str) -> Authority {
    let lowered = url.trim().to_ascii_lowercase();
    let s = lowered.as_str();
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
    let (userinfo, host_port) = match authority.rfind('@') {
        Some(i) => (Some(authority[..i].to_string()), &authority[i + 1..]),
        None => (None, authority),
    };
    Authority {
        scheme,
        userinfo,
        host_port: host_port.to_string(),
    }
}

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
pub fn host_key(url: &str) -> String {
    let parsed = authority(url);
    let host_port = parsed.host_port.as_str();
    let host = match parsed.scheme {
        Some("https") => host_port.strip_suffix(":443").unwrap_or(host_port),
        Some("http") => host_port.strip_suffix(":80").unwrap_or(host_port),
        _ => host_port,
    };
    if host.is_empty() {
        return url.trim().to_ascii_lowercase();
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
/// Twenty seconds, not two minutes, and the ceiling is arithmetic rather than
/// taste. Four things come out of one 60s metric budget, in this order: the gap
/// before the first request, the first request, the wait the throttle asked
/// for, and the retried request. The case a cap can protect is the ordinary
/// one, where the throttle came back quickly, since refusing a request is cheap
/// for a server that is refusing it: `gap (2) + wait (20) + retried request
/// (30) = 52 < 60`. So a larger cap eats that margin, and raising it makes a
/// cancelled retry MORE likely rather than less.
///
/// No cap value makes the worst case fit. A first request that runs its whole
/// 30s timeout before the throttle arrives costs 2 + 30 + 30 = 62 with a cap of
/// ZERO, so a gap plus two full-timeout requests is already over budget on its
/// own. When that happens `tokio` cancels somewhere inside the retry and the
/// metric reports `indeterminate`, which is exactly true: we never got an
/// answer. A retry guaranteed to fit would need a metric budget above 82
/// seconds, which is a decision nobody has taken.
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
/// The gate itself gives three guarantees, and the second does not imply the
/// first: a gap alone would let two tasks both observe it elapsed and proceed
/// together, so exclusion is a lock, not a calculation. The third is what makes
/// the second one honest, since a pause we chose is no answer to a pause the
/// server asked for.
///
/// 1. Never two requests in flight to one host.
/// 2. At least `min_gap` between one request's release and the next one's
///    start on that host.
/// 3. Nothing at all to a host before the instant it asked us to come back at,
///    once `stand_down` has recorded one.
pub struct Politeness {
    /// Host key to per-host state. A `std::sync::Mutex` on purpose: its guard
    /// is not `Send`, so holding it across an `await` fails to compile in a
    /// future that must be `Send`. That makes "the map lock is held while
    /// waiting for a host", which would silently serialise the whole sweep
    /// behind one slow server, a build error rather than a stall no test would
    /// catch. Hold it only long enough to clone the `Arc` out or read the
    /// stand-down instant beside it.
    ///
    /// The map only grows, one entry per distinct host key. With a registry of
    /// a few hundred endpoints that is a few hundred small entries for the
    /// life of a sweep, so there is nothing to evict and no eviction to get
    /// wrong.
    hosts: std::sync::Mutex<HashMap<String, Host>>,
    /// The pause between consecutive requests to one host, measured from
    /// release.
    min_gap: Duration,
    /// The longest `Retry-After` we will wait out. Read by the client, which
    /// is what actually honours a header (see `honour` above); the gate itself
    /// never consumes it.
    retry_after_cap: Duration,
}

/// Everything the gate remembers about one host.
///
/// Both fields are read and written only under the map lock, which is held for
/// a lookup and never across an await. The lock inside `serialiser` is the one
/// that is held across an await, and it is reached through an `Arc` cloned out
/// of here precisely so the map lock does not have to be.
struct Host {
    /// Serialises requests to this host, and carries the instant the last
    /// request to it was released, which is what the gap is measured from.
    serialiser: Arc<tokio::sync::Mutex<Option<Instant>>>,
    /// The earliest instant we may send this host anything at all. `None`
    /// until the host tells us to come back later; see `stand_down`.
    not_before: Option<Instant>,
}

impl Host {
    fn new() -> Host {
        Host { serialiser: Arc::new(tokio::sync::Mutex::new(None)), not_before: None }
    }
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

    /// Wait until this URL's host is free, the minimum gap since that host's
    /// last release has elapsed **and** any instant the host asked us to come
    /// back at has passed, then take it. The returned guard holds the host
    /// until it is dropped, and dropping it stamps the release time.
    ///
    /// The gap is measured from **release**, not from acquisition, so a slow
    /// request does not eat the pause that follows it: a 30-second query
    /// followed immediately by another request is exactly the case the pause
    /// exists for, and measuring from the start would let it through.
    ///
    /// A host that has been told to stand down waits for that instant too, and
    /// the wait is the LONGER of the two, not the sum: both are "not before
    /// this moment" conditions on the same clock.
    ///
    /// The sleep happens while holding the per-host lock, which is what makes
    /// the guarantees one thing rather than three: a waiter cannot slip in
    /// during another waiter's pause, and a host that asked us to come back in
    /// five minutes is not asked something else in the meantime.
    pub async fn acquire(&self, url: &str) -> HostGuard {
        let key = host_key(url);
        // Cloning the `Arc` out is the entire critical section for the map
        // lock. Everything that waits happens after the guard is dropped.
        let serialiser = {
            // A poisoned map lock means a panic happened elsewhere while
            // holding it. The guarded work is a `HashMap` lookup with no user
            // code in it, so there is nothing here for a panic to have left
            // inconsistent, and refusing every later acquire would turn one
            // panic into a sweep that probes nothing.
            let mut hosts = self.hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::clone(&hosts.entry(key.clone()).or_insert_with(Host::new).serialiser)
        };
        let guard = serialiser.lock_owned().await;
        // Read the stand-down AFTER taking the host, not before: a throttle
        // recorded while we were queueing for this host is exactly the one we
        // must not walk into. Nothing can record one while we hold the host,
        // since recording one means having had a response from it, which means
        // having held it.
        //
        // A second, separate critical section rather than one that spans the
        // await, because the map lock's guard is not `Send` and holding it
        // across the await would not compile. That is the guard rail working,
        // not an obstacle: see the field's comment.
        let not_before = {
            let hosts = self.hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            hosts.get(&key).and_then(|h| h.not_before)
        };
        let mut wait = Duration::ZERO;
        if let Some(released) = *guard {
            // `checked_sub` rather than a subtraction: `Duration` subtraction
            // panics on underflow, and a release older than the gap is the
            // common case, not an error.
            if let Some(remaining) = self.min_gap.checked_sub(released.elapsed()) {
                wait = remaining;
            }
        }
        if let Some(until) = not_before {
            // `saturating_duration_since` rather than a subtraction: an
            // instant already past is the common case once a sweep has moved
            // on, not an error.
            wait = wait.max(until.saturating_duration_since(Instant::now()));
        }
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        HostGuard { guard }
    }

    /// Record that this URL's host asked us to come back in `delay`, so that
    /// EVERY later request to it waits, not just the retry of the request that
    /// was told.
    ///
    /// This is what makes honouring a `Retry-After` honest. A delay honoured
    /// only by the request that received it means we wait twenty seconds
    /// before repeating that one question and knock again with the next
    /// metric's question after the ordinary gap: we said we would come back
    /// later and then came back immediately with something else. A throttle is
    /// about the server, so it is remembered on the server.
    ///
    /// Deliberately synchronous, and deliberately not bounded here:
    ///
    /// - Synchronous because it takes the map lock and nothing else. It is
    ///   called from the retry path, which runs between chain walks with no
    ///   host guard held, and it must stay callable from there without a
    ///   second await that could queue behind a host it is about to defer.
    /// - Unbounded because the caller already decides what IT is willing to
    ///   wait out (`honour` and the cap), and this is a different question: how
    ///   long the host asked to be left alone. An hour recorded here defers the
    ///   host for an hour, the metric and endpoint budgets cancel the acquires
    ///   that wait on it, and those metrics report `indeterminate`, which is
    ///   exactly true: the server told us to go away and we never got to ask.
    ///   A second bound here would be a second place deciding how long we wait.
    ///
    /// Only ever extends: a shorter delay arriving after a longer one does not
    /// bring the host back early, since the longer instruction has not expired
    /// just because a later request was answered.
    pub fn stand_down(&self, url: &str, delay: Duration) {
        // `checked_add`, because `Instant + Duration` panics on overflow and
        // `Retry-After` is a header a stranger controls: the delta-seconds
        // form parses as an `i64`, so `Retry-After: 9223372036854775807` is a
        // well-formed value no clock can represent. An instant we cannot
        // represent is not an instruction we can keep, and the request that
        // saw it is reported as the throttle it was.
        let Some(until) = Instant::now().checked_add(delay) else {
            tracing::warn!(url, delay_s = delay.as_secs(),
                           "Retry-After is too far in the future to represent; host not deferred");
            return;
        };
        let key = host_key(url);
        let mut hosts = self.hosts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let host = hosts.entry(key).or_insert_with(Host::new);
        host.not_before = host.not_before.max(Some(until));
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
