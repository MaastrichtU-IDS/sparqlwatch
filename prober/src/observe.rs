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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub status: Option<u16>,
    pub cors: bool,
    pub boolean: Option<bool>,
    pub bindings: Vec<String>,
    pub body_kind: BodyKind,
    /// The retained response body, truncated. Kept because "why did this
    /// endpoint score badly" is the first question a provider asks, and
    /// because the declaration parser reads it.
    pub body: Option<String>,
    pub elapsed_ms: u64,
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
            elapsed_ms,
            error: Some(error),
        }
    }
}
