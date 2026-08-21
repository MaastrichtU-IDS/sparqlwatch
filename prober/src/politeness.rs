//! Per-host politeness: a server identity to serialise requests against, and
//! a reader for the `Retry-After` response header. Both are pure functions,
//! tested here on their own before Task 3 puts either behind a lock.

use std::time::Duration;

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
/// No URL crate is a dependency, so this is hand-rolled: strip a scheme if
/// present, take everything up to the first `/` as the authority, drop
/// anything up to and including the last `@` (userinfo), lowercase, and
/// strip a port that is the default for whichever scheme was seen. A string
/// with no recognisable scheme, slash or `@` falls straight through this
/// same pipeline and comes out as its own trimmed, lowercased self: the
/// registry is seeded from a real-world dump that certainly contains junk,
/// and collapsing every unparseable string into one constant bucket would
/// serialise unrelated hosts behind each other.
pub fn host_key(url: &str) -> String {
    let s = url.trim();
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        (Some("https"), r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (Some("http"), r)
    } else {
        (None, s)
    };
    let authority = match rest.find('/') {
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
    let host = authority.to_ascii_lowercase();
    let host = match scheme {
        Some("https") => host.strip_suffix(":443").unwrap_or(&host).to_string(),
        Some("http") => host.strip_suffix(":80").unwrap_or(&host).to_string(),
        _ => host,
    };
    if host.is_empty() {
        return s.to_ascii_lowercase();
    }
    host
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
        ] {
            assert_eq!(host_key(a), host_key(b), "{a} and {b} are one server");
        }
        assert_ne!(host_key("http://a.example.org/x"), host_key("http://b.example.org/x"));
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
