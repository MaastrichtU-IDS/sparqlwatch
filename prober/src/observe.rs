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

/// One property's row from a class profile.
///
/// Counts rather than a verdict, because a profile is not a measurement: see
/// Ruling 2 in docs/superpowers/specs/2026-08-29-content-profiles-design.md. The
/// caller turns these into a `ContentProfile` fact and applies no threshold, so
/// the frequency a reader wants is `subjects` over the rdf:type row's
/// `subjects`, computed by whoever wants it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRow {
    /// The predicate, as an IRI.
    pub property: String,
    /// How many DISTINCT subjects in the sample carry it. Distinct, so a
    /// multi-valued property cannot exceed the denominator and produce a
    /// frequency above 1.0.
    pub subjects: u64,
    /// How many distinct datatypes its objects had. Carried beside
    /// `any_datatype` so a property with mixed datatypes is VISIBLE as mixed
    /// rather than silently reduced to whichever one the store returned first.
    pub datatypes: u64,
    /// One of the datatypes seen, or the string "IRI" when the object was one.
    /// `None` when the endpoint bound nothing for it.
    pub any_datatype: Option<String>,
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
    /// The `access-control-allow-origin` header's VALUE, on a preflight.
    /// `cors` above records only presence, and presence is not a grant: a
    /// value that is neither `*` nor our own origin grants somebody else, and
    /// publishing that as ours would be a confident wrong answer. `None` for
    /// every probe that is not a preflight, and when no such header was sent.
    #[serde(default)]
    pub allow_origin: Option<String>,
    /// The `access-control-allow-methods` header's value, on a preflight.
    /// `None` both when the header was absent (which permits a simple GET,
    /// the header being optional) and for every non-preflight probe; the
    /// resolver only consults it for the preflight kind, so the two cannot be
    /// confused.
    #[serde(default)]
    pub allow_methods: Option<String>,
    /// The `access-control-allow-headers` header's value, on a preflight.
    /// Recorded as evidence rather than judged: the preflight asks about
    /// `content-type`, and an operator debugging a refusal wants to see what
    /// came back.
    #[serde(default)]
    pub allow_headers: Option<String>,
    /// The grouped rows of a class profile, when this observation came from one.
    ///
    /// `None` for every probe that is not a profile, and for a profile the
    /// endpoint refused. That second case is the load-bearing one: a REFUSAL
    /// must not arrive as an empty profile, because an empty profile says the
    /// class carries no properties and nothing observed that. Same distinction
    /// the six-verdict vocabulary draws between `absent` and `indeterminate`.
    #[serde(default)]
    pub profile: Option<Vec<ProfileRow>>,
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
            allow_origin: None,
            allow_methods: None,
            allow_headers: None,
            profile: None,
            elapsed_ms,
            error: Some(error),
        }
    }
}
