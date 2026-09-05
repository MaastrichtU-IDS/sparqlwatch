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
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

const SD_FEATURE: &str = "http://www.w3.org/ns/sparql-service-description#feature";
const SD_EXTENSION_FUNCTION: &str = "http://www.w3.org/ns/sparql-service-description#extensionFunction";
const SD_SUPPORTED_LANGUAGE: &str = "http://www.w3.org/ns/sparql-service-description#supportedLanguage";
const SD_DEFAULT_DATASET: &str = "http://www.w3.org/ns/sparql-service-description#defaultDataset";
const SD_GRAPH: &str = "http://www.w3.org/ns/sparql-service-description#graph";
const SD_DEFAULT_ENTAILMENT_REGIME: &str =
    "http://www.w3.org/ns/sparql-service-description#defaultEntailmentRegime";
const VOID_CLASS: &str = "http://rdfs.org/ns/void#class";
const VOID_PROPERTY: &str = "http://rdfs.org/ns/void#property";
const VOID_TRIPLES: &str = "http://rdfs.org/ns/void#triples";
const VOID_CLASSES: &str = "http://rdfs.org/ns/void#classes";
const VOID_ENTITIES: &str = "http://rdfs.org/ns/void#entities";
const SD_NAMED_GRAPH: &str = "http://www.w3.org/ns/sparql-service-description#namedGraph";
const VOID_CLASS_PARTITION: &str = "http://rdfs.org/ns/void#classPartition";
const VOID_EXAMPLE_RESOURCE: &str = "http://rdfs.org/ns/void#exampleResource";
const VOID_PROPERTY_PARTITION: &str = "http://rdfs.org/ns/void#propertyPartition";
const SD_ENDPOINT: &str = "http://www.w3.org/ns/sparql-service-description#endpoint";
const SD_SERVICE: &str = "http://www.w3.org/ns/sparql-service-description#Service";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Linking predicates: a service's subtree includes what it points at.
/// `defaultGraph` and `namedGraph` are here because real descriptions hang
/// VoID partitions off them, not only off `defaultDataset`.
///
/// The two partition predicates were added on 2026-09-05, when the vocabulary
/// INSIDE a partition started being collected. Until then only the boolean
/// `has_void_partitions` was read, unscoped in pass one, so the expansion never
/// had to reach a partition node; now `void:class` and `void:property` are read
/// scoped in pass two, and without these a partition hung off the dataset sits
/// outside the service's subtree and its vocabulary is silently dropped.
const LINKING: [&str; 8] = [
    "http://rdfs.org/ns/void#classPartition",
    "http://rdfs.org/ns/void#propertyPartition",
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
    ///
    /// THE DESCRIPTION DOCUMENT'S OWN SIZE, not the dataset's. A description is
    /// a handful of triples; the dataset it describes may hold millions. See
    /// `declared_triples` for the other one, and never conflate them.
    pub triples: usize,
    /// What the dataset says it holds, from `void:triples`.
    ///
    /// `None` means no claim was made. NOT `Some(0)`: zero is itself a claim,
    /// that the endpoint is empty, and a description that says nothing has not
    /// made it. The same distinction the six-verdict vocabulary draws between
    /// `absent` and `indeterminate`, one layer down.
    ///
    /// A value that will not parse as a non-negative integer is also `None`.
    /// Real descriptions carry typos, and `declared-but-wrong` is the harshest
    /// verdict in the vocabulary to hand out over a parse failure of our own.
    pub declared_triples: Option<u64>,
    /// What the dataset says it holds, from `void:classes`. `None` as above.
    pub declared_classes: Option<u64>,
    /// What the dataset says it holds, from `void:entities`. `None` as above.
    pub declared_entities: Option<u64>,
    /// How many `sd:namedGraph` statements the probed service makes.
    ///
    /// COUNTED, not read: there is no VoID or service-description predicate
    /// that states a number of graphs, so the claim is the length of the list
    /// the description gives. `None` when the description names none, which is
    /// no claim rather than a claim of zero graphs.
    pub declared_named_graphs: Option<u64>,
    /// `sd:defaultDataset` or `sd:graph` appeared at least once.
    pub names_dataset: bool,
    /// `void:classPartition` or `void:propertyPartition` appeared at least once.
    ///
    /// A GRADE input, read UNSCOPED in pass one, and deliberately unchanged by
    /// the two sets below: it answers "how informative is the document this
    /// operator published", so scoping it would regrade every description
    /// covering more than one service.
    pub has_void_partitions: bool,
    /// The classes a `void:classPartition` names, through `void:class`.
    ///
    /// The vocabulary the endpoint SAYS it holds, which is only half a fact.
    /// Set against the classes a profile pass actually found, it becomes the
    /// declared/observed axis the whole project is about: a class in here and
    /// not in a profile is declared and unconfirmed, one in a profile and not
    /// in here is confirmed and undeclared.
    ///
    /// NOT to be confused with `declared_classes`, which is the COUNT from
    /// `void:classes`. A count and a set are different facts and a shared name
    /// would let one be read as the other; the compiler caught exactly that
    /// when these were first written.
    pub partitioned_classes: BTreeSet<String>,
    /// The properties a `void:propertyPartition` names, through
    /// `void:property`. Same reading as `partitioned_classes`.
    pub partitioned_properties: BTreeSet<String>,
    /// `sd:defaultEntailmentRegime` appeared at least once.
    pub has_entailment: bool,
    /// `void:exampleResource` appeared at least once. One of the three things
    /// the spec's level 4 recognises, alongside an entailment regime and
    /// extension functions: naming a resource a client can actually
    /// dereference is a description doing more than describing itself.
    pub has_example_resources: bool,
    /// Whether the DOCUMENT declares any `sd:extensionFunction`, regardless of
    /// which service declares it. Deliberately separate from
    /// `extension_functions`, which is scoped to the service we probed: that
    /// set answers "does this service claim the capability", this flag answers
    /// "how informative is the document the operator published". Feeding the
    /// scoped set into the grade published Level(1), meaning "a stub", for one
    /// service of a two-service document and Level(4) for the other, from the
    /// same bytes.
    pub doc_declares_extension_functions: bool,
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
    /// The number this description stated under `iri`, if it stated one.
    ///
    /// A lookup rather than four public fields read at the call site, so that
    /// `metrics.toml` names the predicate it means and nothing in `resolve`
    /// has to know which VoID term maps to which field. An unrecognised
    /// predicate returns `None`: a metric pointed at a term this build cannot
    /// read has no declaration to compare against, which is the same answer as
    /// a description that stated nothing, and both are honest.
    pub fn count_of(&self, iri: &str) -> Option<u64> {
        match iri {
            "http://rdfs.org/ns/void#triples" => self.declared_triples,
            "http://rdfs.org/ns/void#classes" => self.declared_classes,
            "http://rdfs.org/ns/void#entities" => self.declared_entities,
            // No VoID or service-description predicate states a number of
            // graphs, so the claim is the length of the sd:namedGraph list.
            // Keyed on that predicate because it is the one a metric can name.
            "http://www.w3.org/ns/sparql-service-description#namedGraph" => {
                self.declared_named_graphs
            }
            _ => None,
        }
    }

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
    // `media::rdf_format_of`, not `RdfFormat::from_media_type` directly: the
    // header routinely carries a `charset` parameter, and handing that to
    // `from_media_type` returns `None`, silently reparsing an RDF/XML or
    // JSON-LD description as Turtle and losing every declaration in it. That
    // is exactly the drift that put the rule in one shared place, where
    // `client.rs` reads it too.
    let format = content_type
        .and_then(crate::media::rdf_format_of)
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
            SD_EXTENSION_FUNCTION => d.doc_declares_extension_functions = true,
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
        // The counts, before the capability sets: their objects are literals,
        // so the IRI-only guard below would drop every one of them.
        //
        // SCOPED, like the sets and for the same reason. A two-service document
        // whose other service declares a size must not have that size read as
        // this one's, which would be a false assertive claim about how big an
        // endpoint is.
        //
        // Last statement wins if a document repeats one. Repeats are not a
        // shape worth policing here: `resolve()` compares one number against
        // one observation, and a description contradicting itself is a problem for
        // its publisher rather than a case this parser can settle.
        match quad.predicate.as_str() {
            VOID_TRIPLES => d.declared_triples = count_of(&quad.object).or(d.declared_triples),
            VOID_CLASSES => d.declared_classes = count_of(&quad.object).or(d.declared_classes),
            VOID_ENTITIES => d.declared_entities = count_of(&quad.object).or(d.declared_entities),
            // Counted rather than read: one statement, one graph.
            SD_NAMED_GRAPH => {
                d.declared_named_graphs = Some(d.declared_named_graphs.unwrap_or(0) + 1)
            }
            _ => {}
        }
        // Predicate first, so a document full of unrelated triples costs no
        // allocation here.
        let set = match quad.predicate.as_str() {
            SD_FEATURE => &mut d.features,
            SD_EXTENSION_FUNCTION => &mut d.extension_functions,
            SD_SUPPORTED_LANGUAGE => &mut d.languages,
            VOID_CLASS => &mut d.partitioned_classes,
            VOID_PROPERTY => &mut d.partitioned_properties,
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

/// A declared count, or `None` when the object is not one.
///
/// Accepts any literal whose lexical form parses as a non-negative integer,
/// whatever its datatype: real descriptions type these as `xsd:integer`,
/// `xsd:nonNegativeInteger`, `xsd:long`, and quite often as a plain string.
/// Refusing on datatype would discard true claims over a detail no reader
/// cares about, while a lexical form that is not a number is refused outright.
///
/// A negative count is `None` rather than clamped: it is not a number of
/// things, so treating it as zero would invent a claim the publisher did not
/// make.
fn count_of(object: &Term) -> Option<u64> {
    match object {
        Term::Literal(l) => l.value().trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Which subjects the capability sets may be read from.
///
/// `None` means no scope at all: read the whole document. `Some(set)` means
/// read only those subjects, and an empty set therefore yields no
/// capabilities.
///
/// A **service**, for both jobs a service set does here, is any subject the
/// document presents as one: a subject typed `sd:Service`, or a subject
/// carrying an `sd:endpoint`. One definition, stated once, because the two
/// jobs must agree. Counting only `sd:endpoint` subjects made a service block
/// that states no endpoint invisible, so a genuinely two-service document took
/// the single-service fallback below, was read whole, and credited the probed
/// endpoint with its neighbour's extension function.
///
/// The four cases:
///
/// - The document presents no service at all: **no scope**. Most real
///   descriptions state no endpoint, and many state no type either; scoping
///   those to nothing would turn every one of them into a false `undeclared`.
///   (The 21 byte-identical Virtuoso stubs in the survey state both, and match,
///   so they scope to their one service.)
/// - Some `sd:endpoint` matches one of `endpoints` under `same_endpoint`:
///   **scope to those subjects**, expanded transitively through `LINKING`.
/// - Endpoints or service types are stated, none matches, and the document
///   presents exactly one service: **no scope**. We fetched this document from
///   the endpoint we are probing and it describes one service; the URL
///   disagreement is theirs.
/// - Endpoints or service types are stated, none matches, and the document
///   presents more than one service: **empty scope**. Crediting one of several
///   services at random is exactly the leak this function exists to close. The
///   grade is unaffected, because the grade is not scoped.
fn scope_of(quads: &[Quad], endpoints: &[&str]) -> Option<HashSet<NamedOrBlankNode>> {
    let mut services: HashSet<&NamedOrBlankNode> = HashSet::new();
    let mut matched: HashSet<NamedOrBlankNode> = HashSet::new();
    for quad in quads {
        // A subject the document types as a service is a service whether or
        // not it also states where to reach it.
        if quad.predicate.as_str() == RDF_TYPE
            && matches!(&quad.object, Term::NamedNode(o) if o.as_str() == SD_SERVICE)
        {
            services.insert(&quad.subject);
            continue;
        }
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
    // whatever that points at in turn: a CAPABILITY hung off a blank node,
    // as in `sd:defaultDataset [ sd:defaultGraph [ sd:extensionFunction ...
    // ] ]`, is this service's own because of it. A VoID partition hung the
    // same way is not an example of this mattering: `has_void_partitions` is
    // a grade input, read unscoped in pass one, so it never reaches this
    // expansion at all, scoped or not.
    //
    // Indexed once, subject to what it points at, then walked as a worklist
    // from the matched services, so each subject is expanded at most once and
    // the whole expansion is linear in the quad count. The fixed-point loop
    // this replaces rescanned every quad every round, which a document turns
    // quadratic by writing its linking chain in reverse document order: a
    // 259,980-byte body (inside the 256 KiB cap in `client.rs`) with a
    // 10,440-link reversed `sd:graph` chain took 20.9 s in release and 171 s in
    // debug, against 27 ms for a flat body of the same size. That is
    // synchronous CPU work at no await point, so no `tokio::time::timeout` in
    // `budget.rs` can drop it and one hostile host would stall the whole
    // sequential sweep, which is the umakadata failure this system exists to
    // avoid. The `matched` set is also the visited set, so a cyclic document
    // still terminates.
    let mut links: HashMap<&NamedOrBlankNode, Vec<NamedOrBlankNode>> = HashMap::new();
    for quad in quads {
        if !LINKING.contains(&quad.predicate.as_str()) {
            continue;
        }
        let linked = match &quad.object {
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
            // The arm that makes a capability hung off a blank node (e.g.
            // `sd:defaultDataset [ sd:defaultGraph [ sd:extensionFunction
            // ... ] ]`) part of the service that points at it.
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
            _ => continue,
        };
        links.entry(&quad.subject).or_default().push(linked);
    }

    let mut frontier: VecDeque<NamedOrBlankNode> = matched.iter().cloned().collect();
    while let Some(subject) = frontier.pop_front() {
        let Some(linked) = links.get(&subject) else {
            continue;
        };
        for next in linked {
            // The boundary. A node that is itself another of the document's
            // services stays that service's, even though ours points at it:
            // its declarations are its own claims, not ours. A node that is
            // merely a dataset does not stop the walk, so two services
            // pointing at the SAME dataset node genuinely share it.
            if services.contains(&next) && !matched.contains(next) {
                continue;
            }
            if matched.insert(next.clone()) {
                frontier.push_back(next.clone());
            }
        }
    }
    Some(matched)
}

/// Compare two endpoint URLs the way an operator means them, not byte for
/// byte. Ignores scheme, a trailing slash, a default port, host case, a
/// leading `www.`, a `#fragment`, and userinfo (the `user@` in an authority).
/// Every one of those disagreements is common between a registry URL and a
/// published `sd:endpoint`, and treating them as different services strips a
/// real description down to nothing.
///
/// The query string is deliberately kept. `?db=a` and `?db=b` on one path can
/// be two genuinely different services, and unioning them would be a false
/// capability credit; a lost declaration only softens a verdict (`Verified` to
/// `UndeclaredButVerified`, `DeclaredOnly` to `Indeterminate`) and can never
/// mint an `Absent`, so it is the safe direction.
///
/// Two consequences of the leniency, accepted rather than overlooked:
///
/// - Dropping the scheme and a leading `www.` means one document declaring two
///   services at `http://x/sparql` and `https://www.x/sparql` has them unioned,
///   because the scope is the union of every subject that matches. Byte
///   equality would instead lose a real declaration on every http-to-https
///   redirect, which the survey shows is routine (472 of 548 registry URLs are
///   plain `http://`), while two same-host services differing only by scheme or
///   `www.` is pathological.
/// - Scope selection compares subjects only and ignores the graph name, so a
///   TriG or N-Quads description stating two services in two named graphs is
///   read as one flat graph. Real descriptions are served as Turtle or RDF/XML,
///   so this is a known narrowing rather than a live leak.
fn same_endpoint(a: &str, b: &str) -> bool {
    fn norm(u: &str) -> String {
        let s = u.trim();
        let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
        // A fragment is never sent to the server, so it cannot distinguish two
        // endpoints. Stripped before the path split, since a fragment normally
        // sits at the end of the path.
        let s = s.split('#').next().unwrap_or(s);
        let (authority, path) = match s.find('/') {
            Some(i) => (&s[..i], s[i..].trim_end_matches('/')),
            None => (s, ""),
        };
        // Userinfo is credentials, not identity: everything up to and including
        // the last `@`. A raw `@` cannot appear in a host, so the last one
        // delimits.
        let authority = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };
        let host = authority.to_ascii_lowercase();
        let host = host.strip_suffix(":443").or_else(|| host.strip_suffix(":80")).unwrap_or(&host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        format!("{host}{path}")
    }
    let (a, b) = (norm(a), norm(b));
    // Two URLs that normalise to nothing are not "the same endpoint". Today
    // endpoint URLs are validated before a sweep, so this cannot fire, but the
    // failure mode of a matcher that returns true on empty input is a false
    // capability credit, which is the one outcome this module must never cause.
    !a.is_empty() && a == b
}
