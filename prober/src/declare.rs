//! Reduce a fetched RDF graph (a SPARQL service description, in practice) to
//! the small set of facts it declares about itself.
//!
//! This module decides nothing. It has no notion of "good" or "bad": it only
//! reports what predicates and objects appeared, so that a later stage can
//! compare a claim against an observation. The finding motivating this
//! module is that the claim and the observation routinely disagree -- see
//! `prober/tests/declare.rs` for the concrete numbers from the survey.

use oxrdf::Term;
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::BTreeSet;

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
/// or no.
///
/// A syntax error partway through the body is not a failure of this
/// function: whatever was parsed before the error is returned as-is, since a
/// partial graph is still evidence and the caller has no better option than
/// "what we could read". This function never panics and never returns an
/// `Err` -- there is nothing for a caller to do with either that "collect
/// what you can" does not already cover.
pub fn parse_declarations(body: &str, content_type: Option<&str>) -> Declarations {
    let format = content_type
        .and_then(RdfFormat::from_media_type)
        .unwrap_or(RdfFormat::Turtle);

    let mut d = Declarations::empty();
    let quads = RdfParser::from_format(format).for_reader(body.as_bytes());
    for result in quads {
        let quad = match result {
            Ok(q) => q,
            // A syntax error ends the parse here; everything collected up
            // to this point is kept rather than discarded.
            Err(_) => break,
        };
        d.triples += 1;
        let object_iri = match &quad.object {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        };
        match quad.predicate.as_str() {
            SD_FEATURE => {
                if let Some(iri) = object_iri {
                    d.features.insert(iri);
                }
            }
            SD_EXTENSION_FUNCTION => {
                if let Some(iri) = object_iri {
                    d.extension_functions.insert(iri);
                }
            }
            SD_SUPPORTED_LANGUAGE => {
                if let Some(iri) = object_iri {
                    d.languages.insert(iri);
                }
            }
            SD_DEFAULT_DATASET | SD_GRAPH => d.names_dataset = true,
            SD_DEFAULT_ENTAILMENT_REGIME => d.has_entailment = true,
            VOID_EXAMPLE_RESOURCE => d.has_example_resources = true,
            VOID_CLASS_PARTITION | VOID_PROPERTY_PARTITION => d.has_void_partitions = true,
            _ => {}
        }
    }
    d
}
