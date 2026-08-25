//! Loading `endpoints.toml`: the list of endpoints one sweep probes.
//!
//! There are two rules here beyond parsing. The list holds each entry once,
//! and no entry's authority carries a non-empty userinfo component, whatever
//! its scheme.
//!
//! The first, because `run_sweep` emits exactly one `declarationsRead` fact per
//! LIST ENTRY, so a URL listed twice put two of those facts on one endpoint IRI
//! in one run graph, and if the two fetches differed they disagreed: `ASK { ?ep
//! :declarationsRead false }` and its negation both succeeded, with nothing in
//! the graph to tell a consumer which fetch each came from. `emit.rs` cannot
//! repair that after the fact, because the fact is a bare triple on the
//! endpoint IRI with no measurement identity to distinguish two of them.
//!
//! Deduplicating at load rather than at emission also stops the sweep probing
//! one stranger's server twice in the same run, and stage 1d-a seeds this list
//! from the LOD Cloud dump, where 725 `access_url` entries collapse to 548
//! distinct URLs: duplicates are the expected case, not the exotic one. A
//! second source, YummyData's list, is deferred because it lives in that
//! application's database rather than a checked-in file.
//!
//! The second, because the endpoint string is published verbatim inside every
//! subject `emit::subject_iri` builds, in a run graph this project never
//! rewrites: a URL carrying userinfo would put a credential in the permanent
//! record. See `without_credentials`.

use serde::Deserialize;

#[derive(Deserialize)]
struct EndpointFile {
    endpoint: Vec<String>,
}

/// Parse `endpoints.toml` and return its endpoint list, each entry once, with
/// no entry carrying credentials, in first-seen order.
///
/// Deduplicating before dropping credentials, not after, so a URL listed twice
/// produces one warning of each kind rather than two of the second. That
/// ordering is also why the two warnings name their positions differently:
/// `dedupe` reports `position` in the file's list, `without_credentials`
/// reports `deduped_position` in the list it was handed, and the second is not
/// a line in `endpoints.toml` whenever a duplicate came before it.
pub fn load_endpoints(toml_text: &str) -> anyhow::Result<Vec<String>> {
    let file: EndpointFile = toml::from_str(toml_text)?;
    let deduped = dedupe(&file.endpoint);
    let named = without_credentials(&deduped);
    let unreserved = without_reserved_names(&named);
    Ok(without_unpublishable_iris(&unreserved))
}

/// `endpoints` with every entry naming the local machine or a private network
/// dropped, and one warning per entry dropped naming which it was.
///
/// **This is called by the seeder and deliberately NOT by `load_endpoints`.**
/// The question it answers is not "may this string be probed" but "may a
/// third-party dump nominate it". Pointing this prober at your own machine or
/// at an endpoint inside your own network is a legitimate use, and this crate's
/// own test suite does it constantly through wiremock, which listens on
/// `127.0.0.1`. What is wrong is somebody else's metadata naming it, because
/// `http://localhost:3030/Dataset/query` really is in the LOD Cloud dump, and
/// from where we stand that names whatever happens to be listening on the
/// prober's host, published under a run graph nothing ever rewrites.
///
/// So this rule lives at the seam where a stranger's list becomes ours, and
/// `without_reserved_names` is the sibling that holds everywhere. Wiring this
/// one into `load_endpoints` was tried and reverted: it emptied the endpoint
/// list of two tests at `end_to_end.rs:1142` and `:1183`, which is the suite
/// telling us the rule was in the wrong place. A `cfg(test)` exemption would
/// have been a backdoor through published policy instead of a fix.
///
/// The refusal is on the HOST, through `politeness::authority`, and not on a
/// substring: `notexample.org`, `example.org.uk` and `localhostings.com` are
/// names somebody may legitimately serve from, and a `contains` check would
/// remove them from every sweep with no way to notice. Sharing that parse
/// rather than writing a second one is what keeps this answer and `host_key`'s
/// from diverging.
///
/// `https://test-svu/sparql` is in the dump and is NOT refused. A single-label
/// host cannot resolve publicly, but it is neither harmful to probe nor
/// unpublishable, so it belongs in the run as an honest failure. Refusing it
/// would be this project asserting an outcome it had not measured.
///
/// Dropped with a warning rather than made a load error, matching `dedupe` and
/// `without_credentials`: this list is seeded from real-world dumps, and
/// refusing the file would mean monitoring nothing.
///
/// What this does NOT catch is a host that resolves to one of these addresses
/// without saying so: a public name with an `A` record of `127.0.0.1`, or one
/// of the non-dotted spellings the WHATWG URL parser normalises (`http://127.1`
/// and `http://2130706433` both reach loopback through `reqwest`, and
/// `IpAddr::from_str` parses neither). A textual check cannot close that; the
/// place that can is a guard on the address the resolver actually returned,
/// which is a separate slice. Recorded here so it is not mistaken for covered.
pub fn without_unroutable_hosts(endpoints: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (filtered_position, ep) in endpoints.iter().enumerate() {
        let host = host_of(ep);
        match unroutable(&host) {
            Some(reason) => tracing::warn!(
                host = %host,
                filtered_position,
                "registry entry dropped: its host is {reason}, so probing it would either name \
                 the prober's own machine or send traffic somewhere reserved to receive none"
            ),
            None => kept.push(ep.clone()),
        }
    }
    kept
}

/// The host of `url` with any port and any IPv6 brackets removed, lowercased by
/// `politeness::authority`.
///
/// The brackets are handled before the port, because an IPv6 literal is full of
/// colons and splitting on the last one would otherwise cut the address up.
fn host_of(url: &str) -> String {
    let host_port = crate::politeness::authority(url).host_port;
    match host_port.strip_prefix('[') {
        // RFC 3986 brackets an IPv6 literal, and a port can only follow the `]`.
        Some(rest) => rest.split(']').next().unwrap_or(rest).to_string(),
        None => match host_port.rsplit_once(':') {
            Some((host, _port)) => host.to_string(),
            None => host_port,
        },
    }
}

/// Why `host` names the local machine or a private network, phrased to drop
/// into a warning, or `None` if it names neither.
///
/// `host` is expected lowercased, as `host_of` returns it.
///
/// `.localhost` and `.test` are here rather than in `without_reserved_names`
/// even though RFC 2606 reserves both, because they are reserved FOR local use:
/// `http://localhost:3030` and `http://127.0.0.1:3030` are the same machine, so
/// refusing one on every path while allowing the other would be incoherent.
fn unroutable(host: &str) -> Option<&'static str> {
    // The DNS root label: `example.org.` and `example.org` are one host, and a
    // registry hand-edited from a zone file can carry the dotted spelling.
    let host = host.strip_suffix('.').unwrap_or(host);

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        // `is_global` would say all of this in one call and is still unstable,
        // so the four families that matter here are named explicitly. Anything
        // else, including a multicast or documentation-range address, is left
        // to fail honestly at probe time rather than be refused on a guess.
        return match ip {
            std::net::IpAddr::V4(v4) => {
                if v4.is_loopback() {
                    Some("a loopback address")
                } else if v4.is_private() {
                    Some("a private address")
                } else if v4.is_link_local() {
                    Some("a link-local address")
                } else if v4.is_unspecified() {
                    Some("the unspecified address")
                } else {
                    None
                }
            }
            std::net::IpAddr::V6(v6) => {
                if v6.is_loopback() {
                    Some("a loopback address")
                } else if v6.is_unique_local() {
                    Some("a unique-local address")
                } else if v6.is_unicast_link_local() {
                    Some("a link-local address")
                } else if v6.is_unspecified() {
                    Some("the unspecified address")
                } else {
                    None
                }
            }
        };
    }

    // The two RFC 2606 names reserved for local use. `localhost` is caught by
    // the TLD arm: the whole host is then the single label, which is the TLD.
    let tld = host.rsplit('.').next().unwrap_or("");
    if matches!(tld, "localhost" | "test") {
        return Some("in a top-level domain RFC 2606 reserves for local use");
    }
    None
}

/// `endpoints` with every entry under a name that can never be a real service
/// dropped, and one warning per entry dropped.
///
/// **Called by `load_endpoints`, so it holds on every path**, unlike
/// `without_unroutable_hosts`. The difference is what the name is reserved
/// FOR. RFC 2606 sets aside `.example`, `example.com`, `example.net` and
/// `example.org` for documentation, so nobody serves anything there and
/// probing one is traffic sent to a name that exists to receive none; the dump
/// carries `http://example.org` and `http://www.example.org` for exactly that
/// reason. It sets aside `.invalid` to be guaranteed never to resolve. Neither
/// is a name an operator could point this tool at on purpose, which is what
/// makes refusing them everywhere honest rather than restrictive.
pub fn without_reserved_names(endpoints: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (filtered_position, ep) in endpoints.iter().enumerate() {
        let host = host_of(ep);
        match reserved_name(&host) {
            Some(reason) => tracing::warn!(
                host = %host,
                filtered_position,
                "registry entry dropped: its host is {reason}, so no service can be there \
                 to measure"
            ),
            None => kept.push(ep.clone()),
        }
    }
    kept
}

/// Why `host` can never carry a real service, phrased to drop into a warning,
/// or `None` if it may.
///
/// `host` is expected lowercased, as `host_of` returns it, and matching is on
/// whole labels: `notexample.org` and `example.org.uk` are names somebody may
/// legitimately serve from, and a substring check would remove them from every
/// sweep with no way to notice.
fn reserved_name(host: &str) -> Option<&'static str> {
    let host = host.strip_suffix('.').unwrap_or(host);
    let mut labels = host.rsplit('.');
    let tld = labels.next().unwrap_or("");
    if matches!(tld, "example" | "invalid") {
        return Some("in a top-level domain RFC 2606 reserves for documentation");
    }
    if matches!(tld, "com" | "net" | "org") && labels.next() == Some("example") {
        return Some("under a second-level domain RFC 2606 reserves for documentation");
    }
    None
}

/// `endpoints` with every entry that cannot be an `oxrdf::NamedNode` dropped,
/// and one warning per entry dropped.
///
/// `emit_nquads` builds the endpoint term with `NamedNode::new` and, when that
/// fails, drops every fact about the endpoint. So such an entry costs a
/// stranger's bandwidth to produce nothing at all, and refusing it up front is
/// strictly better than probing it and then discarding the result silently. The
/// dump carries two: a `{SPARQL}` template placeholder and a URL with an
/// example query inlined, both documentation artifacts rather than endpoints.
///
/// A value containing whitespace is refused here too, under this one reason,
/// because whitespace is not permitted in an IRI. It is refused and not
/// trimmed: `dqv:computedOn` publishes the registry string verbatim, and a
/// trimmed spelling is a string the registry does not contain.
///
/// The check is `NamedNode::new` itself rather than a character list, so this
/// refusal cannot drift from the constraint the emitter actually applies.
///
/// The warning names the whole URL, as `dedupe`'s does, because a rejected URL
/// commonly differs from a good one only in its query string and the host alone
/// would not locate it. `load_endpoints` and `seed::candidates` therefore call
/// this AFTER `without_credentials`, so a userinfo component has already been
/// dropped by the time anything is logged.
pub fn without_unpublishable_iris(endpoints: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (filtered_position, ep) in endpoints.iter().enumerate() {
        match oxrdf::NamedNode::new(ep) {
            Ok(_) => kept.push(ep.clone()),
            Err(error) => tracing::warn!(
                endpoint = %ep,
                filtered_position,
                %error,
                "registry entry dropped: it cannot be an IRI, so every fact about it would be \
                 unpublishable and probing it would spend a stranger's bandwidth for nothing"
            ),
        }
    }
    kept
}

/// `endpoints` with every entry whose authority carries a non-empty userinfo
/// component dropped, whatever its scheme, and one warning per entry dropped.
///
/// The endpoint string goes verbatim into every subject `emit::subject_iri`
/// builds, reversibly, in a run graph this project never rewrites. So admitting
/// `http://user:secret@host/sparql` would publish that credential permanently,
/// with no later chance to correct it. Dropped rather than made a load error
/// for the same reason `dedupe` drops: the list is seeded from real-world
/// dumps, and refusing the file would mean monitoring nothing.
///
/// The test is on the AUTHORITY, through `politeness::authority`, and not on a
/// bare `@`: `http://a.example/sparql?contact=x@y.example` is a legitimate
/// endpoint, and a `contains('@')` check would silently remove it from every
/// sweep. Sharing that parse rather than writing a second one is what keeps the
/// two answers from diverging.
///
/// The warning names the host, not the URL, because repeating the URL would
/// copy the credential into the log this function exists to keep it out of.
///
/// It names the position `deduped_position` rather than `position`, because
/// `load_endpoints` calls this on the output of `dedupe` and not on the file's
/// own list. The two differ as soon as a duplicate precedes a credentialed
/// entry, and `dedupe`'s warning reports the file's index under the plain
/// name, so an operator reading both in one log would otherwise have no way to
/// know that only one of the two numbers is a position in `endpoints.toml`.
///
/// What this does NOT catch is an API key in a query string, which stays a
/// known exposure recorded under Known limitations in `prober/README.md`. The
/// userinfo half is scheme-agnostic: `politeness::authority` delimits an
/// authority on `//`, so `ftp://alice:s3cret@a.example/sparql` is dropped like
/// any `http` one. Whether a non-http endpoint belongs in the registry at all
/// is a separate question, and a scheme allowlist here would change what gets
/// swept. Stage 1d-a retired that question FOR THE LOD CLOUD DUMP by measuring
/// it: all 548 candidates are `http` or `https`, so an allowlist would refuse
/// nothing. A different source may differ, so the question is retired for this
/// dump rather than in general.
pub fn without_credentials(endpoints: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (deduped_position, ep) in endpoints.iter().enumerate() {
        let authority = crate::politeness::authority(ep);
        // Non-empty, not merely present: RFC 3986 permits an empty userinfo
        // component, and `http://@a.example/sparql` carries nothing to leak.
        if authority.userinfo.is_some_and(|u| !u.is_empty()) {
            tracing::warn!(
                host = %authority.host_port,
                deduped_position,
                "registry entry dropped: its URL carries userinfo, and an endpoint string is \
                 published verbatim inside the subject of every fact about it"
            );
        } else {
            kept.push(ep.clone());
        }
    }
    kept
}

/// `endpoints` with later repeats of an entry dropped, first-seen order
/// preserved, and one warning per entry dropped.
///
/// Order is preserved rather than sorted because the registry's order is the
/// operator's, and a diff between two runs' outputs should not move.
///
/// The comparison is EXACT STRING equality, deliberately, and not
/// `declare::same_endpoint`'s normalisation: `http://x/sparql` and
/// `http://x/sparql/` are two entries here, not one. That normalisation exists
/// for a different question, matching a published `sd:endpoint` against the
/// URL we probed, where leniency loses nothing worse than a declaration. Here
/// the two strings are two things somebody put in the registry, and collapsing
/// them silently would hide a registry problem we would rather see: which of
/// the two spellings the sweep then probed would also be arbitrary, and the
/// endpoint IRI it published would be one the registry does not contain.
///
/// The warning is the point of doing this here rather than quietly: dropping
/// entries from a registry without saying so is how a seeding bug becomes
/// invisible.
///
/// Because this runs before any probing, `emit::emit_nquads`'s duplicate-subject
/// guard is belt and braces rather than a routine path: a subject there is
/// derived from (run, endpoint, metric), so two entries of one list naming one
/// pair would land on a single node. If this function is ever relaxed or
/// bypassed, that guard is what stops the pair carrying two contradictory
/// verdicts.
pub fn dedupe(endpoints: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (position, ep) in endpoints.iter().enumerate() {
        if seen.insert(ep.as_str()) {
            kept.push(ep.clone());
        } else {
            tracing::warn!(
                endpoint = %ep,
                position,
                "duplicate registry entry dropped; it is probed once and gets one row per metric"
            );
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;

    thread_local! {
        /// Where this thread's `logs_of` call, if it is inside one, wants the
        /// subscriber's output put.
        static CAPTURED: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    /// A `MakeWriter` that appends to the calling thread's `CAPTURED` buffer, so
    /// a test can assert the warning was actually emitted rather than assume it.
    ///
    /// Per thread and not one shared buffer, because `cargo test` runs these
    /// tests in parallel and a shared buffer would hand one test another test's
    /// warnings. A thread with no buffer set is not inside `logs_of`, and its
    /// output is dropped.
    #[derive(Clone, Copy, Default)]
    struct ThreadCapture;

    impl Write for ThreadCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            CAPTURED.with(|slot| {
                if let Some(sink) = slot.borrow_mut().as_mut() {
                    sink.extend_from_slice(buf);
                }
            });
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for ThreadCapture {
        type Writer = Self;
        fn make_writer(&self) -> Self::Writer {
            *self
        }
    }

    /// Install one subscriber for the whole test binary, once.
    ///
    /// A process-wide default rather than a `tracing::subscriber::with_default`
    /// per call, which is what `logs_of` used to do and which made this suite
    /// fail 9 times in 80 parallel `cargo test --lib` runs with an empty log.
    /// `with_default` installs per thread, but `tracing-core`'s call-site
    /// interest cache is process-global, and every `with_default` builds a
    /// `Dispatch` whose constructor rebuilds that cache. With one dispatcher
    /// registered the rebuild asks `dispatcher::get_default` on the rebuilding
    /// thread (`tracing-core-0.1.36/src/callsite.rs:564-566`); a thread that is
    /// not inside `with_default` has no default, so `NoSubscriber` answers
    /// `Interest::never`, and that verdict is then cached for every thread. It
    /// disabled the `tracing::warn!` call site under whichever test happened to
    /// be capturing.
    ///
    /// A global default cannot lose that race. `get_default` falls back to the
    /// global whenever no scoped dispatcher is set anywhere in the process
    /// (`tracing-core-0.1.36/src/dispatcher.rs:383-386`), and nothing in this
    /// crate sets one, so every rebuild on every thread sees this subscriber.
    /// The cache is rebuilt explicitly afterwards because `Dispatch::new`
    /// rebuilds it BEFORE `set_global_default` stores the dispatch
    /// (`dispatcher.rs:299-326`), so a call site already registered by an
    /// earlier test would otherwise keep the `never` it was handed.
    fn install_capture() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(ThreadCapture)
                .without_time()
                .with_ansi(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("nothing else in this test binary sets a global default");
            tracing::callsite::rebuild_interest_cache();
        });
    }

    /// Run `f` and return what it logged on this thread.
    ///
    /// The buffer is set rather than checked for absence: a panic inside `f`
    /// leaves it behind, and libtest may put another test on that thread.
    /// `logs_of` is not nested anywhere in this module.
    fn logs_of(f: impl FnOnce()) -> String {
        install_capture();
        CAPTURED.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
        f();
        let bytes = CAPTURED
            .with(|slot| slot.borrow_mut().take())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A note on the placeholder hosts below. An endpoint a test expects to be
    /// KEPT cannot be spelled `b.example`, because RFC 2606 reserves that TLD
    /// and `without_unroutable_hosts` refuses it. Single-label hosts
    /// such as `https://b/sparql` are used instead, which is the same ruling
    /// that seeds the dump's `https://test-svu/sparql` rather than refusing it.
    /// A host inside a URL a test expects to be DROPPED for its credentials is
    /// left as `a.example`, so the assertion that the warning names the host
    /// still names something distinctive.

    #[test]
    fn a_repeated_entry_is_kept_once_in_first_seen_order() {
        let list = v(&["https://b/sparql", "https://a/sparql", "https://b/sparql", "https://c/sparql"]);
        assert_eq!(dedupe(&list), v(&["https://b/sparql", "https://a/sparql", "https://c/sparql"]));
    }

    #[test]
    fn a_url_repeated_many_times_is_still_probed_once() {
        let list = v(&["https://a/sparql"; 5]);
        assert_eq!(dedupe(&list), v(&["https://a/sparql"]));
    }

    /// The ruling, stated as a test: dedupe is on the exact string. Every one
    /// of these pairs is one service to `declare::same_endpoint`, and two
    /// registry entries here, because two spellings in a registry are a
    /// registry problem to see rather than one to collapse silently.
    #[test]
    fn a_near_duplicate_is_two_entries_not_one() {
        for pair in [
            ["http://x/sparql", "http://x/sparql/"],
            ["http://x/sparql", "https://x/sparql"],
            ["http://x/sparql", "http://X/sparql"],
            ["http://x/sparql", "http://www.x/sparql"],
        ] {
            let list = v(&pair);
            assert_eq!(dedupe(&list).len(), 2, "{pair:?} are two registry entries");
        }
    }

    #[test]
    fn every_dropped_duplicate_is_warned_about() {
        let list = v(&["https://a/sparql", "https://b/sparql", "https://a/sparql", "https://a/sparql"]);
        let logs = logs_of(|| {
            assert_eq!(dedupe(&list).len(), 2);
        });
        // Two entries dropped, so two warnings: a count, because a single
        // warning for a registry that repeated one URL fifty times would hide
        // the scale of the seeding bug.
        assert_eq!(logs.matches("duplicate registry entry dropped").count(), 2, "logs were: {logs}");
        assert_eq!(logs.matches("WARN").count(), 2, "the level has to be WARN: {logs}");
        assert!(logs.contains("https://a/sparql"), "the warning must name the URL: {logs}");
        assert!(!logs.contains("https://b/sparql"), "nothing was dropped for b: {logs}");
    }

    #[test]
    fn a_clean_list_warns_about_nothing() {
        let list = v(&["https://a/sparql", "https://b/sparql"]);
        let logs = logs_of(|| {
            assert_eq!(dedupe(&list).len(), 2);
        });
        assert!(logs.is_empty(), "a clean registry must be quiet, logged: {logs}");
    }

    #[test]
    fn an_endpoint_carrying_credentials_is_dropped_with_a_warning() {
        // `emit::subject_iri` puts the endpoint string into every subject of
        // every fact about it, reversibly, in a run graph this project never
        // rewrites. So a URL carrying userinfo would publish credentials
        // permanently. Dropped with a warning, matching how `dedupe` handles a
        // duplicate rather than failing the whole load: this list is seeded
        // from real-world dumps, and refusing the file would mean monitoring
        // nothing.
        let logs = logs_of(|| {
            let loaded = load_endpoints(
                r#"endpoint = ["http://alice:s3cret@a.example/sparql", "https://b/sparql"]"#,
            )
            .unwrap();
            assert_eq!(loaded, v(&["https://b/sparql"]));
        });
        assert_eq!(logs.matches("WARN").count(), 1, "the level has to be WARN: {logs}");
        assert!(logs.contains("a.example"), "the warning must name the host: {logs}");
        assert!(
            !logs.contains("s3cret"),
            "the warning must not repeat the credentials it dropped: {logs}"
        );
    }

    #[test]
    fn an_at_sign_outside_the_authority_is_not_credentials() {
        // `http://a.example/sparql?contact=x@y.example` is a legitimate
        // endpoint. A `contains('@')` check would silently remove it from every
        // sweep. Userinfo is a property of the AUTHORITY, which is what
        // `politeness::authority` parses.
        let kept = "http://a/sparql?contact=x@y.example";
        let logs = logs_of(|| {
            let loaded = load_endpoints(&format!(r#"endpoint = ["{kept}"]"#)).unwrap();
            assert_eq!(loaded, v(&[kept]));
        });
        assert!(logs.is_empty(), "a legitimate endpoint must be quiet, logged: {logs}");
    }

    #[test]
    fn credentials_are_dropped_whatever_the_scheme() {
        // `NamedNode::new` accepts any absolute IRI, and `run_sweep` builds
        // rows for every registry entry, so a non-http URL reaches emission
        // and its password lands reversibly in every subject about it. The
        // authority is delimited by `//` whatever the scheme, so the check
        // does not depend on recognising the scheme.
        let logs = logs_of(|| {
            let loaded = load_endpoints(
                r#"endpoint = ["ftp://alice:s3cret@a.example/sparql", "sparql://bob:hunter2@c.example/x", "https://b/sparql"]"#,
            )
            .unwrap();
            assert_eq!(loaded, v(&["https://b/sparql"]));
        });
        assert_eq!(logs.matches("WARN").count(), 2, "one warning each: {logs}");
        assert!(!logs.contains("s3cret"), "the credential must not reach the log: {logs}");
        assert!(!logs.contains("hunter2"), "the credential must not reach the log: {logs}");
    }

    #[test]
    fn an_empty_userinfo_is_not_a_credential() {
        // RFC 3986 permits an empty userinfo component, and such a URL carries
        // nothing to leak. Dropping it would remove a legal endpoint from
        // every sweep for no gain, which is the same mistake as testing for a
        // bare `@`.
        let kept = "http://@a/sparql";
        let logs = logs_of(|| {
            let loaded = load_endpoints(&format!(r#"endpoint = ["{kept}"]"#)).unwrap();
            assert_eq!(loaded, v(&[kept]));
        });
        assert!(logs.is_empty(), "nothing was dropped, so nothing to warn about: {logs}");
    }

    #[test]
    fn the_shipped_registry_file_loads_and_is_already_unique() {
        let loaded = load_endpoints(include_str!("../endpoints.toml")).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0], "https://qlever.dev/api/osm-planet");
    }

    #[test]
    fn a_file_listing_one_url_twice_loads_it_once() {
        let loaded = load_endpoints(
            r#"endpoint = ["https://a/sparql", "https://b/sparql", "https://a/sparql"]"#,
        )
        .unwrap();
        assert_eq!(loaded, v(&["https://a/sparql", "https://b/sparql"]));
    }

    /// A host that names the prober's own machine, or a neighbour on its
    /// network, is refused before anything is sent. The reason is asserted, not
    /// merely the absence: a test that only checked the entry was gone would
    /// pass on a refusal for the wrong cause.
    #[test]
    fn a_loopback_private_or_link_local_host_is_refused_with_its_reason() {
        for (url, reason) in [
            ("http://localhost:3030/Dataset/query", "RFC 2606"),
            ("http://127.0.0.1:8890/sparql", "loopback"),
            ("http://[::1]:8890/sparql", "loopback"),
            ("http://10.1.2.3/sparql", "private"),
            ("http://172.16.0.1/sparql", "private"),
            ("http://192.168.1.1/sparql", "private"),
            ("http://169.254.169.254/latest/meta-data/", "link-local"),
            ("http://0.0.0.0/sparql", "unspecified"),
        ] {
            let logs = logs_of(|| {
                let kept = without_unroutable_hosts(&v(&[url]));
                assert!(kept.is_empty(), "{url} must not reach a sweep, kept {kept:?}");
            });
            assert!(logs.contains(reason), "{url} must be refused as {reason}, logged: {logs}");
            assert_eq!(logs.matches("WARN").count(), 1, "one warning per entry: {logs}");
        }
    }

    /// RFC 2606 reserves these names so that nobody sends them traffic. The
    /// dump contains two of them, one of which it marks `OK`.
    #[test]
    fn an_rfc_2606_reserved_name_is_refused_with_its_reason() {
        for url in [
            "http://example.org",
            "http://www.example.org",
            "http://example.com/sparql",
            "http://example.net/sparql",
            "http://anything.example/sparql",
            "http://anything.invalid/sparql",
            // The DNS root label spells one host two ways, and a registry that
            // was hand-edited from a zone file can carry the dotted form.
            "http://example.org./sparql",
        ] {
            let logs = logs_of(|| {
                let kept = without_reserved_names(&v(&[url]));
                assert!(kept.is_empty(), "{url} must not reach a sweep, kept {kept:?}");
            });
            assert!(logs.contains("RFC 2606"), "{url} must be refused as reserved: {logs}");
        }
    }

    /// The refusal is on the host, not on a substring of it. Each of these is a
    /// name or address somebody may legitimately run an endpoint on, and
    /// dropping one would remove it from every sweep for good.
    ///
    /// `https://test-svu/sparql` is in the dump and is seeded on purpose: a
    /// single-label host cannot resolve publicly, but it is neither harmful to
    /// probe nor unpublishable, so it belongs in the run as an honest failure
    /// rather than as a refusal.
    #[test]
    fn a_routable_lookalike_is_not_refused() {
        let urls = v(&[
            "https://test-svu/sparql",
            "http://notexample.org/sparql",
            "http://myexample.com/sparql",
            "http://example.org.uk/sparql",
            "http://localhostings.com/sparql",
            "http://11.0.0.1/sparql",
            "http://172.32.0.1/sparql",
            "http://169.253.0.1/sparql",
            "http://126.255.255.255/sparql",
        ]);
        let logs = logs_of(|| {
            assert_eq!(without_unroutable_hosts(&urls), urls);
            assert_eq!(without_reserved_names(&urls), urls);
        });
        assert!(logs.is_empty(), "nothing was refused, so nothing to warn about: {logs}");
    }

    /// The two filters answer different questions, so they live in different
    /// places, and this pins that rather than leaving it to a doc comment.
    ///
    /// A loopback URL must SURVIVE `load_endpoints`, because pointing this
    /// prober at your own machine is legitimate and this crate's own suite does
    /// it through wiremock, which listens on `127.0.0.1`. The same URL must be
    /// REFUSED by the seeder, because a third-party dump naming it is naming
    /// somebody else's machine. Wiring the loopback rule into `load_endpoints`
    /// emptied the endpoint list of two tests at `end_to_end.rs:1142` and
    /// `:1183`, which is how the suite reported that the rule was in the wrong
    /// place.
    #[test]
    fn a_loopback_url_survives_a_hand_written_registry_and_is_refused_by_the_seeder() {
        let local = "http://127.0.0.1:3030/query";
        let loaded = load_endpoints(&format!("endpoint = [{local:?}]")).unwrap();
        assert_eq!(loaded, v(&[local]), "an operator may probe their own machine");

        let seeded = without_unroutable_hosts(&v(&[local]));
        assert!(seeded.is_empty(), "a dump may not nominate it, kept {seeded:?}");
    }

    /// `localhost` and `127.0.0.1` are the same machine, so they are refused by
    /// the same rule. RFC 2606 reserves `.localhost` and `.test` for local use,
    /// which is a different thing from reserving `.example` for documentation,
    /// and treating them alike would refuse on every path a name an operator
    /// may legitimately have pointed at their own host.
    #[test]
    fn a_local_use_name_is_refused_by_the_seeder_and_not_on_every_path() {
        for url in ["http://localhost:3030/query", "http://anything.test/sparql"] {
            let loaded = load_endpoints(&format!("endpoint = [{url:?}]")).unwrap();
            assert_eq!(loaded, v(&[url]), "{url} is for local use, not documentation");

            let logs = logs_of(|| {
                let seeded = without_unroutable_hosts(&v(&[url]));
                assert!(seeded.is_empty(), "a dump may not nominate {url}, kept {seeded:?}");
            });
            assert!(logs.contains("local use"), "{url} refused for the wrong reason: {logs}");
        }
    }

    /// A candidate that cannot be a `NamedNode` is refused before it is probed,
    /// because every fact about it would be unpublishable: `emit_nquads` builds
    /// the endpoint term with `NamedNode::new` and silently drops the facts when
    /// that fails, so probing it spends a stranger's bandwidth to produce
    /// nothing. The test asserts `NamedNode::new` agrees, so it cannot drift
    /// from the emitter's actual constraint.
    #[test]
    fn a_url_that_cannot_be_a_named_node_is_refused_with_its_reason() {
        for url in [
            // Both of these are in the dump: a template placeholder and a URL
            // with an example query inlined. Both are documentation artifacts.
            "https://query.wikidata.org/bigdata/namespace/wdq/sparql?query={SPARQL}",
            "https://trackloaded.com/sparql-endpoint.php?query=SELECT+?name+WHERE+{+?s+?p+?o+}",
            "http://a/sparql with a space",
            " http://b/sparql",
            "not-a-url",
        ] {
            assert!(
                oxrdf::NamedNode::new(url).is_err(),
                "{url} has to be unusable as a NamedNode for this test to be about anything"
            );
            let logs = logs_of(|| {
                let kept = without_unpublishable_iris(&v(&[url]));
                assert!(kept.is_empty(), "{url} must not reach a sweep, kept {kept:?}");
            });
            assert!(logs.contains("cannot be an IRI"), "{url} must be refused as such: {logs}");
            assert_eq!(logs.matches("WARN").count(), 1, "one warning per entry: {logs}");
        }
    }

    /// One rule reported under one reason. A value carrying whitespace is not a
    /// valid IRI, so it is refused by the same check and not repaired: trimming
    /// it would publish an endpoint string the registry does not contain, and
    /// `dqv:computedOn` publishes the registry string verbatim.
    #[test]
    fn a_value_containing_whitespace_is_refused_not_trimmed() {
        let logs = logs_of(|| {
            let loaded = load_endpoints(
                r#"endpoint = [" https://a/sparql", "https://b/sparql"]"#,
            )
            .unwrap();
            assert_eq!(loaded, v(&["https://b/sparql"]));
            assert!(
                !loaded.iter().any(|e| e == "https://a/sparql"),
                "the trimmed spelling is a string the registry never contained: {loaded:?}"
            );
        });
        assert!(logs.contains("cannot be an IRI"), "refused under the IRI reason: {logs}");
    }

    /// The wiring, and it is deliberately partial. A registry file can be
    /// hand-edited and stage 5 accepts public submissions, so the two refusals
    /// that hold whoever supplied the list belong here: a documentation name
    /// can carry no service, and a string that is not an IRI is unpublishable.
    ///
    /// The loopback rule is NOT here, and `localhost` surviving this call is
    /// the assertion that says so. See
    /// `a_loopback_url_survives_a_hand_written_registry_and_is_refused_by_the_seeder`.
    #[test]
    fn load_endpoints_applies_the_refusals_that_hold_on_every_path() {
        let logs = logs_of(|| {
            let loaded = load_endpoints(
                r#"endpoint = ["http://localhost:3030/Dataset/query", "http://example.org", "https://q/sparql?query={SPARQL}", "https://b/sparql"]"#,
            )
            .unwrap();
            assert_eq!(
                loaded,
                v(&["http://localhost:3030/Dataset/query", "https://b/sparql"]),
                "a hand-written registry may name the local machine; only the \
                 documentation name and the non-IRI are refused here"
            );
        });
        assert_eq!(logs.matches("WARN").count(), 2, "one warning per refusal: {logs}");
    }

    #[test]
    fn a_file_that_is_not_an_endpoint_list_is_a_load_error() {
        assert!(load_endpoints("nonsense = 1").is_err());
        assert!(load_endpoints("endpoint = \"not a list\"").is_err());
    }
}
