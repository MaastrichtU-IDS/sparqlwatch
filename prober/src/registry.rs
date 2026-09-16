//! Loading `endpoints.toml`: the list of endpoints one sweep probes.
//!
//! Every rule about which endpoints may be on such a list lives here, so there
//! is one implementation of endpoint-list policy. Two of them are worth stating
//! up front: the list holds each entry once, and no entry's authority carries a
//! non-empty userinfo component, whatever its scheme. `prober/README.md` under
//! Configuration files lists all of them in the order they run.
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
//!
//! A third is not decidable from the string at all: `registry/exclusions.toml`
//! names the hosts somebody asked this project to leave alone. It is the only
//! rule applied both here and in the seeder, and the only one that fails the
//! load rather than dropping an entry with a warning: a list that cannot be
//! read or parsed stops the run, because the alternative is probing a host that
//! asked not to be. The list is read from disk at every run, so an entry takes
//! effect at the next sweep; `without_excluded` documents that and the rest of
//! the limits `/about` has to state, and `read_exclusions` documents the path.

use serde::{Deserialize, Serialize};

/// One entry as the file may spell it.
///
/// Untagged rather than a struct with optional fields, because the four
/// registry files do not agree and are not written by the same hand:
/// `endpoints.toml` and `endpoints.container.toml` are hand-kept lists of bare
/// strings, and `seed-registry` writes tables. A struct would reject every bare
/// string and fail the next sweep on a file nobody edited.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Url(String),
    Described(RegistryEntry),
}

/// An entry with whatever the catalogue said about it.
///
/// `title` is ABSENT, not empty, for an endpoint that serves many datasets:
/// `datasets` carries the count instead, and the site shows the host. Picking
/// one of forty-two titles would assert something untrue about the server.
///
/// `Serialize` is here, not added in the task that first calls it, because a
/// struct and the derive that writes it belong together: `seed-registry`
/// serialises this shape with `toml::to_string_pretty`, and
/// `skip_serializing_if` on each optional field is what keeps an endpoint with
/// no domain from writing an empty `domain = ""` line into a file of hundreds
/// of entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasets: Option<u32>,
}

#[derive(Deserialize)]
struct EndpointFile {
    endpoint: Vec<Entry>,
}

/// Parse `endpoints.toml` and return its endpoint list, each entry once, with
/// no entry carrying credentials and no entry on an excluded host, in
/// first-seen order.
///
/// `excluded` is a parameter rather than something this function reads, so the
/// I/O sits at the edge, in each binary's `main`, where the path comes from a
/// flag. `read_exclusions` is what reads it, and it fails rather than returning
/// an empty list when it cannot.
///
/// Deduplicating before dropping credentials, not after, so a URL listed twice
/// produces one warning of each kind rather than two of the second. That
/// ordering is also why the two warnings name their positions differently:
/// `dedupe` reports `position` in the file's list, `without_credentials`
/// reports `deduped_position` in the list it was handed, and the second is not
/// a line in `endpoints.toml` whenever a duplicate came before it.
pub fn load_endpoints(toml_text: &str, excluded: &[Exclusion]) -> anyhow::Result<Vec<String>> {
    let file: EndpointFile = toml::from_str(toml_text)?;
    // The sweep path wants URLs and nothing else, so the extra fields stop
    // here. Every rule below this line judges the string, and none of them
    // has an opinion about a title.
    let urls: Vec<String> = file
        .endpoint
        .into_iter()
        .map(|e| match e {
            Entry::Url(u) => u,
            Entry::Described(d) => d.url,
        })
        .collect();
    let deduped = dedupe(&urls);
    let named = without_credentials(&deduped);
    // Before the two rules that judge the string itself, so an excluded host
    // is reported as excluded rather than as a documentation name or a bad
    // IRI: the one refusal made on a person's request is the one worth naming.
    // After `without_credentials` for that function's own reason, which is
    // that nothing downstream of it may log a password.
    let wanted = without_excluded(&named, excluded);
    let unreserved = without_reserved_names(&wanted);
    Ok(without_unpublishable_iris(&unreserved))
}

/// Every entry with what the catalogue said about it, unfiltered.
///
/// Deliberately not `load_endpoints`: that function applies the sweep's
/// policy -- dedupe, credentials, exclusions, reserved names -- and the site
/// needs a lookup table, not a sweep list. An endpoint the sweep refuses can
/// still appear in a stored run from before the refusal, and its row should
/// still find a name.
pub fn load_registry(toml_text: &str) -> anyhow::Result<Vec<RegistryEntry>> {
    let file: EndpointFile = toml::from_str(toml_text)?;
    Ok(file
        .endpoint
        .into_iter()
        .map(|e| match e {
            Entry::Url(url) => RegistryEntry { url, title: None, domain: None, datasets: None },
            Entry::Described(d) => d,
        })
        .collect())
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
///
/// Two further edges, both textual and both closed as of this note, so the list
/// above is not read as covering more than it does. An IPv4-MAPPED IPv6 literal
/// IS caught: `[::ffff:127.0.0.1]` and `[::ffff:169.254.169.254]` are decided as
/// the IPv4 addresses they map to, because `Ipv6Addr::is_loopback` is true only
/// for `::1`. And a trailing DNS root label IS stripped, so `localhost.` and
/// `127.0.0.1.` are refused like their undotted spellings.
///
/// Still open, and textual, so a future reader knows where the edge now is:
/// `.local` (RFC 6762 mDNS) and `.home.arpa` (RFC 8375) are reserved for names
/// that resolve on the prober's own link, and neither is refused. Neither
/// appears in the 2026-06-15 dump. They are left out because this arm is framed
/// around the two RFC 2606 local-use names and adding a third reservation is a
/// decision to take with a dump that carries one, not on a guess.
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
/// Why an IPv4 address is not one to send a stranger's dump at, or `None`.
///
/// Split out of `unroutable` so the IPv4-mapped IPv6 arm reaches the same
/// decision rather than a second copy of it: two copies would be one edit away
/// from `[::ffff:10.0.0.1]` and `10.0.0.1` being answered differently.
fn unroutable_v4(v4: std::net::Ipv4Addr) -> Option<&'static str> {
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
            std::net::IpAddr::V4(v4) => unroutable_v4(v4),
            // An IPv4-mapped literal is decided as the IPv4 address it maps to.
            // `Ipv6Addr::is_loopback` is true only for `::1`, and
            // `is_unique_local` and `is_unicast_link_local` do not see through
            // the mapping either, so `[::ffff:127.0.0.1]` and
            // `[::ffff:169.254.169.254]` would otherwise be seeded while
            // reaching 127.0.0.1 and the cloud metadata service on the wire.
            std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => unroutable_v4(v4),
                None => {
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
            },
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

/// One host this project does not probe, and why it is on the list.
///
/// `reason` is required and cannot be blank, so an exclusion cannot be
/// anonymous: somebody reading `registry/exclusions.toml` in a year has to be
/// able to tell why a host is on it, and "a person asked" with a date is a
/// perfectly good reason. Without that, removing an entry would be as
/// unaccountable as adding one.
#[derive(Debug, Deserialize)]
pub struct Exclusion {
    /// The host to leave alone, lowercased and with any DNS root label
    /// stripped by `parse_exclusions`, so it compares equal to what `host_of`
    /// returns for a URL.
    pub host: String,
    pub reason: String,
}

#[derive(Deserialize)]
struct ExclusionFile {
    /// `default` so a file whose last entry has been removed still parses. A
    /// file of nothing but comments is an empty exclusion list, not a broken
    /// one, and without this removing the final entry would fail every load in
    /// the crate.
    #[serde(default)]
    exclusion: Vec<Exclusion>,
}

/// Where the exclusion list lives, as both binaries' `--exclusions` default.
///
/// A path relative to the WORKING DIRECTORY, like `--endpoints`'s
/// `endpoints.toml` and `--metrics`'s `metrics.toml` beside it, so the tool
/// keeps one convention rather than growing a second one for this file. The
/// trap that comes with that is worth naming: run from anywhere but `prober/`,
/// this default names a file that is not there, and `read_exclusions` then
/// fails and says which path it tried. That is the intended outcome. An
/// operator running from elsewhere gives `--exclusions` an absolute path.
///
/// Not `include_str!` and not a path baked in at compile time. Compiling the
/// list in was tried and reverted: it made an entry take effect at the next
/// BUILD, so a deployment already running kept probing a host that had asked
/// not to be until somebody rebuilt and redeployed it. The whole point of the
/// mechanism is to honour that request promptly, and a courtesy channel that
/// takes a release cycle is a weaker promise than `/about` will imply. Baking
/// in `CARGO_MANIFEST_DIR` would be worse than either: it names the build
/// machine's checkout, which on a deployed binary is a path that may not exist
/// or, worse, may hold somebody else's file.
pub const DEFAULT_EXCLUSIONS: &str = "registry/exclusions.toml";

/// The exclusion list at `path`, or an error naming the path.
///
/// **Fails closed**, and that is the property that makes run-time reading safe.
/// A file that cannot be read is not an empty exclusion list: it is a process
/// that does not know what it was asked to leave alone. A deployment that
/// forgets to mount the file then stops loudly instead of quietly resuming a
/// sweep of every host that had asked not to be probed. Same reasoning as
/// `parse_exclusions` applies to a malformed entry, one step out.
///
/// The message names the path and says a relative one is resolved against the
/// working directory, because that is the one mistake this shape invites and an
/// operator cannot fix what the error does not name.
pub fn read_exclusions(path: &std::path::Path) -> anyhow::Result<Vec<Exclusion>> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "the exclusion list at {} could not be read ({error}), so nothing here knows which \
             hosts asked not to be probed, and refusing to continue is the only answer that \
             cannot probe one of them by mistake. A relative path is resolved against the \
             working directory: pass --exclusions an absolute path, or run from the directory \
             holding {}",
            path.display(),
            DEFAULT_EXCLUSIONS
        )
    })?;
    parse_exclusions(&text)
        .map_err(|error| anyhow::anyhow!("the exclusion list at {}: {error}", path.display()))
}

/// Parse an exclusion list, with every host normalised the way a URL's host is.
///
/// **An error here fails the load**, which is the one place in this module that
/// does not drop-with-a-warning. `dedupe`, `without_credentials`,
/// `without_reserved_names` and `without_unpublishable_iris` all fail open,
/// because the endpoint list is seeded from real-world dumps and refusing the
/// file would mean monitoring nothing. This one cannot: failing open means
/// probing a host that asked not to be, and a sweep that does not happen is a
/// smaller wrong than a sweep somebody asked us not to run.
///
/// A `host` that is really a URL is refused rather than kept as an exclusion
/// that matches nothing. That is the failure this check exists for: somebody
/// honours a request by pasting the URL out of their logs, every comparison
/// then misses, and the host is probed anyway with a file on disk saying it is
/// excluded. The same reasoning refuses an empty host.
pub fn parse_exclusions(toml_text: &str) -> anyhow::Result<Vec<Exclusion>> {
    let file: ExclusionFile = toml::from_str(toml_text).map_err(|error| {
        anyhow::anyhow!(
            "the exclusion list does not parse, so nothing can know which hosts to leave \
             alone. Every [[exclusion]] needs a host and a reason: {error}"
        )
    })?;
    let mut parsed: Vec<Exclusion> = Vec::with_capacity(file.exclusion.len());
    for Exclusion { host: written, reason } in file.exclusion {
        let host = exclusion_key(written.trim());
        if host.is_empty() {
            anyhow::bail!(
                "an exclusion with an empty host excludes nothing: name the host to leave \
                 alone, with no scheme and no port"
            );
        }
        // A URL carries at least one of these and a host carries none of them.
        // Space is in the list because a host cannot contain one either, and a
        // pasted line is as likely to arrive with one as with a scheme.
        if host.contains(['/', ':', '@', ' ']) {
            anyhow::bail!(
                "the exclusion {written:?} names a URL rather than a host: write the host on \
                 its own, so https://a.example/sparql becomes a.example. A URL here would \
                 match no endpoint, and the exclusion would quietly do nothing at all"
            );
        }
        if reason.trim().is_empty() {
            anyhow::bail!(
                "the exclusion for {host} has no reason, and an exclusion cannot be \
                 anonymous: say why the host is on the list. \"A person asked\" with a date \
                 is enough"
            );
        }
        parsed.push(Exclusion { host, reason });
    }
    Ok(parsed)
}

/// `host` lowercased with any DNS root label stripped, which is how a host in
/// the exclusion file and a host out of a URL are compared.
///
/// One function used on both sides rather than two: two copies would be one
/// edit away from an entry typed `Asked.Example.` never matching the URL it was
/// written for, which is an exclusion that silently does nothing.
fn exclusion_key(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// `endpoints` with every entry on an excluded host dropped, and one warning
/// per entry dropped naming the host and the reason it is excluded.
///
/// **Called by `load_endpoints`, so it holds on every path, AND by
/// `seed::candidates`.** It is the only rule in this module applied in both
/// places, and it needs both. `load_endpoints` is the one door a sweep comes
/// through, whatever file `--endpoints` names, so subtracting there is what
/// makes "an excluded host is never probed" true of a hand-written list and of
/// the seeded one alike. The seeder is not redundant with that: it regenerates
/// `registry/lod-cloud.toml` from the dump, so a host deleted from that file by
/// hand comes back at the next re-seed, and the committed artefact is public.
/// Leaving an excluded host in a checked-in list, on the strength of a filter
/// somewhere else, would publish the name of a host that asked to be left alone
/// and give a reader no way to see that the sweep skips it.
///
/// Unlike `without_unroutable_hosts`, there is no legitimate use to protect
/// here. A request to be excluded is a request made to THIS software, and this
/// software is what honours it; that is why the rule is on every path rather
/// than at the seam where a stranger's list becomes ours.
///
/// The subtraction is on the HOST, because the request behind an exclusion is
/// "stop probing us" and a host is what a person controls. Excluding one URL
/// would leave the `https://` spelling beside the `http://` one, and the port
/// and path variants, in every sweep. The comparison shares `host_of` with
/// `without_reserved_names` and `without_unroutable_hosts`, so it cannot drift
/// from the host the politeness gate groups by.
///
/// It is on the WHOLE host and not on a suffix. A suffix match would drop
/// `sub.asked.example` on an entry for `asked.example`, and hosts under one
/// domain routinely belong to different people: a request from one department
/// would silently remove every other department's endpoint from the sweep, with
/// nothing to notice it by. An operator who runs several names names them all.
///
/// The position is `filtered_position`, in the list this was handed, matching
/// `without_reserved_names` and not `dedupe`'s `position`.
///
/// What this does NOT do, all of it needed by `/about`:
///
/// - It does not watch a mailbox. An exclusion becomes real when a person adds
///   it to `registry/exclusions.toml`. There is no automation anywhere between
///   a request arriving and somebody editing the file, and this is the limit
///   that stays no matter how the file is read.
/// - It takes effect at the next SWEEP, not at the request. The file is read
///   from disk at every run, so no rebuild and no redeploy stands between an
///   entry and its being honoured, and a sweep already in flight finishes under
///   the list it started with.
/// - It does not retract anything already published. A run graph is immutable
///   and append-only, and `emit::subject_iri` puts the endpoint string inside
///   every subject, so an exclusion stops future sweeps and leaves past
///   measurements standing.
/// - It does not resolve names. A second name for the same server, or the same
///   server renamed, is a second exclusion, because nothing here asks a
///   resolver what an entry points at.
/// - It cannot exclude one path on a host and keep another. The granularity is
///   the name, which errs toward not probing.
/// - It publishes the host it excludes: the file is in a public repository. A
///   hashed entry would be checkable by nobody, including the person who asked.
pub fn without_excluded(endpoints: &[String], exclusions: &[Exclusion]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(endpoints.len());
    for (filtered_position, ep) in endpoints.iter().enumerate() {
        let host = exclusion_key(&host_of(ep));
        match exclusions.iter().find(|excluded| excluded.host == host) {
            Some(excluded) => tracing::warn!(
                host = %host,
                filtered_position,
                reason = %excluded.reason,
                "registry entry dropped: its host asked not to be probed, and \
                 registry/exclusions.toml records why"
            ),
            None => kept.push(ep.clone()),
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

    /// `load_endpoints` with no exclusions in force, which is what every test
    /// in this module except the two below is about.
    fn load_unrestricted(toml_text: &str) -> anyhow::Result<Vec<String>> {
        load_endpoints(toml_text, &[])
    }

    /// The shipped exclusion list, found through `CARGO_MANIFEST_DIR` rather
    /// than the default relative path, because a unit test's working directory
    /// is not something to depend on.
    fn shipped_exclusions() -> Vec<Exclusion> {
        read_exclusions(&shipped_exclusions_path()).expect("the shipped list must be readable")
    }

    fn shipped_exclusions_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_EXCLUSIONS)
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
            let loaded = load_unrestricted(
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
            let loaded = load_unrestricted(&format!(r#"endpoint = ["{kept}"]"#)).unwrap();
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
            let loaded = load_unrestricted(
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
            let loaded = load_unrestricted(&format!(r#"endpoint = ["{kept}"]"#)).unwrap();
            assert_eq!(loaded, v(&[kept]));
        });
        assert!(logs.is_empty(), "nothing was dropped, so nothing to warn about: {logs}");
    }

    #[test]
    fn the_shipped_registry_file_loads_and_is_already_unique() {
        let loaded = load_unrestricted(include_str!("../endpoints.toml")).unwrap();
        assert!(!loaded.is_empty(), "a registry nothing loads from sweeps nothing");
        let unique: std::collections::BTreeSet<&String> = loaded.iter().collect();
        assert_eq!(unique.len(), loaded.len(), "the file already lists each URL once");
    }

    /// The synthetic endpoint is FIRST, and that is a property of the list
    /// rather than a detail of it.
    ///
    /// It is the only entry whose right answers are known, which makes it the
    /// control: a metric disagreeing with it is wrong about the metric, not
    /// about the endpoint. Endpoints are probed in file order, so first means
    /// the control is measured before anything is concluded from the rest.
    ///
    /// Asserted as "loopback comes first" and not as a URL, because the
    /// previous version of this test pinned `qlever.dev` by name and broke the
    /// moment the list was replaced on 2026-09-05, having checked nothing
    /// anyone cared about in the meantime.
    #[test]
    fn the_shipped_registry_leads_with_the_local_control() {
        let loaded = load_unrestricted(include_str!("../endpoints.toml")).unwrap();
        let first = loaded.first().expect("the registry lists at least one endpoint");
        assert!(
            first.contains("127.0.0.1") || first.contains("localhost"),
            "the local control belongs first in the sweep order: {first}"
        );
    }

    #[test]
    fn a_file_listing_one_url_twice_loads_it_once() {
        let loaded = load_unrestricted(
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
            // The DNS root label. `unroutable` strips it, and without that
            // strip `127.0.0.1.` parses as no `IpAddr` and reaches a sweep.
            ("http://127.0.0.1./sparql", "loopback"),
            // IPv4-mapped IPv6, which `Ipv6Addr::is_loopback` and its two
            // siblings do not see through: `is_loopback` is true only for
            // `::1`. The address on the wire is still 127.0.0.1, and the last
            // of these is the cloud metadata URL the link-local arm exists to
            // refuse.
            ("http://[::ffff:127.0.0.1]/sparql", "loopback"),
            ("http://[::ffff:127.0.0.1]:8890/sparql", "loopback"),
            ("http://[0:0:0:0:0:ffff:7f00:1]/sparql", "loopback"),
            ("http://[::ffff:10.0.0.1]/sparql", "private"),
            ("http://[::ffff:169.254.169.254]/latest/meta-data/", "link-local"),
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
    /// dump contains two of them, `http://example.org` and
    /// `http://www.example.org`, and it marks BOTH `OK`, which is the strongest
    /// form of the point: the dump's own field would have admitted a name that
    /// exists to receive no traffic.
    ///
    /// One case asserts the whole warning sentence and not just the `RFC 2606`
    /// token. A test that matches only the token cannot see a formatting
    /// mistake in the sentence around it, and this branch shipped a warning with
    /// eighteen spaces in the middle of it for exactly that reason.
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

        let logs = logs_of(|| {
            assert!(without_reserved_names(&v(&["http://example.org"])).is_empty());
        });
        assert!(
            logs.contains(
                "registry entry dropped: its host is under a second-level domain RFC 2606 \
                 reserves for documentation, so no service can be there to measure"
            ),
            "the whole sentence, not just the token, so a formatting mistake inside it is \
             visible: {logs}"
        );
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
        let loaded = load_unrestricted(&format!("endpoint = [{local:?}]")).unwrap();
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
        // The last two carry the DNS root label, which spells one host two
        // ways. Without `unroutable`'s trailing-dot strip the TLD arm never
        // fires on them, because `host.rsplit('.').next()` is then the empty
        // string, and the seeder keeps them.
        for url in [
            "http://localhost:3030/query",
            "http://anything.test/sparql",
            "http://localhost./query",
            "http://anything.test./sparql",
        ] {
            let loaded = load_unrestricted(&format!("endpoint = [{url:?}]")).unwrap();
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
            let loaded = load_unrestricted(
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
            let loaded = load_unrestricted(
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
        assert!(load_unrestricted("nonsense = 1").is_err());
        assert!(load_unrestricted("endpoint = \"not a list\"").is_err());
    }

    /// A one-host exclusion list written inline, so a test that is about the
    /// RULE does not depend on what the shipped file happens to hold today.
    fn excluding(host: &str) -> Vec<Exclusion> {
        parse_exclusions(&format!(
            "[[exclusion]]\nhost = {host:?}\nreason = \"a person asked, 2026-08-25\"\n"
        ))
        .expect("a host and a reason is a well formed exclusion")
    }

    /// The exclusion is on the HOST, so every URL on that name goes. The
    /// request behind an exclusion is "stop probing us", and a host is what a
    /// person controls: excluding one URL spelling would leave the `https://`
    /// one beside the `http://` one, and the port and path variants, in every
    /// sweep.
    ///
    /// `sparqlwatch-exclusion-worked-example` is not used here on purpose.
    /// These hosts are refused by no other rule either, so a green here is
    /// about `without_excluded` and not about the shipped file.
    #[test]
    fn every_url_on_an_excluded_host_is_dropped() {
        let excluded = excluding("asked.test-host");
        for url in [
            "http://asked.test-host/sparql",
            "https://asked.test-host/sparql",
            "https://asked.test-host:8890/sparql",
            "https://asked.test-host/other/path?query=x",
            "https://ASKED.TEST-HOST/sparql",
            // The DNS root label spells one host two ways, as it does for
            // `unroutable` and `reserved_name`.
            "https://asked.test-host./sparql",
        ] {
            let logs = logs_of(|| {
                let kept = without_excluded(&v(&[url]), &excluded);
                assert!(kept.is_empty(), "{url} must not reach a sweep, kept {kept:?}");
            });
            assert!(logs.contains("asked.test-host"), "the warning must name the host: {logs}");
            assert_eq!(logs.matches("WARN").count(), 1, "one warning per entry: {logs}");
        }
    }

    /// The match is on the WHOLE host and not on a suffix or a substring.
    ///
    /// A substring check would drop `notasked.test-host`, which is somebody
    /// else's server. A suffix check would drop `sub.asked.test-host`, and that
    /// is the dangerous direction here rather than the safe one: hosts under
    /// one domain routinely belong to different people, so a request from one
    /// department would silently remove every other department's endpoint from
    /// the sweep with nothing to notice it by. An operator who runs several
    /// names names them all.
    #[test]
    fn a_host_that_merely_resembles_an_excluded_one_is_kept() {
        let excluded = excluding("asked.test-host");
        let urls = v(&[
            "https://notasked.test-host/sparql",
            "https://asked.test-hosting/sparql",
            "https://sub.asked.test-host/sparql",
        ]);
        let logs = logs_of(|| {
            assert_eq!(without_excluded(&urls, &excluded), urls);
        });
        assert!(logs.is_empty(), "nothing was excluded, so nothing to warn about: {logs}");
    }

    /// The reason travels into the warning. An operator who finds an endpoint
    /// missing from a sweep has to be able to tell from the log why it is
    /// missing, and "excluded" without a reason is the anonymous exclusion this
    /// file's required field exists to prevent.
    #[test]
    fn the_warning_carries_the_reason_the_exclusion_was_given() {
        let excluded = excluding("asked.test-host");
        let logs = logs_of(|| {
            let kept = without_excluded(&v(&["https://asked.test-host/sparql"]), &excluded);
            assert!(kept.is_empty());
        });
        assert!(
            logs.contains("a person asked, 2026-08-25"),
            "the reason has to reach the log: {logs}"
        );
    }

    /// The wiring, on every path. `load_endpoints` is the only door a sweep
    /// comes through, whatever file `--endpoints` names, so subtracting there
    /// is what makes "an excluded host is never probed" true of a hand-written
    /// list and of the seeded one alike.
    ///
    /// The list is a parameter and not something this function reads, so a test
    /// states the policy it runs under and no test depends on what the shipped
    /// file happens to hold. `tests/binary.rs` is where the real binary is
    /// shown to pass the file's contents in.
    #[test]
    fn load_endpoints_subtracts_the_exclusion_list_it_is_given() {
        let logs = logs_of(|| {
            let loaded = load_endpoints(
                r#"endpoint = ["https://asked.test-host/sparql", "https://b/sparql"]"#,
                &excluding("asked.test-host"),
            )
            .unwrap();
            assert_eq!(
                loaded,
                v(&["https://b/sparql"]),
                "an excluded host must not reach a sweep from any endpoint list"
            );
        });
        assert_eq!(logs.matches("WARN").count(), 1, "one warning per entry: {logs}");
        assert!(logs.contains("asked not to be probed"), "say why it went: {logs}");
    }

    /// An exclusion list that cannot be READ fails the load, naming the path.
    ///
    /// The file is read at run time, so "the file is not where this process was
    /// told to look" is a state that exists, and it is exactly the state in
    /// which a deployment would otherwise probe every host that had asked not
    /// to be. Failing closed turns a forgotten mount into a loud stop instead
    /// of a quiet resumption of probing, which is the same reasoning
    /// `parse_exclusions` applies to a malformed entry, one step out.
    #[test]
    fn an_exclusion_list_that_cannot_be_read_is_an_error_naming_the_path() {
        let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("registry/no-such-exclusion-list.toml");
        let error = read_exclusions(&missing)
            .expect_err("a list that cannot be read is not an empty list")
            .to_string();
        assert!(
            error.contains("no-such-exclusion-list.toml"),
            "name the path that could not be read: {error}"
        );
        assert!(
            error.contains("working directory"),
            "a relative path is the trap, so the message has to name it: {error}"
        );
        read_exclusions(&shipped_exclusions_path()).expect("the shipped list is readable");
    }

    /// An exclusion cannot be anonymous. A missing `reason` is a parse error
    /// and a blank one is refused by name, because a file whose entries do not
    /// say why a host is on it cannot be maintained by whoever reads it next:
    /// removing an entry would be as unaccountable as adding one.
    #[test]
    fn an_exclusion_without_a_reason_is_a_load_error() {
        let missing = parse_exclusions("[[exclusion]]\nhost = \"a.test-host\"\n")
            .expect_err("an exclusion with no reason is anonymous")
            .to_string();
        assert!(missing.contains("reason"), "name the missing field: {missing}");

        for blank in ["", "   ", "\n\t"] {
            let error = parse_exclusions(&format!(
                "[[exclusion]]\nhost = \"a.test-host\"\nreason = {blank:?}\n"
            ))
            .expect_err("a blank reason is an anonymous exclusion spelled differently")
            .to_string();
            assert!(error.contains("anonymous"), "say what is wrong: {error}");
            assert!(error.contains("a.test-host"), "name the entry: {error}");
        }
    }

    /// A `host` that is really a URL is a load error and not an exclusion that
    /// matches nothing.
    ///
    /// This is the failure mode that matters: somebody honours a request by
    /// pasting the URL from their logs, every comparison then misses, and the
    /// host is probed anyway with a file on disk saying it is excluded. Failing
    /// the load is the only outcome that cannot be mistaken for success.
    #[test]
    fn an_exclusion_naming_a_url_rather_than_a_host_is_a_load_error() {
        for host in [
            "https://a.test-host/sparql",
            "a.test-host/sparql",
            "a.test-host:8890",
            "user@a.test-host",
        ] {
            let error = parse_exclusions(&format!(
                "[[exclusion]]\nhost = {host:?}\nreason = \"a person asked, 2026-08-25\"\n"
            ))
            .expect_err("a URL here would quietly match nothing")
            .to_string();
            assert!(error.contains("rather than a host"), "{host} must be refused: {error}");
            assert!(error.contains(host), "name the value: {error}");
        }

        let empty = parse_exclusions("[[exclusion]]\nhost = \"\"\nreason = \"a person asked\"\n")
            .expect_err("an exclusion with no host excludes nothing")
            .to_string();
        assert!(empty.contains("host"), "name what is missing: {empty}");
    }

    /// An exclusion list that does not parse fails the LOAD.
    ///
    /// The deliberate difference from every other rule in this module, all of
    /// which drop an entry with a warning and carry on: those fail open because
    /// a real-world dump carries junk and refusing the file would mean
    /// monitoring nothing. This one cannot fail open, because failing open here
    /// means probing a host that asked not to be. A sweep that does not happen
    /// is a smaller wrong than a sweep somebody asked us not to run.
    #[test]
    fn a_broken_exclusion_list_fails_the_load_rather_than_being_ignored() {
        assert!(parse_exclusions("[[exclusion]]\nhost = 7\n").is_err());
        assert!(parse_exclusions("exclusion = \"not a list of tables\"").is_err());
        assert!(parse_exclusions("[[exclusion").is_err());
        parse_exclusions(&std::fs::read_to_string(shipped_exclusions_path()).unwrap())
            .expect("the shipped exclusion list must parse");
    }

    /// A list with no entries is a list. When the last exclusion is ever
    /// removed the file still has to load: `#[serde(default)]` is what makes a
    /// file of nothing but comments parse, and without it removing the final
    /// entry would fail every load in the crate.
    #[test]
    fn an_exclusion_list_with_no_entries_loads_and_excludes_nothing() {
        let none = parse_exclusions("# every entry removed again\n")
            .expect("a file with no entries is an empty exclusion list");
        assert!(none.is_empty());
        let urls = v(&["https://a/sparql", "https://b/sparql"]);
        assert_eq!(without_excluded(&urls, &none), urls);
    }

    /// A host is a host whatever its case, and whether or not it carries the
    /// DNS root label. Normalised on the FILE's side too, so an entry typed
    /// `Asked.Test-Host.` is not an exclusion that silently matches nothing.
    #[test]
    fn an_exclusion_is_normalised_the_way_a_url_host_is() {
        let excluded = excluding("Asked.Test-Host.");
        let kept = without_excluded(&v(&["https://asked.test-host/sparql"]), &excluded);
        assert!(kept.is_empty(), "one host written two ways, kept {kept:?}");
    }

    /// The shipped file's own shape. Every entry names a host and says why, and
    /// the list is not empty, because an empty list is applied vacuously and the
    /// wiring test above would then prove nothing.
    #[test]
    fn every_shipped_exclusion_names_a_host_and_a_reason() {
        let list = shipped_exclusions();
        assert!(!list.is_empty(), "an empty list is subtracted vacuously");
        for entry in &list {
            assert!(!entry.host.is_empty(), "an exclusion with no host excludes nothing");
            assert!(
                !entry.reason.trim().is_empty(),
                "{} is on the list anonymously",
                entry.host
            );
        }
    }

    // These three tests, and only these three, use single-label hosts rather
    // than `example.org`/`a.example`: that TLD and second-level domain are
    // exactly what `without_reserved_names` refuses, per the placeholder-host
    // note above, and these tests are about the shape of the file parsing,
    // not about that filter. `load_registry_keeps_what_load_endpoints_drops`
    // below does not go through `load_endpoints`'s filters at all, so it keeps
    // `example.org` to look like a real catalogue entry.

    #[test]
    fn a_bare_string_list_still_loads() {
        // endpoints.toml and endpoints.container.toml are hand-written in this
        // shape and are not regenerated. A reader that only understood the new
        // shape would fail the next sweep on a file nobody touched.
        let text = r#"endpoint = ["https://b/sparql"]"#;
        let got = load_endpoints(text, &[]).expect("the bare form must still parse");
        assert_eq!(got, vec!["https://b/sparql".to_string()]);
    }

    #[test]
    fn a_table_list_loads_and_yields_its_urls() {
        let text = r#"
[[endpoint]]
url = "https://b/sparql"
title = "Example"
domain = "government"
"#;
        let got = load_endpoints(text, &[]).expect("the table form must parse");
        assert_eq!(got, vec!["https://b/sparql".to_string()]);
    }

    #[test]
    fn the_two_shapes_may_be_mixed_in_one_file() {
        // Not a shape we write, but a shape a half-finished hand edit produces,
        // and refusing it with a serde error names neither line.
        let text = r#"
endpoint = ["https://a/sparql", { url = "https://b/sparql", title = "B" }]
"#;
        let got = load_endpoints(text, &[]).expect("a mixed list must parse");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn load_registry_keeps_what_load_endpoints_drops() {
        let text = r#"
[[endpoint]]
url = "https://example.org/sparql"
title = "Example"
domain = "government"

[[endpoint]]
url = "https://many.example/sparql"
datasets = 42
"#;
        let got = load_registry(text).expect("the registry form must parse");
        assert_eq!(got[0].title.as_deref(), Some("Example"));
        assert_eq!(got[0].domain.as_deref(), Some("government"));
        assert_eq!(got[1].title, None);
        assert_eq!(got[1].datasets, Some(42));
    }
}
