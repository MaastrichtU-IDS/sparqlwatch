//! Reduce a fetched RDF graph (a SPARQL service description, in practice) to
//! the small set of facts it declares about itself.
//!
//! This module decides nothing. It has no notion of "good" or "bad": it only
//! reports what predicates and objects appeared, so that a later stage can
//! compare a claim against an observation. The finding motivating this
//! module is that the claim and the observation routinely disagree -- see
//! `prober/tests/declare.rs` for the concrete numbers from the survey.

use oxrdf::{NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeSet, HashSet};

const SD_FEATURE: &str = "http://www.w3.org/ns/sparql-service-description#feature";
const SD_EXTENSION_FUNCTION: &str = "http://www.w3.org/ns/sparql-service-description#extensionFunction";
const SD_SUPPORTED_LANGUAGE: &str = "http://www.w3.org/ns/sparql-service-description#supportedLanguage";
const SD_DEFAULT_DATASET: &str = "http://www.w3.org/ns/sparql-service-description#defaultDataset";
const SD_GRAPH: &str = "http://www.w3.org/ns/sparql-service-description#graph";
const SD_DEFAULT_ENTAILMENT_REGIME: &str =
    "http://www.w3.org/ns/sparql-service-description#defaultEntailmentRegime";
const VOID_CLASS_PARTITION: &str = "http://rdfs.org/ns/void#classPartition";
const VOID_EXAMPLE_RESOURCE: &str = "http://rdfs.org/ns/void#exampleResource";
const VOID_PROPERTY_PARTITION: &str = "http://rdfs.org/ns/void#propertyPartition";
const SD_ENDPOINT: &str = "http://www.w3.org/ns/sparql-service-description#endpoint";

/// Linking predicates: a service's subtree includes what it points at.
/// `defaultGraph` and `namedGraph` are here because real descriptions hang
/// VoID partitions off them, not only off `defaultDataset`.
const LINKING: [&str; 6] = [
    "http://www.w3.org/ns/sparql-service-description#defaultDataset",
    "http://www.w3.org/ns/sparql-service-description#availableGraphs",
    "http://www.w3.org/ns/sparql-service-description#namedGraph",
    "http://www.w3.org/ns/sparql-service-description#defaultGraph",
    "http://www.w3.org/ns/sparql-service-description#graph",
    "http://www.w3.org/ns/sparql-service-description#graphCollection",
];

/// What an endpoint claims about itself, reduced from a fetched graph. Every
/// field is a fact about the graph's content, never a judgement about
/// whether the claim is adequate, honest, or matched by reality -- that
/// comparison is `resolve()`'s job alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declarations {
    /// Object IRIs of `sd:feature` triples.
    pub features: BTreeSet<String>,
    /// Object IRIs of `sd:extensionFunction` triples.
    pub extension_functions: BTreeSet<String>,
    /// Object IRIs of `sd:supportedLanguage` triples.
    pub languages: BTreeSet<String>,
    /// Total triples successfully parsed before any error.
    pub triples: usize,
    /// `sd:defaultDataset` or `sd:graph` appeared at least once.
    pub names_dataset: bool,
    /// `void:classPartition` or `void:propertyPartition` appeared at least once.
    pub has_void_partitions: bool,
    /// `sd:defaultEntailmentRegime` appeared at least once.
    pub has_entailment: bool,
    /// `void:exampleResource` appeared at least once. One of the three things
    /// the spec's level 4 recognises, alongside an entailment regime and
    /// extension functions: naming a resource a client can actually
    /// dereference is a description doing more than describing itself.
    pub has_example_resources: bool,
}

impl Declarations {
    /// The declarations of a graph that named nothing -- absent, unfetched,
    /// or fetched and found empty. Distinct in meaning but identical in
    /// shape to what a malformed body parses to; callers that need to tell
    /// "nothing to fetch" from "fetched garbage" must track that separately.
    pub fn empty() -> Self {
        Self::default()
    }

    /// True if `iri` appears as a declared feature, extension function, or
    /// supported language. This is the one query a caller needs to ask
    /// "did the endpoint claim this capability" -- it does not ask whether
    /// the endpoint actually has it.
    pub fn declares(&self, iri: &str) -> bool {
        self.features.contains(iri) || self.extension_functions.contains(iri) || self.languages.contains(iri)
    }
}

/// Parse `body` into `Declarations`. `content_type` is the HTTP
/// `Content-Type` the body was served under, if any; an absent or
/// unrecognised value falls back to Turtle, since that is the format almost
/// every service description is actually served as, `Content-Type` header
/// or no. `endpoint` is the URL we probed, and it decides which service in
/// the document the capability sets are read from (see `scope_of`).
///
/// A syntax error partway through the body is not a failure of this
/// function: whatever was parsed before the error is returned as-is, since a
/// partial graph is still evidence and the caller has no better option than
/// "what we could read". This function never panics and never returns an
/// `Err` -- there is nothing for a caller to do with either that "collect
/// what you can" does not already cover.
pub fn parse_declarations(body: &str, content_type: Option<&str>, endpoint: &str) -> Declarations {
    parse_declarations_for(body, content_type, &[endpoint])
}

/// As `parse_declarations`, but matching the document's `sd:endpoint`
/// statements against several URLs for the same service. The caller passes
/// the URL it probed and, when a fetch was redirected, the URL it landed on:
/// a description commonly states the post-redirect URL (https, canonical
/// host) while the registry holds the one we asked for, and both name the
/// same service.
pub fn parse_declarations_for(body: &str, content_type: Option<&str>, endpoints: &[&str]) -> Declarations {
    let format = content_type
        .and_then(RdfFormat::from_media_type)
        .unwrap_or(RdfFormat::Turtle);

    // Collected rather than streamed, because scoping cannot be decided
    // incrementally: the `sd:endpoint` triple that says which service a
    // subject is may arrive after the declarations it governs. Memory is
    // bounded by the caller's 256 KiB body cap (`MAX_BODY` in `client.rs`),
    // not unbounded, and `body` is already fully in memory here.
    let mut quads: Vec<Quad> = Vec::new();
    for result in RdfParser::from_format(format).for_reader(body.as_bytes()) {
        match result {
            Ok(q) => quads.push(q),
            // A syntax error ends the parse here; everything collected up
            // to this point is kept rather than discarded.
            Err(_) => break,
        }
    }

    let mut d = Declarations::empty();

    // Pass one: the grade, UNSCOPED. These fields answer "how informative is
    // the document this operator published", and an operator who published
    // one rich document covering two services published a rich document.
    // Scoping them would report a level-4 description as a stub whenever the
    // probed URL did not match, which is a false assertive claim about the
    // publication rather than about the service.
    d.triples = quads.len();
    for quad in &quads {
        match quad.predicate.as_str() {
            SD_DEFAULT_DATASET | SD_GRAPH => d.names_dataset = true,
            SD_DEFAULT_ENTAILMENT_REGIME => d.has_entailment = true,
            VOID_EXAMPLE_RESOURCE => d.has_example_resources = true,
            VOID_CLASS_PARTITION | VOID_PROPERTY_PARTITION => d.has_void_partitions = true,
            _ => {}
        }
    }

    // Pass two: the capability sets, SCOPED. These are claims about one
    // service, so they may only be read from the service we probed.
    let scope = scope_of(&quads, endpoints);
    for quad in &quads {
        if scope.as_ref().is_some_and(|in_scope| !in_scope.contains(&quad.subject)) {
            continue;
        }
        // Predicate first, so a document full of unrelated triples costs no
        // allocation here.
        let set = match quad.predicate.as_str() {
            SD_FEATURE => &mut d.features,
            SD_EXTENSION_FUNCTION => &mut d.extension_functions,
            SD_SUPPORTED_LANGUAGE => &mut d.languages,
            _ => continue,
        };
        // A capability is an IRI. A literal or blank node in that position
        // names nothing a metric's `declared_by` could ever match.
        if let Term::NamedNode(object) = &quad.object {
            set.insert(object.as_str().to_string());
        }
    }
    d
}

/// Which subjects the capability sets may be read from.
///
/// `None` means no scope at all: read the whole document. `Some(set)` means
/// read only those subjects, and an empty set therefore yields no
/// capabilities. The four cases:
///
/// - No `sd:endpoint` triple anywhere: **no scope**. Most real descriptions,
///   including the 21 byte-identical Virtuoso stubs in the survey, state no
///   endpoint; scoping those to nothing would turn every one of them into a
///   false `undeclared`.
/// - Some `sd:endpoint` matches one of `endpoints` under `same_endpoint`:
///   **scope to those subjects**, expanded transitively through `LINKING`.
/// - Endpoints are stated, none matches, and the document describes exactly
///   one service: **no scope**. We fetched this document from the endpoint we
///   are probing and it describes one service; the URL disagreement is theirs.
/// - Endpoints are stated, none matches, and the document describes more than
///   one: **empty scope**. Crediting one of several services at random is
///   exactly the leak this function exists to close. The grade is unaffected,
///   because the grade is not scoped.
fn scope_of(quads: &[Quad], endpoints: &[&str]) -> Option<HashSet<NamedOrBlankNode>> {
    let mut services: HashSet<&NamedOrBlankNode> = HashSet::new();
    let mut matched: HashSet<NamedOrBlankNode> = HashSet::new();
    for quad in quads {
        if quad.predicate.as_str() != SD_ENDPOINT {
            continue;
        }
        // A stated endpoint counts towards "how many services does this
        // document describe" whatever its object is, but only an IRI can
        // match a URL we probed. A non-IRI object can therefore never widen
        // the scope, only keep it from dissolving.
        services.insert(&quad.subject);
        if let Term::NamedNode(object) = &quad.object {
            if endpoints.iter().any(|probed| same_endpoint(object.as_str(), probed)) {
                matched.insert(quad.subject.clone());
            }
        }
    }

    if services.is_empty() {
        return None;
    }
    if matched.is_empty() {
        return if services.len() == 1 { None } else { Some(matched) };
    }

    // A service's subtree is whatever it points at through `LINKING`, and
    // whatever that points at in turn, which is what makes a VoID partition
    // hung off a default graph (or off a blank node under one) this service's
    // own. Each round adds at least one subject or the loop stops, so the
    // quad count bounds it and a cyclic document cannot spin.
    for _ in 0..quads.len() {
        let mut grew = false;
        for quad in quads {
            if !LINKING.contains(&quad.predicate.as_str()) || !matched.contains(&quad.subject) {
                continue;
            }
            let linked = match &quad.object {
                Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
                Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
                _ => continue,
            };
            grew |= matched.insert(linked);
        }
        if !grew {
            break;
        }
    }
    Some(matched)
}

/// Compare two endpoint URLs the way an operator means them, not byte for
/// byte. Ignores scheme, a trailing slash, a default port, host case, and a
/// leading `www.`. Every one of those disagreements is common between a
/// registry URL and a published `sd:endpoint`, and treating them as different
/// services strips a real description down to nothing.
fn same_endpoint(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let s = u.trim();
        let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
        let (host, path) = match s.find('/') {
            Some(i) => (&s[..i], s[i..].trim_end_matches('/')),
            None => (s, ""),
        };
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix(":443").or_else(|| host.strip_suffix(":80")).unwrap_or(&host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        format!("{host}{path}")
    }
    norm(a) == norm(b)
}
