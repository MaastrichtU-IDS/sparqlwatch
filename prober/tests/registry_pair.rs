//! The two registries must differ in exactly one way.
//!
//! `endpoints.toml` names the synthetic endpoint on loopback, which is where it
//! listens on the host. `endpoints.container.toml` names it `synthetic:9200`,
//! the compose service name, because loopback inside a container is that
//! container rather than the machine.
//!
//! Every other entry has to match. A sweep run from the container registry
//! after somebody added an endpoint only to the host one would quietly cover
//! less than the sweep beside it, and two run graphs would disagree about which
//! endpoints exist for a reason nothing in the store records.

use sparqlwatch_prober::registry::load_endpoints;

/// The one permitted difference, as a pair of authorities.
const HOST_ONLY: &str = "127.0.0.1:9200";
const CONTAINER_ONLY: &str = "synthetic:9200";

fn normalise(urls: &[String]) -> Vec<String> {
    urls.iter()
        .map(|u| u.replace(CONTAINER_ONLY, HOST_ONLY))
        .collect()
}

#[test]
fn the_two_registries_agree_on_every_endpoint_but_the_local_control() {
    let host = load_endpoints(include_str!("../endpoints.toml"), &[]).unwrap();
    let container = load_endpoints(include_str!("../endpoints.container.toml"), &[]).unwrap();
    assert_eq!(
        normalise(&host),
        normalise(&container),
        "the host and container registries have drifted; every entry but the \
         synthetic endpoint's host must match"
    );
}

/// The difference is REAL, not accidentally absent. If somebody "fixed" the
/// drift by copying the host file over the container one, the test above would
/// pass while a containerised sweep recorded the local control as unreachable.
#[test]
fn the_container_registry_names_the_service_and_not_loopback() {
    // Asked of the PARSED urls and not of the file text. The first version read
    // the raw bytes and failed on each file's own header comment, which
    // explains the difference and therefore names both spellings. A comment is
    // not a registry entry.
    let host = load_endpoints(include_str!("../endpoints.toml"), &[]).unwrap();
    let container =
        load_endpoints(include_str!("../endpoints.container.toml"), &[]).unwrap();

    assert!(
        container.iter().any(|u| u.contains(CONTAINER_ONLY)),
        "the container registry must name the compose service: {container:?}"
    );
    assert!(
        !container.iter().any(|u| u.contains(HOST_ONLY)),
        "loopback inside a container is that container: {container:?}"
    );
    assert!(
        host.iter().any(|u| u.contains(HOST_ONLY)),
        "the host registry names loopback: {host:?}"
    );
    assert!(!host.iter().any(|u| u.contains(CONTAINER_ONLY)), "{host:?}");
}

/// Neither registry may list a url twice.
///
/// `load_endpoints` already drops a repeat and WARNs, so a duplicate costs
/// nothing at sweep time -- which is exactly why two of them sat in both files
/// long enough to be warned about on every hourly sweep for weeks. A warning
/// that fires every hour and changes nothing trains a reader to skip warnings,
/// and the next one will be about something that matters.
///
/// Asked of the FILE and not of `load_endpoints`, which returns the
/// deduplicated list: asking the loader would compare a set against itself and
/// pass forever.
#[test]
fn neither_registry_lists_an_endpoint_twice() {
    /// Mirrors the loader's own `Entry`, which is private: an entry is a bare
    /// url or a table carrying one. Both spellings name an endpoint, so both
    /// count -- listing a url as a string and again as an inactive record is
    /// the same duplicate wearing two hats.
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Url(String),
        Described { url: String },
    }

    impl Entry {
        fn url(&self) -> &str {
            match self {
                Entry::Url(url) | Entry::Described { url } => url,
            }
        }
    }

    #[derive(serde::Deserialize)]
    struct Listed {
        endpoint: Option<Vec<Entry>>,
    }

    for (name, text) in [
        ("endpoints.toml", include_str!("../endpoints.toml")),
        (
            "endpoints.container.toml",
            include_str!("../endpoints.container.toml"),
        ),
    ] {
        let listed: Listed = toml::from_str(text).expect("the registry parses");
        let entries = listed.endpoint.unwrap_or_default();
        let mut seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for entry in &entries {
            *seen.entry(entry.url()).or_insert(0) += 1;
        }
        let repeated: Vec<String> = seen
            .iter()
            .filter(|(_, &count)| count > 1)
            .map(|(url, count)| format!("{url} ({count}x)"))
            .collect();
        assert!(
            repeated.is_empty(),
            "{name} lists {} url(s) more than once: {}. Probing is unaffected -- \
             the loader drops the repeat -- but the sweep warns about it every \
             hour. Delete the later entry, keeping the one whose surrounding \
             comment explains why the endpoint is listed.",
            repeated.len(),
            repeated.join(", ")
        );
    }
}
