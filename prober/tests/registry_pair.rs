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
