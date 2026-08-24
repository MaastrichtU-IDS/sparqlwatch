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
//! one stranger's server twice in the same run, and stage 1d seeds this list
//! from LOD Cloud plus YummyData, two overlapping real-world dumps: duplicates
//! are the expected case, not the exotic one.
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
    Ok(without_credentials(&dedupe(&file.endpoint)))
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
/// swept, so it is left to stage 1d's triage of the seeding dumps.
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
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// A `MakeWriter` that keeps what a subscriber wrote, so a test can assert
    /// the warning was actually emitted rather than assume it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for Captured {
        type Writer = Self;
        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `f` with a subscriber of our own, scoped to this thread, and return
    /// what it logged.
    fn logs_of(f: impl FnOnce()) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .without_time()
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        captured.text()
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

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
                r#"endpoint = ["http://alice:s3cret@a.example/sparql", "https://b.example/sparql"]"#,
            )
            .unwrap();
            assert_eq!(loaded, v(&["https://b.example/sparql"]));
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
        let kept = "http://a.example/sparql?contact=x@y.example";
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
                r#"endpoint = ["ftp://alice:s3cret@a.example/sparql", "sparql://bob:hunter2@c.example/x", "https://b.example/sparql"]"#,
            )
            .unwrap();
            assert_eq!(loaded, v(&["https://b.example/sparql"]));
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
        let kept = "http://@a.example/sparql";
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

    #[test]
    fn a_file_that_is_not_an_endpoint_list_is_a_load_error() {
        assert!(load_endpoints("nonsense = 1").is_err());
        assert!(load_endpoints("endpoint = \"not a list\"").is_err());
    }
}
