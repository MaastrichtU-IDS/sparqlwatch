use serde::{Deserialize, Serialize};

/// What the body actually was, independent of the status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyKind {
    SparqlJson,
    Rdf,
    Html,
    Other,
    None,
}

/// Raw evidence from one request. Deliberately carries no judgement: the
/// resolver decides what it means.
///
/// Every `Option` field carries `#[serde(default)]`. Nothing in the tree
/// persists or reads back an `Observation` today, so this is unreachable, but
/// raw-evidence retention is a deferred item on this project: the day it
/// lands, a record serialized before a field existed must still deserialize
/// (as `None`) rather than fail outright. One line now beats a confusing
/// failure later, and it costs nothing while there is no archive to protect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    #[serde(default)]
    pub status: Option<u16>,
    pub cors: bool,
    #[serde(default)]
    pub boolean: Option<bool>,
    pub bindings: Vec<String>,
    pub body_kind: BodyKind,
    /// The retained response body, truncated. Kept because "why did this
    /// endpoint score badly" is the first question a provider asks, and
    /// because the declaration parser reads it.
    #[serde(default)]
    pub body: Option<String>,
    /// The URL the fetch ended on after redirects, when this observation came
    /// from one. `None` for every probe that is not a fetch, and for a fetch
    /// that never got a response. A description commonly states its
    /// post-redirect URL as `sd:endpoint` while the registry holds the
    /// pre-redirect one, so scoping a declaration to the service we probed
    /// needs both strings, not just the one we asked for.
    #[serde(default)]
    pub final_url: Option<String>,
    /// The response's `Content-Type` header, lowercased, when one was sent.
    /// `parse_declarations` needs this to pick the right RDF syntax; a body
    /// parsed under the wrong assumed format silently yields near-empty
    /// declarations, indistinguishable from a genuinely empty description.
    #[serde(default)]
    pub content_type: Option<String>,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
}

impl Observation {
    pub fn failed(error: String, elapsed_ms: u64) -> Self {
        Self {
            status: None,
            cors: false,
            boolean: None,
            bindings: Vec::new(),
            body_kind: BodyKind::None,
            body: None,
            final_url: None,
            content_type: None,
            elapsed_ms,
            error: Some(error),
        }
    }
}
