//! How one run's graph is written, and what a reader of an unfinished run may
//! rely on. Stated here because four functions in this module have to agree on
//! it and `web/load_run.py` has to agree with all four.
//!
//! A run is written as a header of run-level facts, the endpoints the sweep
//! declined to ask, one self-contained chunk per endpoint, and a footer. A crash
//! leaves a prefix of that sequence, ending at a line boundary in a document
//! that still parses, so the danger is never an unreadable file: it is a
//! readable file whose lines contradict each other. Two rules keep that from
//! happening.
//!
//! 1. **Every section ends with its terminator.** `sw:emission` closes the
//!    header, `sw:dormantCount` closes the dormancy section,
//!    `sw:completedEndpoint` closes a chunk, `sw:finalised` closes the footer. A
//!    reader holding a terminator holds the whole section; a reader that does
//!    not holds a fragment and may drop it. Those four spellings are a wire
//!    format shared with the loader, so neither side is free to change them
//!    alone.
//! 2. **No fact family publishes its own summary before the things it
//!    summarises.** A cut inside a list has to lose the list, never leave a
//!    count standing beside three of the two hundred values it counted, which a
//!    consumer would render as a complete answer. This is why `sw:sampleSize`
//!    and `sw:sampleTruncated` come after the last `sw:sampledValue`, and why
//!    `sw:failedEndpoints` sits in the footer: it summarises the chunks.

use crate::dormancy::SkipReason;
use crate::metrics::Cost;
use crate::verdict::{Level, Verdict};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfSerializer};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

/// The predicate that closes the header, `emit_header`'s last quad.
///
/// This and the three below are the wire format `web/load_run.py` cuts a
/// truncated run back to. Their canonical spellings are in
/// `docs/design/section-terminators.md`, which the test at the bottom of this
/// module asserts these four constants against, and which
/// `web/tests/test_load_run.py` asserts the loader's set against. Renaming one
/// here alone reds this crate's suite instead of silently discarding every
/// endpoint a crash preserved.
pub const HEADER_TERMINATOR: &str = "urn:sparqlwatch:emission";
/// The predicate that closes the dormancy section, `emit_dormancy`'s last quad.
///
/// The COUNT, `sw:dormantCount`, and deliberately not the per-endpoint
/// `sw:dormantEndpoint` one character away from it. Exactly one of the two is a
/// section terminator, and the convention already points both ways, since
/// `sw:completedEndpoint` is singular, per-endpoint AND a terminator. Reading
/// the per-endpoint spelling as a boundary would make every dormancy line a cut
/// point, so a truncation would land in the middle of the section instead of
/// after it.
pub const DORMANCY_TERMINATOR: &str = "urn:sparqlwatch:dormantCount";
/// The predicate that closes one endpoint's chunk, `emit_endpoint`'s last quad.
pub const CHUNK_TERMINATOR: &str = "urn:sparqlwatch:completedEndpoint";
/// The predicate that closes the footer, `emit_footer`'s last quad.
pub const FOOTER_TERMINATOR: &str = "urn:sparqlwatch:finalised";

const DQV: &str = "http://www.w3.org/ns/dqv#";
const PROV: &str = "http://www.w3.org/ns/prov#";
const DCAT: &str = "http://www.w3.org/ns/dcat#";

pub struct RunId(pub String);

#[derive(Clone)]
pub struct MeasurementRow {
    pub endpoint: String,
    pub metric_id: String,
    pub verdict: Verdict,
    pub level: Option<Level>,
    /// How long the probe took, when we actually measured it. `None` when the
    /// measurement came from an expired budget: a timed-out probe that
    /// published `elapsedMs 0` would read as the fastest observation in the
    /// dataset. Same principle as `Absent` -- do not state what you did not
    /// measure -- and `Option` makes the absence representable instead of
    /// encoding it as a zero.
    pub elapsed_ms: Option<u64>,
}

/// Whether one endpoint's description fetch produced a parseable graph of at
/// least one triple, computed in `run_sweep` as `declarations.triples > 0`.
/// Published once per endpoint per run, independent of what `resolve_fetch`
/// graded the same fetch as: a description that declares something and then
/// hits a syntax error mid-parse keeps `read: true` here while its
/// `service-description` row is `Indeterminate`. This is deliberately not a
/// `MeasurementRow`: it is a fact about the fetch, not a measurement against
/// a metric definition.
#[derive(Clone)]
pub struct DeclarationsRead {
    pub endpoint: String,
    pub read: bool,
}

/// Why a metric was never measured. An enum, not a string, so a second reason
/// added later cannot be spelled two ways by two call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotMeasuredReason {
    /// The metric's cost exceeded the ceiling the sweep was run with.
    CostCeiling,
    /// The prober itself failed on this endpoint: the task probing its host
    /// panicked or was cancelled, so no request was ever answered and no
    /// observation exists to grade.
    ///
    /// Deliberately not an `Indeterminate` measurement row. An `Indeterminate`
    /// verdict asserts that a measurement happened and was inconclusive, which
    /// is what an expired budget produces, and a reader could not tell the two
    /// apart. This says the weaker, true thing: nothing was measured, and the
    /// reason is on our side rather than the endpoint's.
    ProberFailed,
}

impl NotMeasuredReason {
    /// The published slug. Stable: it goes into the graph.
    pub fn slug(&self) -> &'static str {
        match self {
            NotMeasuredReason::CostCeiling => "cost-ceiling",
            NotMeasuredReason::ProberFailed => "prober-failed",
        }
    }
}

/// A metric that was deliberately not run against an endpoint, and why.
///
/// Deliberately NOT a seventh `Verdict`. A verdict says what we found out
/// about a capability; this says that no measurement happened at all. Folding
/// it into the verdict vocabulary would force every consumer that filters on
/// verdicts to know about a value that is not one. It is published as its own
/// type, carrying no `dqv:value` and no `sw:level`, so a consumer asking "what
/// is the verdict" gets nothing (which is correct) while a consumer asking
/// "why is there no verdict" gets an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotMeasured {
    pub endpoint: String,
    pub metric_id: String,
    pub reason: NotMeasuredReason,
}

/// What one enumerating probe actually saw. Published so a reader can ask
/// "which classes does this endpoint hold" instead of only "does it hold any",
/// which is the question the whole project exists to answer before somebody
/// writes a query.
///
/// Deliberately NOT a `void:classPartition` / `void:class` structure, and not
/// any other borrowed vocabulary. Both of those carry `rdfs:domain
/// void:Dataset`, so either one here would entail under plain RDFS that this
/// sample IS a dataset description. It is not: it is an observation from a
/// bounded query against a service, and the thing behind that service may be
/// several datasets or a virtual graph over a relational store (`ontop`, in
/// our own `endpoints.toml`). The same mistake was already shipped once on
/// this project, when the `NotMeasured` fact reused `dqv:computedOn` and
/// `dqv:isMeasurementOf` and thereby entailed that 548 deliberately declined
/// pairs were measurements. Nothing broke until a consumer ran inference, and
/// no test could have caught it, so the rule is now the type's own
/// documentation: a sample carries our own predicates, `rdf:type`, and
/// `prov:wasGeneratedBy`, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSample {
    pub endpoint: String,
    pub metric_id: String,
    /// The IRIs the probe actually bound, in the order the endpoint returned
    /// them. Not sorted: the order is evidence about the endpoint, and sorting
    /// would discard it for a tidiness nobody asked for. Not deduplicated
    /// either -- `SELECT DISTINCT` already did that, and doing it again here
    /// would hide an endpoint that ignores `DISTINCT`.
    ///
    /// A value that cannot be written as an IRI is dropped at emission and is
    /// not counted by the published `sampleSize`, so the size a consumer reads
    /// always matches the `sampledValue` quads beside it. The drop itself is
    /// logged, because the graph has no way to say "there was one more and we
    /// could not name it".
    pub values: Vec<String>,
    /// True when `values.len()` reached the metric's declared `sample_limit`,
    /// so a further value may exist and we did not see it. A list a reader
    /// believes is complete when it is not is the content equivalent of a
    /// confident wrong answer, so this is published rather than inferred.
    pub truncated: bool,
}

fn nn(s: &str) -> anyhow::Result<NamedNode> {
    Ok(NamedNode::new(s)?)
}

/// Which kind of fact a subject names. The three kinds share a key, so they
/// must not share an IRI: a measurement and a not-measured fact about one pair
/// would otherwise land on a node that both has and has not a verdict, which
/// is what `the_three_kinds_never_share_a_subject` pins.
enum FactKind {
    Measurement,
    NotMeasured,
    ContentSample,
}

impl FactKind {
    /// The published segment for this kind. Stable: it goes into the graph.
    fn prefix(&self) -> &'static str {
        match self {
            FactKind::Measurement => "measurement",
            FactKind::NotMeasured => "not-measured",
            FactKind::ContentSample => "content-sample",
        }
    }
}

/// Percent-encode keeping RFC 3986's unreserved set (`ALPHA / DIGIT / "-" /
/// "." / "_" / "~"`), over UTF-8 bytes, with uppercase hex.
///
/// The output holds no `:`, which is half of what makes `subject_iri`
/// injective; a validated metric id holding none is the other half. No
/// normalisation of any kind happens here: `registry::dedupe` treats two
/// endpoint strings differing only in case as two registry entries, so they
/// are two subjects here too, and the endpoint IRI a subject encodes is the
/// one the registry actually contains.
fn encode_unreserved(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The subject of one published run-scoped fact, derived from what the fact is
/// about rather than from where its row sat.
///
/// Derived so that an endpoint's facts can be written on their own (stage
/// 1c-b4) and so two runs can be diffed. No consumer needs to parse it: a
/// measurement carries `dqv:computedOn` and `dqv:isMeasurementOf`, a sample
/// carries `sw:sampledFrom` and `sw:sampledBy`, and a not-measured fact
/// carries `sw:notMeasuredOn` and `sw:notMeasuredMetric`.
///
/// Rejects a metric id outside `[a-z0-9][a-z0-9-]*`. `load_metrics` refuses one
/// too, and the duplication is deliberate: this function's injectivity depends
/// on the invariant, so it checks it rather than trusting a check two modules
/// away. A validated `MetricId` newtype would make the check unnecessary and is
/// the better long-term shape; it is deferred because it touches every module
/// that names a metric.
///
/// The cost of embedding the endpoint reversibly: a URL carrying a credential
/// would be published forever, in a graph this project never rewrites.
/// `registry::load_endpoints` drops a URL with userinfo for that reason; an API
/// key in a query string is a known, unmitigated exposure, recorded under
/// Known limitations in `prober/README.md`.
fn subject_iri(
    kind: FactKind,
    run: &RunId,
    endpoint: &str,
    metric_id: &str,
) -> anyhow::Result<NamedNode> {
    let mut chars = metric_id.chars();
    let well_formed = match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        _ => false,
    };
    if !well_formed {
        anyhow::bail!(
            "metric id '{metric_id}' cannot be part of a subject IRI: allowed is [a-z0-9][a-z0-9-]*, \
             because the subject is split on ':' and any other character would make the endpoint \
             field and the metric field ambiguous"
        );
    }
    nn(&format!(
        "urn:sparqlwatch:{}:{}:{}:{}",
        kind.prefix(),
        run.0,
        encode_unreserved(endpoint),
        metric_id
    ))
}

/// Whether `subject` carries more than one distinguishable fact in this
/// emission.
///
/// A subject that does is dropped whole rather than resolved: keeping the first
/// of a `verified` and an `absent` observation would have the graph assert
/// `verified`, with full confidence, about a pair we also measured as `absent`.
/// A run graph is never rewritten, so there is no later chance to correct it,
/// and an endpoint with no measurement for a metric is a shape the site's
/// queries already handle.
fn conflicted(payloads: &BTreeMap<String, BTreeSet<String>>, subject: &str) -> bool {
    payloads.get(subject).is_some_and(|p| p.len() > 1)
}

/// Everything `emit_nquads` needs to publish one run's graph, gathered into
/// named fields rather than positional parameters.
///
/// The field list grew once already (see `Sweep`, one commit prior, for the
/// same growth on `run_sweep`'s return type) and the spec already promises
/// more side-facts to publish, so a struct is the right shape regardless of
/// clippy's line: a ninth fact list only ever adds a field, never touches an
/// existing call site.
///
/// The reason that matters here specifically: `generated_at` and
/// `metric_revision` are adjacent and both plain `&str`. Under the old
/// positional signature, swapping them was a silent, type-checking mistake
/// that would publish the metric revision as `prov:generatedAtTime` and the
/// timestamp as `metricDefinitionRevision`, inside a per-run graph this
/// project treats as immutable once written. Named fields make that specific
/// swap a compile error instead of a review miss.
pub struct RunEmission<'a> {
    pub run: &'a RunId,
    /// The instant the run was started, published as `prov:generatedAtTime`.
    /// Not a revision hash: see the struct's doc comment for the mistake this
    /// field name exists to prevent.
    pub generated_at: &'a str,
    /// Identifies the metric definitions the run used. The spec requires a
    /// run to record both it and the prober version, because a measurement is
    /// only interpretable against the definition that produced it. Must be a
    /// pure function of the definitions (see `metrics::definitions_revision`),
    /// never a clock or a counter, so that re-running the same definitions
    /// yields the same revision. Not a timestamp: see the struct's doc
    /// comment for the mistake this field name exists to prevent.
    pub metric_revision: &'a str,
    pub rows: &'a [MeasurementRow],
    pub declarations_read: &'a [DeclarationsRead],
    pub not_measured: &'a [NotMeasured],
    /// The ceiling the sweep was run with, recorded on the run's activity. A
    /// parameter rather than something this function discovers:
    /// `emit_nquads` reads no clock, no environment and no global, so the
    /// same inputs always produce the same document.
    pub max_cost: Cost,
    /// What the enumerating probes saw, published verbatim and in the
    /// endpoint's own order.
    pub content_samples: &'a [ContentSample],
    /// How many hosts the sweep talked to at once, recorded on the activity
    /// beside `max_cost` and for the same reason: it is a parameter of the run,
    /// not of any one measurement.
    ///
    /// It does NOT change what `elapsedMs` measures. `client::gated_chain`
    /// publishes the sum of the hops' own durations and each hop's timer starts
    /// after the gate is acquired, precisely so our politeness is never
    /// published as somebody's response time. What it does explain is the run's
    /// wall-clock duration, and whether the cross-host redirect contention in
    /// `run_sweep`'s doc could have cost a metric here at all: at a concurrency
    /// of one no other group was running, so it could not.
    ///
    /// `NonZeroUsize` because that is what `run_sweep` takes, so the published
    /// number cannot say a sweep ran zero hosts at once.
    pub concurrency: NonZeroUsize,
    /// How many endpoints the run failed on, from `Sweep::failed_endpoints`.
    /// It is NOT what tells a reader whether a run finished. `sw:finalised` is,
    /// and a run that died never reached the footer that carries either one, so
    /// this count is only meaningful beside it. What it adds is the total, where
    /// the per-endpoint facts say `prober-failed` one at a time.
    pub failed_endpoints: usize,
}

/// The two IRIs every fact in one run hangs off: the run's named graph and the
/// activity that produced it. Derived in one place, so the header, every chunk
/// and the footer cannot disagree about which graph they are writing into.
fn graph_and_activity(run: &RunId) -> anyhow::Result<(GraphName, NamedNode)> {
    let graph = GraphName::NamedNode(nn(&format!("urn:sparqlwatch:run:{}", run.0))?);
    let activity = nn(&format!("urn:sparqlwatch:activity:{}", run.0))?;
    Ok((graph, activity))
}

/// Serialize one section's quads.
///
/// N-Quads has no prologue and no trailer and every line ends in a newline, so
/// a document is exactly the concatenation of the sections that make it up and
/// any prefix of it parses. That is what lets a run be written in sections at
/// all.
fn serialize(quads: &[Quad]) -> anyhow::Result<String> {
    let mut out = Vec::new();
    let mut ser = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for q in quads {
        ser.serialize_quad(q.as_ref())?;
    }
    ser.finish()?;
    Ok(String::from_utf8(out)?)
}

/// The run-level facts, written once before any chunk. Everything here is a
/// parameter of the run, true at t=0, so none of it is a claim about what the
/// run will find.
pub struct RunHeader<'a> {
    pub run: &'a RunId,
    /// The instant the run was started, published as `prov:generatedAtTime`.
    /// Not a revision hash: see `RunEmission`'s doc comment for the mistake
    /// this field name exists to prevent.
    pub generated_at: &'a str,
    /// Identifies the metric definitions the run used. Not a timestamp, for the
    /// same reason.
    pub metric_revision: &'a str,
    /// The ceiling the sweep was run with. A parameter rather than something
    /// this module discovers: `emit` reads no clock, no environment and no
    /// global, so the same inputs always produce the same document.
    pub max_cost: Cost,
    /// How many hosts the sweep talked to at once; see `RunEmission` for what a
    /// consumer may and may not conclude from it.
    pub concurrency: NonZeroUsize,
    /// The endpoints this sweep declined to ask, and why.
    ///
    /// Carried on the HEADER rather than handed to a writer method of its own,
    /// and that is the whole design of this section. A `RunWriter::dormancy`
    /// call would be a rule to enforce: `write_endpoint` and `finish` would have
    /// to refuse a file whose section was missing, and "missing" would be a
    /// representable state of a half-written run. Here it is a product of
    /// construction. Every existing construction site passes a `RunHeader`, so
    /// each one gains this field with an empty slice, and a run file with no
    /// dormancy section cannot be built.
    ///
    /// The quads themselves are NOT in the header section. They have endpoint
    /// subjects, and `load_run._holds_endpoint_facts` decides whether to take a
    /// file with no terminator anywhere, which is a fragment cut inside the
    /// header, by asking whether every subject is the activity. So
    /// `emit_dormancy` writes them AFTER `sw:emission`, and `write::RunWriter`
    /// is what puts the two sections out back to back.
    pub dormant: &'a [DormancyFact],
}

/// One endpoint a sweep declined to ask, as the run graph publishes it.
///
/// Built in `main.rs` from `dormancy::Skipped`, and kept as its own type rather
/// than emitting `Skipped` directly because `Skipped` is the policy's answer and
/// this is the published fact: the policy is free to grow a field the graph does
/// not carry, and the graph is free to carry one the policy computes elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormancyFact {
    pub endpoint: String,
    /// When the endpoint was relegated, as the state holds it and not as this
    /// run computed it, so the graph publishes the instant it actually happened.
    /// `None` for an operator hold, which has no relegation instant, and for an
    /// entry a hand edit left without one; no quad is then written, because a
    /// zero or an empty literal would be a claim about a date nobody recorded.
    pub dormant_since: Option<String>,
    pub reason: SkipReason,
}

/// The predicate that names one declined endpoint on the run's activity.
///
/// Required, and it mirrors `sw:completedEndpoint`. Without it the endpoint's
/// dormancy triples hang off nothing an activity reaches: no CONSTRUCT could
/// date them without inventing a triple, which is the defect
/// `web/queries/endpoint_description.rq` records for the class sample ("a
/// document that named one activity and left the sample hanging off nothing
/// would invite a consumer to date the sample to the sweep that declined to take
/// it"), and it is the join any reader has to make to reach these facts at all.
/// No `.rq` file in `web/queries/` joins it yet; a later stage's page is what
/// will, and this is the quad that makes it possible.
const DORMANT_ENDPOINT: &str = "urn:sparqlwatch:dormantEndpoint";
/// Why, as `SkipReason::slug` spells it. A slug and not a sentence, because a
/// page renders it and the loader parses it.
const DORMANCY_REASON: &str = "urn:sparqlwatch:dormancyReason";
/// When, typed as `xsd:dateTime`. Omitted when the fact carries no instant.
const DORMANT_SINCE: &str = "urn:sparqlwatch:dormantSince";

/// One declined endpoint's quads: the naming quad on the activity, the type, the
/// reason, and the instant when there is one.
///
/// The endpoint is typed HERE, because `emit_endpoint` types every endpoint in
/// its own chunk so a truncated reader never holds an untyped endpoint, and an
/// endpoint the sweep declined has no chunk to be typed in.
///
/// An endpoint that is not a well-formed IRI yields no quads and is logged,
/// which is what every other fact family in this module does with one: a run
/// graph is never rewritten, so a fact that cannot be published is dropped
/// rather than published wrongly. `emit_dormancy` counts what it published, so
/// the count a consumer reads always matches the quads beside it.
pub fn dormancy_quads(fact: &DormancyFact, run: &RunId) -> anyhow::Result<Vec<Quad>> {
    let (graph, activity) = graph_and_activity(run)?;
    let endpoint = match NamedNode::new(&fact.endpoint) {
        Ok(node) => node,
        Err(error) => {
            tracing::warn!(
                endpoint = %fact.endpoint,
                error = %error,
                "dropping a dormancy fact: the endpoint is not a valid IRI, so nothing in the \
                 graph can name it"
            );
            return Ok(Vec::new());
        }
    };
    let mut quads = vec![
        Quad::new(
            NamedOrBlankNode::NamedNode(activity),
            nn(DORMANT_ENDPOINT)?,
            Term::NamedNode(endpoint.clone()),
            graph.clone(),
        ),
        Quad::new(
            NamedOrBlankNode::NamedNode(endpoint.clone()),
            rdf::TYPE.into_owned(),
            Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
            graph.clone(),
        ),
        Quad::new(
            NamedOrBlankNode::NamedNode(endpoint.clone()),
            nn(DORMANCY_REASON)?,
            Term::Literal(Literal::new_simple_literal(fact.reason.slug())),
            graph.clone(),
        ),
    ];
    if let Some(since) = &fact.dormant_since {
        quads.push(Quad::new(
            NamedOrBlankNode::NamedNode(endpoint),
            nn(DORMANT_SINCE)?,
            Term::Literal(Literal::new_typed_literal(since.as_str(), xsd::DATE_TIME)),
            graph,
        ));
    }
    Ok(quads)
}

/// Write the dormancy section: one group per declined endpoint, then the count
/// that closes it.
///
/// **Why it exists at all.** A sweep that probes 486 of 543 endpoints and
/// publishes a run graph naming only those 486 leaves a consumer to read silence
/// about the other 57 as a claim that nothing was found there. Dormancy is not a
/// verdict and never becomes one, so the run says the weaker, true thing: this
/// sweep declined to ask, and here is why.
///
/// **Ordered by endpoint**, so two runs that declined the same set produce the
/// same bytes here and a diff by eye shows only what changed. The caller's order
/// is not evidence about anything, unlike a content sample's, so there is
/// nothing to preserve.
///
/// **The count is last and is published even at zero.** Last, because rule 2 of
/// the section protocol forbids a fact family summarising itself before the
/// things it summarises: a cut inside the section has to lose the section, never
/// leave a count standing beside two of the 48 endpoints it counted. At zero,
/// because `sw:failedEndpoints` is published at zero for the same reason, and
/// because a reader distinguishing "this sweep declined nothing" from "this run
/// predates dormancy" needs the fact present rather than absent. Its subject is
/// the run's activity, which `load_run._is_terminator_line` requires as a second
/// anchor beside the predicate.
///
/// **Nothing reads the count.** It is for a reader of the n-quads, and the
/// `RunHeader` field is what makes the section's absence impossible rather than
/// merely detectable.
pub fn emit_dormancy(run: &RunId, dormant: &[DormancyFact]) -> anyhow::Result<String> {
    let (graph, activity) = graph_and_activity(run)?;
    let mut ordered: Vec<&DormancyFact> = dormant.iter().collect();
    ordered.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    let mut quads: Vec<Quad> = Vec::new();
    // Counted from what was published rather than from `dormant.len()`, so an
    // endpoint whose IRI cannot be written does not leave the count one above
    // the groups beside it. Same rule as `sw:sampleSize`.
    let mut published = 0usize;
    for fact in ordered {
        let group = dormancy_quads(fact, run)?;
        if group.is_empty() {
            continue;
        }
        published += 1;
        quads.extend(group);
    }
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity),
        nn(DORMANCY_TERMINATOR)?,
        Term::Literal(Literal::new_typed_literal(published.to_string(), xsd::INTEGER)),
        graph,
    ));
    serialize(&quads)
}

/// Write the header, ending with the terminator that says a reader should
/// expect chunks after it.
///
/// `RunHeader::dormant` is deliberately NOT written here: its quads have
/// endpoint subjects and this section must have none (see the field's own
/// comment). `emit_dormancy` writes them, and `write::RunWriter::start` is what
/// calls both in order. `RunWriter` is the only production writer of a run file,
/// so there is exactly one place that pairing can go wrong.
pub fn emit_header(header: RunHeader) -> anyhow::Result<String> {
    let RunHeader { run, generated_at, metric_revision, max_cost, concurrency, dormant: _ } =
        header;
    let (graph, activity) = graph_and_activity(run)?;
    let mut quads: Vec<Quad> = Vec::new();

    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        rdf::TYPE.into_owned(),
        Term::NamedNode(nn(&format!("{PROV}Activity"))?),
        graph.clone(),
    ));
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn(&format!("{PROV}generatedAtTime"))?,
        Term::Literal(Literal::new_typed_literal(generated_at, xsd::DATE_TIME)),
        graph.clone(),
    ));
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn("urn:sparqlwatch:proberVersion")?,
        Term::Literal(Literal::new_simple_literal(env!("CARGO_PKG_VERSION"))),
        graph.clone(),
    ));
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn("urn:sparqlwatch:metricDefinitionRevision")?,
        Term::Literal(Literal::new_simple_literal(metric_revision)),
        graph.clone(),
    ));
    // Which metrics ran is a property of the run, not of any one measurement:
    // without it a run that declined `classes` is indistinguishable from one
    // that ran it, and the not-measured facts say which metrics were declined
    // but not what policy declined them.
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn("urn:sparqlwatch:maxCost")?,
        Term::Literal(Literal::new_simple_literal(max_cost.slug())),
        graph.clone(),
    ));
    // How many hosts were in flight. A parameter of the run rather than of any
    // measurement, and it does NOT change what `elapsedMs` means: `gated_chain`
    // sums the hops' own durations and each hop's timer starts after the gate
    // is acquired, so a gate wait is never published as a response time. It is
    // published because it explains the run's wall-clock duration and because
    // it is what tells a consumer whether the cross-host redirect contention
    // documented on `run_sweep` could have reached this run's metric budgets.
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn("urn:sparqlwatch:concurrency")?,
        Term::Literal(Literal::new_typed_literal(concurrency.to_string(), xsd::INTEGER)),
        graph.clone(),
    ));
    // The header's terminator, and a fact about how this document is built
    // rather than about what the run found: it says the file is a header, then
    // the endpoints the sweep declined to ask, then one chunk per endpoint, then
    // a footer, each section ending in its own terminator. A reader holding it
    // knows to expect a dormancy count, chunks and a `finalised` footer; a
    // reader holding it without `finalised` knows the run did not finish; a
    // reader holding neither has a run from before this stage, which promised
    // nothing. True the moment it is written, which is what a graph that is
    // never rewritten requires: it is not a promise about the future that a
    // later chunk would have to retract.
    //
    // Its section structure is ALMOST what `emit_nquads` output has: that
    // function omits the dormancy section, so a document it produced carries
    // this quad and no `sw:dormantCount`. It has no production caller, so no
    // published run has that shape, but the two are no longer the same document
    // and this comment used to say they were.
    //
    // The stronger reading, that the file reached disk incrementally, becomes
    // true when `run_sweep` writes the sections as they finish; no run emitted
    // before that is published.
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity),
        nn(HEADER_TERMINATOR)?,
        Term::Literal(Literal::new_simple_literal("incremental")),
        graph,
    ));
    serialize(&quads)
}

/// One endpoint's slice of all four fact families: everything a chunk needs to
/// stand on its own.
///
/// Per endpoint rather than per family because the chunk is the unit of
/// truncation: a crash then costs the endpoints not yet written and nothing
/// else. That only holds if every fact about an endpoint is in one chunk, which
/// is also what makes the per-chunk duplicate-subject pre-scan complete while
/// seeing one chunk at a time.
pub struct EndpointFacts<'a> {
    pub run: &'a RunId,
    /// The endpoint this chunk is about, named separately from the facts rather
    /// than read off the first of them: the terminator needs it even for a
    /// chunk whose every fact was dropped as unpublishable, and a chunk with no
    /// facts at all is a shape `run_sweep` can produce.
    pub endpoint: &'a str,
    pub rows: &'a [MeasurementRow],
    pub declarations_read: &'a [DeclarationsRead],
    pub not_measured: &'a [NotMeasured],
    pub content_samples: &'a [ContentSample],
}

/// The one thing that survives between two chunks: which endpoints this writer
/// has already been asked for.
///
/// Nothing else, deliberately. Any other carried state would make a later chunk
/// depend on an earlier one, and a chunk that cannot be read on its own is not
/// a unit of truncation.
#[derive(Debug, Default)]
pub struct EmitState {
    /// Attempted, not written: the insert happens before any quad is built and
    /// is never rolled back, so an endpoint whose IRI is malformed (empty
    /// chunk, no marker, warnings only) is recorded here too. That is the safe
    /// direction, because the reason to refuse a retry is that the pre-scan
    /// cannot see across two chunks, and that is true whether or not the first
    /// attempt published anything.
    attempted: BTreeSet<String>,
}

impl EmitState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Write one endpoint's chunk, ending with the terminator that says this
/// activity finished that endpoint.
pub fn emit_endpoint(state: &mut EmitState, facts: EndpointFacts) -> anyhow::Result<String> {
    let EndpointFacts {
        run,
        endpoint: subject_endpoint,
        rows,
        declarations_read,
        not_measured,
        content_samples,
    } = facts;
    // A second chunk for one endpoint would put its facts either side of the
    // pre-scan below, so a pair measured twice with differing results would be
    // published twice instead of dropped, on one subject, in a graph that is
    // never rewritten. Refusing the second chunk is the direction that cannot
    // publish a wrong answer.
    if !state.attempted.insert(subject_endpoint.to_string()) {
        tracing::warn!(
            endpoint = %subject_endpoint,
            "dropping a repeat chunk: every fact about an endpoint must be in one chunk, or the \
             duplicate-subject pre-scan cannot see both halves of a disagreement"
        );
        return Ok(String::new());
    }
    let (graph, activity) = graph_and_activity(run)?;
    let mut quads: Vec<Quad> = Vec::new();

    // The chunk's endpoint as a term, once. An endpoint that is not a
    // well-formed IRI can be neither typed nor marked, and every fact about it
    // is skipped below for the same reason; the marker at the end of this
    // function is where that loss is logged.
    let subject_term = NamedNode::new(subject_endpoint);

    // Typed per chunk, so every chunk types every endpoint it names. A chunk
    // carrying facts about an endpoint it never typed is not self-contained,
    // and a reader of a truncated file may hold this chunk and not the one that
    // typed the endpoint.
    //
    // Per chunk rather than per run changes nothing about what `emit_nquads`
    // publishes, because that gives each endpoint exactly one chunk and the old
    // run-scoped set was keyed on the endpoint too. The difference appears only
    // where a second chunk names an endpoint an earlier chunk already typed: a
    // run-scoped set would leave it untyped in the chunk that has its facts,
    // which is `a_chunk_types_every_endpoint_it_publishes_a_fact_about`.
    let mut typed_endpoints: BTreeSet<String> = BTreeSet::new();
    // The chunk's own endpoint is typed here rather than at its first fact, so
    // the claim above holds for a chunk with no publishable fact as well. Such
    // a chunk still carries the completion marker below, and a marker naming a
    // resource this graph never types is a join a consumer cannot complete. The
    // quad lands where it did before, because typing at the first fact already
    // made it the chunk's first quad.
    if let Ok(n) = &subject_term {
        typed_endpoints.insert(subject_endpoint.to_string());
        quads.push(Quad::new(
            NamedOrBlankNode::NamedNode(n.clone()),
            rdf::TYPE.into_owned(),
            Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
            graph.clone(),
        ));
    }

    // Two entries of one fact list can name the same (endpoint, metric) pair.
    // Under the running index they replaced, such a pair got two subjects;
    // a subject derived from the pair puts them on one node, and if they
    // disagree that node carries two `dqv:value` literals in a graph that is
    // never rewritten. So the subjects are counted before anything is
    // published: a subject carrying two different payloads publishes nothing
    // (see `conflicted`), and a byte-identical repeat is skipped because RDF is
    // a set and writing it twice says nothing new.
    //
    // Per chunk, which is complete because an endpoint's facts are all in one
    // chunk: two facts about one pair are in this chunk or nowhere. The check
    // above is what defends that premise here; `run_sweep` checks it at the
    // other end.
    //
    // `registry::dedupe` drops a repeated endpoint before a sweep starts, so
    // nothing in the current pipeline is expected to reach this: it is belt and
    // braces, kept because the cost of being wrong is permanent. `dedupe`'s own
    // doc comment records the reciprocal half of that arrangement. `dedupe` is
    // also the only guard the `declarations_read` loop below has, because that
    // fact's subject is the bare endpoint IRI rather than one of these derived
    // ones, so there is no run-scoped subject here to count.
    //
    // Keyed on the subject, so it catches a pair repeated WITHIN one kind. It
    // says nothing across kinds: a pair that is both measured and declared
    // not-measured lands on two nodes by design (`FactKind::prefix`) and both
    // are published. Keeping those two consistent is `run_sweep`'s job, not
    // this function's.
    //
    // The payload is what would distinguish two facts about one pair: a verdict
    // with its level and elapsed time, a reason, or a value list with its
    // truncation flag. The sample payload is deliberately the raw value list
    // and not the `writable` subset the loop actually publishes, so two samples
    // differing only in a value that cannot be written are treated as
    // conflicting and neither is published. That is the conservative direction,
    // and computing `writable` twice to sharpen it would double the
    // unwritable-value warnings for no gain a consumer can see.
    let mut payloads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in rows {
        if let Ok(s) = subject_iri(FactKind::Measurement, run, &r.endpoint, &r.metric_id) {
            payloads
                .entry(s.into_string())
                .or_default()
                .insert(format!("{}|{:?}|{:?}", r.verdict.slug(), r.level, r.elapsed_ms));
        }
    }
    for fact in not_measured {
        if let Ok(s) = subject_iri(FactKind::NotMeasured, run, &fact.endpoint, &fact.metric_id) {
            payloads
                .entry(s.into_string())
                .or_default()
                .insert(fact.reason.slug().to_string());
        }
    }
    for sample in content_samples {
        if let Ok(s) =
            subject_iri(FactKind::ContentSample, run, &sample.endpoint, &sample.metric_id)
        {
            payloads
                .entry(s.into_string())
                .or_default()
                .insert(format!("{:?}|{}", sample.values, sample.truncated));
        }
    }
    // Which subjects have already been written, so a byte-identical repeat is
    // written once.
    let mut emitted: BTreeSet<String> = BTreeSet::new();

    for r in rows {
        // A malformed endpoint IRI is one bad row, not a reason to discard a
        // whole sweep: this runs after all the probing, so aborting here would
        // turn hours of work into no output at all. The registry is seeded from
        // a real-world dump known to contain junk URLs.
        let endpoint = match NamedNode::new(&r.endpoint) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %r.endpoint,
                    metric = %r.metric_id,
                    error = %e,
                    "skipping measurement: endpoint is not a valid IRI"
                );
                continue;
            }
        };
        // Same non-fatal handling as the junk endpoint above, and for the same
        // reason: an `Err` from here costs the caller a written chunk, so a
        // metric id that cannot be part of a subject must cost one fact rather
        // than an endpoint's whole output.
        let m = match subject_iri(FactKind::Measurement, run, &r.endpoint, &r.metric_id) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %r.endpoint,
                    metric = %r.metric_id,
                    error = %e,
                    "skipping measurement: no subject can be derived for it"
                );
                continue;
            }
        };
        if conflicted(&payloads, m.as_str()) {
            tracing::warn!(
                endpoint = %r.endpoint,
                metric = %r.metric_id,
                verdict = %r.verdict.slug(),
                "dropping measurement: this (endpoint, metric) pair was measured more than once \
                 with differing results, so nothing is published about it"
            );
            continue;
        }
        if !emitted.insert(m.as_str().to_string()) {
            continue;
        }
        let subj = NamedOrBlankNode::NamedNode(m.clone());

        if typed_endpoints.insert(r.endpoint.clone()) {
            // The endpoint is the same resource across every metric, so it is
            // typed once per chunk rather than once per measurement.
            quads.push(Quad::new(
                NamedOrBlankNode::NamedNode(endpoint.clone()),
                rdf::TYPE.into_owned(),
                Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
                graph.clone(),
            ));
        }

        quads.push(Quad::new(
            subj.clone(),
            rdf::TYPE.into_owned(),
            Term::NamedNode(nn(&format!("{DQV}QualityMeasurement"))?),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}computedOn"))?,
            Term::NamedNode(endpoint),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}isMeasurementOf"))?,
            Term::NamedNode(nn(&format!("urn:sparqlwatch:metric:{}", r.metric_id))?),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}value"))?,
            Term::Literal(Literal::new_simple_literal(r.verdict.slug())),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{PROV}wasGeneratedBy"))?,
            Term::NamedNode(activity.clone()),
            graph.clone(),
        ));
        if let Some(ms) = r.elapsed_ms {
            quads.push(Quad::new(
                subj.clone(),
                nn("urn:sparqlwatch:elapsedMs")?,
                Term::Literal(Literal::new_typed_literal(ms.to_string(), xsd::INTEGER)),
                graph.clone(),
            ));
        }
        if let Some(Level(l)) = r.level {
            quads.push(Quad::new(
                subj,
                nn("urn:sparqlwatch:level")?,
                Term::Literal(Literal::new_typed_literal(l.to_string(), xsd::INTEGER)),
                graph.clone(),
            ));
        }
    }

    // A separate IRI space from a measurement's, deliberately: these two must
    // never share a subject, or a consumer joining on the measurement IRI
    // lands on a node that both has and has not a verdict. The two spaces stay
    // apart because `FactKind::prefix` puts a different segment in each, so a
    // measurement and a not-measured fact about the very same (endpoint,
    // metric) pair are still two nodes.
    for fact in not_measured {
        // Same non-fatal handling as a measurement row: one junk endpoint
        // string must not cost the whole sweep its output.
        let endpoint = match NamedNode::new(&fact.endpoint) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %fact.endpoint,
                    metric = %fact.metric_id,
                    error = %e,
                    "skipping not-measured fact: endpoint is not a valid IRI"
                );
                continue;
            }
        };
        let node = match subject_iri(FactKind::NotMeasured, run, &fact.endpoint, &fact.metric_id) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %fact.endpoint,
                    metric = %fact.metric_id,
                    error = %e,
                    "skipping not-measured fact: no subject can be derived for it"
                );
                continue;
            }
        };
        if conflicted(&payloads, node.as_str()) {
            tracing::warn!(
                endpoint = %fact.endpoint,
                metric = %fact.metric_id,
                reason = %fact.reason.slug(),
                "dropping not-measured fact: this (endpoint, metric) pair was declined more than \
                 once for differing reasons, so nothing is published about it"
            );
            continue;
        }
        if !emitted.insert(node.as_str().to_string()) {
            continue;
        }
        let subj = NamedOrBlankNode::NamedNode(node);
        if typed_endpoints.insert(fact.endpoint.clone()) {
            quads.push(Quad::new(
                NamedOrBlankNode::NamedNode(endpoint.clone()),
                rdf::TYPE.into_owned(),
                Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
                graph.clone(),
            ));
        }
        quads.push(Quad::new(
            subj.clone(),
            rdf::TYPE.into_owned(),
            Term::NamedNode(nn("urn:sparqlwatch:NotMeasured")?),
            graph.clone(),
        ));
        // Sparqlwatch-owned predicates, deliberately NOT `dqv:computedOn` and
        // `dqv:isMeasurementOf`. Reusing a predicate whose declared domain is a
        // class we are not is an assertion, not a convenience: DQV gives
        // `dqv:computedOn` the domain `dqv:QualityMeasurement` and
        // `dqv:isMeasurementOf` the domain `qb:Observation`, so either one on
        // this subject entails, under plain RDFS, that a quality measurement
        // exists here. It does not. A consumer materialising domains would then
        // read every declined pair as a measurement whose `dqv:value` went
        // missing, which is the exact confusion this fact type was minted to
        // prevent, published once per declined (endpoint, metric) pair.
        //
        // These two carry no `rdfs:domain` and no `rdfs:range` anywhere,
        // because an undeclared predicate entails nothing. Endpoint and metric
        // stay joinable; nothing about a measurement is claimed.
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:notMeasuredOn")?,
            Term::NamedNode(endpoint),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:notMeasuredMetric")?,
            Term::NamedNode(nn(&format!("urn:sparqlwatch:metric:{}", fact.metric_id))?),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:notMeasuredReason")?,
            Term::Literal(Literal::new_simple_literal(fact.reason.slug())),
            graph.clone(),
        ));
        // The same link every measurement carries. Without it, a consumer
        // holding this fact can reach the policy that produced it
        // (`urn:sparqlwatch:maxCost`, which hangs off the activity) only by
        // assuming the `run:{at}` / `activity:{at}` naming convention or by
        // relying on being inside the right named graph. In a run where every
        // metric was declined there is no measurement to borrow the activity
        // from.
        //
        // Unlike the two DQV properties replaced above, this one's declared
        // domain is safe to accept: PROV-O gives `prov:wasGeneratedBy` the
        // domain `prov:Entity`, and this fact genuinely is a thing our run
        // produced. The test is not "does the predicate have a domain" but
        // "is the domain something we are", and here it is.
        //
        // No `dqv:value` and no `sw:level`: nothing was measured, so there is
        // nothing to state. The reason is all the new information there is.
        quads.push(Quad::new(
            subj,
            nn(&format!("{PROV}wasGeneratedBy"))?,
            Term::NamedNode(activity.clone()),
            graph.clone(),
        ));
    }

    // A third IRI space, for the same reason the first two are separate: three
    // fact types live in one graph, and a consumer joining on any of their
    // subjects must never land on a node that is several things at once. Its
    // own `FactKind` variant, so a sample of a pair is a different node from
    // that pair's measurement even though both derive from the same key.
    for sample in content_samples {
        // Same non-fatal handling as every other fact: one junk endpoint out of
        // 548 must not cost the sweep its whole output after the probing is
        // already paid for.
        let endpoint = match NamedNode::new(&sample.endpoint) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %sample.endpoint,
                    metric = %sample.metric_id,
                    error = %e,
                    "skipping content sample: endpoint is not a valid IRI"
                );
                continue;
            }
        };
        // Serialized before the size is written, because the size counts what
        // is published. A value that is not a well-formed IRI cannot be
        // written, and a size counting it would disagree with the
        // `sampledValue` quads beside it with nothing in the graph to explain
        // the gap: a consumer asking "how many classes did we see" would get
        // two published answers and no way to tell an unwritable value from an
        // emitter that lost one. So the size is always verifiable from the
        // graph itself, and a dropped value is reported to the log, which is
        // the only place that can carry a fact about a value we could not
        // name.
        //
        // In the endpoint's order, untouched: not sorted, not filtered beyond
        // what serialization forces, not deduplicated. `SELECT DISTINCT`
        // already deduplicated, and reordering would discard evidence about
        // the endpoint for no gain.
        let mut writable: Vec<NamedNode> = Vec::with_capacity(sample.values.len());
        for value in &sample.values {
            match NamedNode::new(value) {
                Ok(n) => writable.push(n),
                Err(e) => tracing::warn!(
                    endpoint = %sample.endpoint,
                    metric = %sample.metric_id,
                    value = %value,
                    error = %e,
                    "skipping sampled value: not a valid IRI"
                ),
            }
        }
        if writable.len() < sample.values.len() {
            tracing::warn!(
                endpoint = %sample.endpoint,
                metric = %sample.metric_id,
                bound = sample.values.len(),
                published = writable.len(),
                "sampled values were dropped as unwritable; sampleSize counts what is published, \
                 so the graph stays self-consistent and this log is where the loss is recorded"
            );
        }
        // Nothing writable is nothing to publish, for the same reason a probe
        // that bound nothing publishes no sample: a node saying `sampleSize 0`
        // gives a consumer something to join that says nothing, and here it
        // would falsely read as "this endpoint has no classes" when what
        // happened is that we could not write down the ones it named. The log
        // above is what records that.
        if writable.is_empty() {
            continue;
        }
        let node =
            match subject_iri(FactKind::ContentSample, run, &sample.endpoint, &sample.metric_id) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        endpoint = %sample.endpoint,
                        metric = %sample.metric_id,
                        error = %e,
                        "skipping content sample: no subject can be derived for it"
                    );
                    continue;
                }
            };
        if conflicted(&payloads, node.as_str()) {
            tracing::warn!(
                endpoint = %sample.endpoint,
                metric = %sample.metric_id,
                bound = sample.values.len(),
                "dropping content sample: this (endpoint, metric) pair was sampled more than once \
                 with differing results, so nothing is published about it"
            );
            continue;
        }
        if !emitted.insert(node.as_str().to_string()) {
            continue;
        }
        let subj = NamedOrBlankNode::NamedNode(node);
        if typed_endpoints.insert(sample.endpoint.clone()) {
            quads.push(Quad::new(
                NamedOrBlankNode::NamedNode(endpoint.clone()),
                rdf::TYPE.into_owned(),
                Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
                graph.clone(),
            ));
        }
        quads.push(Quad::new(
            subj.clone(),
            rdf::TYPE.into_owned(),
            Term::NamedNode(nn("urn:sparqlwatch:ContentSample")?),
            graph.clone(),
        ));
        // Sparqlwatch-owned predicates throughout, for the reason spelled out
        // on `ContentSample` itself: `void:classPartition` and `void:class`
        // both declare `rdfs:domain void:Dataset`, so borrowing either would
        // entail that this observation is a dataset description. An
        // undeclared predicate entails nothing, which is exactly what we want
        // to say.
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:sampledFrom")?,
            Term::NamedNode(endpoint),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:sampledBy")?,
            Term::NamedNode(nn(&format!("urn:sparqlwatch:metric:{}", sample.metric_id))?),
            graph.clone(),
        ));
        for value in &writable {
            quads.push(Quad::new(
                subj.clone(),
                nn("urn:sparqlwatch:sampledValue")?,
                Term::NamedNode(value.clone()),
                graph.clone(),
            ));
        }
        // After the values, not before them, which is rule 2 of the section
        // protocol at the top of this file. A chunk cut inside the value list
        // used to leave `sampleSize 200, sampleTruncated false` standing beside
        // three values, which `endpoint_content.rq` matches and the page renders
        // as two hundred classes sampled, complete. With the summary last, the
        // same cut loses the sample instead of misstating it, and a lost sample
        // is a shape every consumer already handles.
        //
        // `writable.len()` still, at its new site, because the published size
        // has to count what is actually published rather than what was bound.
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:sampleTruncated")?,
            Term::Literal(Literal::new_typed_literal(
                if sample.truncated { "true" } else { "false" },
                xsd::BOOLEAN,
            )),
            graph.clone(),
        ));
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:sampleSize")?,
            Term::Literal(Literal::new_typed_literal(
                writable.len().to_string(),
                xsd::INTEGER,
            )),
            graph.clone(),
        ));
        // The same link every other fact carries, and safe for the same
        // reason: PROV-O gives `prov:wasGeneratedBy` the domain `prov:Entity`,
        // and a sample genuinely is a thing this run produced. Without it, a
        // consumer holding a sample cannot reach the run that took it except
        // by assuming the naming convention.
        quads.push(Quad::new(
            subj,
            nn(&format!("{PROV}wasGeneratedBy"))?,
            Term::NamedNode(activity.clone()),
            graph.clone(),
        ));
    }

    for fact in declarations_read {
        // Same non-fatal handling as a measurement row above: one bad
        // endpoint string must not cost every other endpoint its fact.
        let endpoint = match NamedNode::new(&fact.endpoint) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    endpoint = %fact.endpoint,
                    error = %e,
                    "skipping declarationsRead fact: endpoint is not a valid IRI"
                );
                continue;
            }
        };
        if typed_endpoints.insert(fact.endpoint.clone()) {
            quads.push(Quad::new(
                NamedOrBlankNode::NamedNode(endpoint.clone()),
                rdf::TYPE.into_owned(),
                Term::NamedNode(nn(&format!("{DCAT}DataService"))?),
                graph.clone(),
            ));
        }
        quads.push(Quad::new(
            NamedOrBlankNode::NamedNode(endpoint),
            nn("urn:sparqlwatch:declarationsRead")?,
            Term::Literal(Literal::new_typed_literal(
                if fact.read { "true" } else { "false" },
                xsd::BOOLEAN,
            )),
            graph.clone(),
        ));
    }

    // The chunk's terminator, and its last line. Says that this activity
    // finished this endpoint: true when written, and what makes the chunk the
    // unit of truncation. A reader holding it holds every fact this run has
    // about the endpoint; a reader that does not may drop the lines above
    // rather than read a fragment of a sample as a whole one. Per endpoint and
    // not per run, because at 548 endpoints a run-level marker would say
    // nothing about which ones were reached, and Task 4's read tier needs
    // exactly that.
    //
    // What "completed" includes: an endpoint the prober FAILED on. `run_sweep`
    // writes a chunk for every endpoint it was given, including the ones a
    // panicked group lost, so a chunk of `prober-failed` declines carries this
    // marker too. The fact is about the chunk being whole, not about the probe
    // succeeding. Withholding it there would leave that endpoint looking, on a
    // run that later crashed, exactly like one the run never reached, and the
    // read tier would report a later sweep as never having got to an endpoint
    // whose failure that sweep published.
    //
    // An endpoint that is not a valid IRI has had every fact above skipped for
    // that reason, and there is no term to name it with here either, so the
    // chunk is empty and carries no marker. The warnings above are where that
    // loss is recorded.
    match subject_term {
        Ok(n) => quads.push(Quad::new(
            NamedOrBlankNode::NamedNode(activity),
            nn(CHUNK_TERMINATOR)?,
            Term::NamedNode(n),
            graph,
        )),
        Err(e) => tracing::warn!(
            endpoint = %subject_endpoint,
            error = %e,
            "chunk carries no completion marker: the endpoint is not a valid IRI, so nothing in \
             the graph can name it"
        ),
    }
    serialize(&quads)
}

/// The run-level facts that are only true once every chunk has been written.
pub struct RunFooter<'a> {
    pub run: &'a RunId,
    /// How many endpoints the run failed on, from `Sweep::failed_endpoints`.
    /// It is NOT what tells a reader whether a run finished. `sw:finalised` is,
    /// and a run that died never reached the footer that carries either one, so
    /// this count is only meaningful beside it. What it adds is the total, where
    /// the per-endpoint facts say `prober-failed` one at a time.
    ///
    /// In the footer rather than the header because it summarises the chunks,
    /// and rule 2 of the section protocol forbids publishing a summary before
    /// the things it summarises. It is only trustworthy in the presence of
    /// `sw:finalised` below, which is the quad that says the summary is over a
    /// complete run.
    pub failed_endpoints: usize,
}

/// Write the footer, ending with the terminator that says the run finished.
pub fn emit_footer(footer: RunFooter) -> anyhow::Result<String> {
    let RunFooter { run, failed_endpoints } = footer;
    let (graph, activity) = graph_and_activity(run)?;
    let quads: Vec<Quad> = vec![
        // Published on every run, including the ordinary `0`, rather than only
        // when it is non-zero: a consumer has to be able to read "this run failed
        // on nothing" as a fact, and an absent quad would be indistinguishable
        // from a run emitted before this fact existed.
        Quad::new(
            NamedOrBlankNode::NamedNode(activity.clone()),
            nn("urn:sparqlwatch:failedEndpoints")?,
            Term::Literal(Literal::new_typed_literal(failed_endpoints.to_string(), xsd::INTEGER)),
            graph.clone(),
        ),
        // The footer's terminator, and the last line the writer ever writes. It
        // says THAT the run finished, and it is the single quad a consumer tests
        // for, so "footer present" does not depend on which of these two a
        // consumer happened to look for.
        //
        // A boolean, and not `prov:endedAtTime`, which would be the better fact.
        // Nothing here can produce that instant soundly: this module reads no clock
        // by design (see `RunHeader::max_cost`), `std` cannot format a `SystemTime`
        // as `xsd:dateTime`, and no date library is in the lock file under this
        // stage's no-new-dependencies rule. Hand-rolling civil-time arithmetic from
        // Unix seconds would write a typed `xsd:dateTime` into a graph that is
        // never rewritten, where `nn` validates IRIs and not literal lexical
        // spaces, so a month-length mistake would be permanent. An `--ended-at`
        // flag cannot work either, though it is what `--at` does one field over: a
        // process cannot know at launch when it will finish, so the flag would
        // publish a predicted future, which is the thing this fact exists to avoid.
        //
        // If a duration is ever wanted, a monotonic `Instant` delta published as
        // integer seconds needs no formatting and asserts nothing about the future.
        // Do NOT derive one by summing `elapsedMs`: each of those starts after the
        // per-host gate is acquired and excludes politeness by design, and at
        // concurrency 4 their sum is not even a bound on the run.
        Quad::new(
            NamedOrBlankNode::NamedNode(activity),
            nn(FOOTER_TERMINATOR)?,
            Term::Literal(Literal::new_typed_literal("true", xsd::BOOLEAN)),
            graph,
        ),
    ];
    serialize(&quads)
}

/// The endpoints one run publishes a chunk for, in first-appearance order over
/// the union of all four fact lists.
///
/// The union and not `rows`, because an endpoint can appear in one list only: a
/// prober-failed endpoint is in `not_measured` alone, and an endpoint whose
/// description was fetched but whose metrics all failed to grade is in
/// `declarations_read` alone (see
/// `declarations_read_emits_a_boolean_quad_shaped_for_the_run`, which passes
/// `rows: &[]`). Deriving the sequence from `rows` would silently publish no
/// chunk for either.
///
/// First appearance and not sorted, because for a run whose lists are already
/// grouped by endpoint, which is what `assemble` produces, it reproduces the
/// order the whole-run emitter used, so the order-sensitive tests and anyone
/// diffing two files by eye see no movement. The four lists are visited in the
/// order a chunk writes them, for the same reason.
fn endpoint_order(
    rows: &[MeasurementRow],
    not_measured: &[NotMeasured],
    content_samples: &[ContentSample],
    declarations_read: &[DeclarationsRead],
) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();
    let all = rows
        .iter()
        .map(|r| r.endpoint.as_str())
        .chain(not_measured.iter().map(|f| f.endpoint.as_str()))
        .chain(content_samples.iter().map(|s| s.endpoint.as_str()))
        .chain(declarations_read.iter().map(|d| d.endpoint.as_str()));
    for endpoint in all {
        if seen.insert(endpoint) {
            order.push(endpoint.to_string());
        }
    }
    order
}

/// One named graph per run keeps history immutable and lets a bad run be
/// dropped wholesale.
///
/// The composition of `emit_header`, one `emit_endpoint` per endpoint of the
/// run, and `emit_footer`. Keeping this function is what lets a caller that
/// holds a whole run in memory keep working unchanged while `run_sweep` writes
/// the very same sections one at a time as they finish.
///
/// Takes a single `RunEmission` rather than its fields positionally; see that
/// struct's doc comment for why.
pub fn emit_nquads(input: RunEmission) -> anyhow::Result<String> {
    let RunEmission {
        run,
        generated_at,
        metric_revision,
        rows,
        declarations_read,
        not_measured,
        max_cost,
        content_samples,
        concurrency,
        failed_endpoints,
    } = input;
    // No dormancy section, deliberately, and this is the one place the claim
    // above (that this function's section structure is the same as a
    // chunk-by-chunk run's) stops being true. This function has no production
    // caller: `main.rs` writes through `RunWriter`, which is what pairs the
    // header with `emit_dormancy`. What the omission buys is both frozen
    // baselines below staying green, and what it costs is that a caller who
    // brought this function back into production would publish a run that says
    // nothing about what it declined to ask. Such a caller has to write the
    // section, which means taking the facts as a parameter.
    let mut out = emit_header(RunHeader {
        run,
        generated_at,
        metric_revision,
        max_cost,
        concurrency,
        dormant: &[],
    })?;
    let mut state = EmitState::new();
    for endpoint in endpoint_order(rows, not_measured, content_samples, declarations_read) {
        // The four flat lists carry no endpoint grouping this function can rely
        // on, so each is sliced by endpoint here. Cloned rather than borrowed
        // because an endpoint's entries need not be contiguous, and a run's
        // four lists are a few thousand small structs at registry scale.
        let rows: Vec<MeasurementRow> =
            rows.iter().filter(|r| r.endpoint == endpoint).cloned().collect();
        let not_measured: Vec<NotMeasured> =
            not_measured.iter().filter(|f| f.endpoint == endpoint).cloned().collect();
        let content_samples: Vec<ContentSample> =
            content_samples.iter().filter(|s| s.endpoint == endpoint).cloned().collect();
        let declarations_read: Vec<DeclarationsRead> =
            declarations_read.iter().filter(|d| d.endpoint == endpoint).cloned().collect();
        out.push_str(&emit_endpoint(
            &mut state,
            EndpointFacts {
                run,
                endpoint: &endpoint,
                rows: &rows,
                declarations_read: &declarations_read,
                not_measured: &not_measured,
                content_samples: &content_samples,
            },
        )?);
    }
    out.push_str(&emit_footer(RunFooter { run, failed_endpoints })?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Cost;
    use crate::verdict::{Level, Verdict};
    use oxrdfio::RdfParser;

    const REV: &str = "abc123";
    const AT: &str = "2026-08-20T08:00:00Z";

    fn row(endpoint: &str, metric_id: &str, verdict: Verdict) -> MeasurementRow {
        MeasurementRow {
            endpoint: endpoint.into(),
            metric_id: metric_id.into(),
            verdict,
            level: None,
            elapsed_ms: Some(1),
        }
    }

    fn rows() -> Vec<MeasurementRow> {
        vec![
            MeasurementRow {
                endpoint: "https://qlever.dev/api/osm-planet".into(),
                metric_id: "geo-functions".into(),
                verdict: Verdict::UndeclaredButVerified,
                level: None,
                elapsed_ms: Some(210),
            },
            MeasurementRow {
                endpoint: "https://data.kkg.kadaster.nl/query".into(),
                metric_id: "service-description".into(),
                verdict: Verdict::DeclaredOnly,
                level: Some(Level(2)),
                elapsed_ms: Some(5714),
            },
        ]
    }

    /// Parse the emitted document back into quads. Asserting over quads rather
    /// than over substrings of the serialization is what makes these tests
    /// about the published data model instead of about text.
    fn quads_of(out: &str) -> Vec<Quad> {
        RdfParser::from_format(RdfFormat::NQuads)
            .for_slice(out.as_bytes())
            .map(|q| q.expect("emitted document must parse as N-Quads"))
            .collect()
    }

    fn emit(rows: &[MeasurementRow]) -> Vec<Quad> {
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: REV,
            rows,
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        quads_of(&out)
    }

    fn objects<'a>(qs: &'a [Quad], predicate: &str) -> Vec<&'a Term> {
        qs.iter().filter(|q| q.predicate.as_str() == predicate).map(|q| &q.object).collect()
    }

    /// One emission exercising every fact list `emit_nquads` writes, so the
    /// frozen baseline in `only_the_subjects_changed` covers every predicate
    /// the emitter can produce. No two entries of any list share an
    /// (endpoint, metric) pair, so the baseline says nothing about the
    /// duplicate case, which has its own test.
    fn baseline_emission() -> String {
        let rows = vec![
            MeasurementRow {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "availability".into(),
                verdict: Verdict::Verified,
                level: None,
                elapsed_ms: Some(12),
            },
            MeasurementRow {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "cors".into(),
                verdict: Verdict::Absent,
                level: Some(Level(2)),
                elapsed_ms: Some(34),
            },
            MeasurementRow {
                endpoint: "https://b.example/sparql".into(),
                metric_id: "service-description".into(),
                verdict: Verdict::DeclaredOnly,
                level: Some(Level(1)),
                elapsed_ms: None,
            },
        ];
        let declarations_read = vec![
            DeclarationsRead { endpoint: "https://a.example/sparql".into(), read: true },
            DeclarationsRead { endpoint: "https://b.example/sparql".into(), read: false },
        ];
        let not_measured = vec![NotMeasured {
            endpoint: "https://b.example/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let content_samples = vec![ContentSample {
            endpoint: "https://a.example/sparql".into(),
            metric_id: "classes".into(),
            values: vec![
                "https://a.example/vocab#Zebra".into(),
                "https://a.example/vocab#Apple".into(),
            ],
            truncated: true,
        }];
        emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows,
            declarations_read: &declarations_read,
            not_measured: &not_measured,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &content_samples,
        })
        .unwrap()
    }

    #[test]
    fn only_the_subjects_changed() {
        // Baseline frozen from the pre-1c-b3 emitter on 2026-08-23: the sorted
        // (predicate, object) multiset and the quad count for the fixture
        // `baseline_emission` builds. Subjects are deliberately not part of it,
        // because subjects are the one thing this stage changes. Everything
        // else is what a published consumer reads, so a change here is a change
        // to the data model and has to be deliberate.
        //
        // Moved once, on 2026-08-23, by stage 1c-b3: the emitter now publishes
        // two more run-level facts on the activity, `urn:sparqlwatch:concurrency`
        // and `urn:sparqlwatch:failedEndpoints`, so the multiset gained those two
        // pairs and the count went from 41 to 43. Nothing else moved. The
        // baseline is not weakened to tolerate additions, because its whole
        // value is that an unintended predicate cannot slip in; an intended one
        // costs this paragraph.
        //
        // Moved again, on 2026-08-24, by stage 1c-b4: a run is now written as a
        // header, one chunk per endpoint and a footer, and each of those three
        // sections ends with a terminator a reader of a truncated file can test
        // for. So the multiset gained `urn:sparqlwatch:emission` once,
        // `urn:sparqlwatch:completedEndpoint` once per endpoint (two here) and
        // `urn:sparqlwatch:finalised` once, and the count went from 43 to 47.
        // Nothing else moved. The sample's `sampleSize` and `sampleTruncated`
        // did move to after its values, which this baseline cannot see because
        // it compares an order-insensitive multiset, and that is the point:
        // rearranging what is published is not changing what is published.
        const BASELINE_PAIRS: &[(&str, &str)] = &[
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/prov#Activity>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:ContentSample>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:NotMeasured>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://a.example/sparql>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://a.example/sparql>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://b.example/sparql>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:availability>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:cors>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:service-description>"),
        ("http://www.w3.org/ns/dqv#value", "\"absent\""),
        ("http://www.w3.org/ns/dqv#value", "\"declared-only\""),
        ("http://www.w3.org/ns/dqv#value", "\"verified\""),
        ("http://www.w3.org/ns/prov#generatedAtTime", "\"2026-08-20T08:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://a.example/sparql>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://b.example/sparql>"),
        ("urn:sparqlwatch:concurrency", "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:declarationsRead", "\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:declarationsRead", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:elapsedMs", "\"12\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:elapsedMs", "\"34\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:emission", "\"incremental\""),
        ("urn:sparqlwatch:failedEndpoints", "\"0\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:finalised", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:level", "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:level", "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:maxCost", "\"cheap\""),
        ("urn:sparqlwatch:metricDefinitionRevision", "\"abc123\""),
        ("urn:sparqlwatch:notMeasuredMetric", "<urn:sparqlwatch:metric:classes>"),
        ("urn:sparqlwatch:notMeasuredOn", "<https://b.example/sparql>"),
        ("urn:sparqlwatch:notMeasuredReason", "\"cost-ceiling\""),
        (
            "urn:sparqlwatch:proberVersion",
            // Not a frozen literal: the emitter writes `env!("CARGO_PKG_VERSION")`
            // at `:303`, so a version bump would otherwise red this test with
            // "a predicate or an object changed", which is not what changed.
            concat!("\"", env!("CARGO_PKG_VERSION"), "\""),
        ),
        ("urn:sparqlwatch:sampleSize", "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:sampleTruncated", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:sampledBy", "<urn:sparqlwatch:metric:classes>"),
        ("urn:sparqlwatch:sampledFrom", "<https://a.example/sparql>"),
        ("urn:sparqlwatch:sampledValue", "<https://a.example/vocab#Apple>"),
        ("urn:sparqlwatch:sampledValue", "<https://a.example/vocab#Zebra>"),
        ];
        const BASELINE_QUADS: usize = 47;
        assert_frozen(&baseline_emission(), BASELINE_PAIRS, BASELINE_QUADS);
    }

    /// Compare an emission against a frozen (predicate, object) multiset and a
    /// frozen quad count.
    ///
    /// Shared by the two frozen baselines rather than written twice, so the two
    /// cannot drift into comparing different things and then disagree about
    /// what "unchanged" means.
    fn assert_frozen(out: &str, expected_pairs: &[(&str, &str)], expected_quads: usize) {
        let qs = quads_of(out);
        let mut pairs: Vec<(String, String)> = qs
            .iter()
            .map(|q| (q.predicate.as_str().to_string(), q.object.to_string()))
            .collect();
        pairs.sort();
        let expected: Vec<(String, String)> = expected_pairs
            .iter()
            .map(|(p, o)| ((*p).to_string(), (*o).to_string()))
            .collect();
        assert_eq!(pairs, expected, "a predicate or an object changed");
        assert_eq!(qs.len(), expected_quads, "the quad count changed");
    }

    /// One emission covering every fact family, built for the frozen baseline
    /// that stage 1c-b4's split into header, per-endpoint chunks and footer is
    /// measured against.
    ///
    /// Deliberately richer than `baseline_emission`: four endpoints, a
    /// measurement with a level and one without, a measurement with an elapsed
    /// time and one without, both `NotMeasuredReason` variants, a sample with
    /// the truncation flag each way, a `declarationsRead` both true and false,
    /// one endpoint that appears ONLY in `declarations_read` and one that
    /// appears ONLY in `not_measured`. The last two are what force the split's
    /// endpoint sequence to come from the union of all four lists: an endpoint
    /// derived from `rows` alone would lose both of them and publish no chunk
    /// for either.
    fn split_baseline_emission() -> String {
        let rows = vec![
            MeasurementRow {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "availability".into(),
                verdict: Verdict::Verified,
                level: None,
                elapsed_ms: Some(12),
            },
            MeasurementRow {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "cors".into(),
                verdict: Verdict::Absent,
                level: Some(Level(2)),
                elapsed_ms: None,
            },
            MeasurementRow {
                endpoint: "https://b.example/sparql".into(),
                metric_id: "service-description".into(),
                verdict: Verdict::DeclaredOnly,
                level: Some(Level(1)),
                elapsed_ms: Some(340),
            },
        ];
        let declarations_read = vec![
            DeclarationsRead { endpoint: "https://a.example/sparql".into(), read: true },
            DeclarationsRead { endpoint: "https://b.example/sparql".into(), read: false },
            DeclarationsRead { endpoint: "https://c.example/sparql".into(), read: true },
        ];
        let not_measured = vec![
            NotMeasured {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "properties".into(),
                reason: NotMeasuredReason::CostCeiling,
            },
            // A prober-failed endpoint carries no measurement and no
            // `declarationsRead` fact, which is why this one appears in no
            // other list.
            NotMeasured {
                endpoint: "https://d.example/sparql".into(),
                metric_id: "availability".into(),
                reason: NotMeasuredReason::ProberFailed,
            },
        ];
        let content_samples = vec![
            ContentSample {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "classes".into(),
                values: vec![
                    "https://a.example/vocab#Zebra".into(),
                    "https://a.example/vocab#Apple".into(),
                ],
                truncated: true,
            },
            ContentSample {
                endpoint: "https://b.example/sparql".into(),
                metric_id: "classes".into(),
                values: vec!["https://b.example/vocab#Mango".into()],
                truncated: false,
            },
        ];
        emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows,
            declarations_read: &declarations_read,
            not_measured: &not_measured,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(4).unwrap(),
            failed_endpoints: 1,
            content_samples: &content_samples,
        })
        .unwrap()
    }

    #[test]
    fn the_split_emission_publishes_what_the_whole_one_did() {
        // Baseline frozen from the pre-1c-b4 emitter on 2026-08-24 over the
        // synthesized run `split_baseline_emission` builds: the sorted
        // (predicate, object) multiset and the quad count. Subjects are not
        // part of it, because stage 1c-b3 froze those separately in
        // `only_the_subjects_changed` and this stage does not touch them.
        //
        // Frozen as a constant here rather than compared against `emit_nquads`:
        // once `emit_nquads` IS the composition of header, chunks and footer,
        // comparing the two is a tautology that can never fail again, so the
        // only proof that the split changed nothing has to be an artefact
        // captured before the split happened.
        //
        // Moved once, on 2026-08-24, by the split itself: the three section
        // terminators are new facts, so the multiset gained
        // `urn:sparqlwatch:emission` once, `urn:sparqlwatch:completedEndpoint`
        // once per endpoint (four here) and `urn:sparqlwatch:finalised` once,
        // and the count went from 58 to 64. Nothing else moved, which is what
        // this test exists to say. The baseline is not weakened to tolerate
        // additions, because its whole value is that an unintended predicate
        // cannot slip in; an intended one costs this paragraph.
        const BASELINE_PAIRS: &[(&str, &str)] = &[
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dcat#DataService>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/dqv#QualityMeasurement>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<http://www.w3.org/ns/prov#Activity>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:ContentSample>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:ContentSample>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:NotMeasured>"),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:sparqlwatch:NotMeasured>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://a.example/sparql>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://a.example/sparql>"),
        ("http://www.w3.org/ns/dqv#computedOn", "<https://b.example/sparql>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:availability>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:cors>"),
        ("http://www.w3.org/ns/dqv#isMeasurementOf", "<urn:sparqlwatch:metric:service-description>"),
        ("http://www.w3.org/ns/dqv#value", "\"absent\""),
        ("http://www.w3.org/ns/dqv#value", "\"declared-only\""),
        ("http://www.w3.org/ns/dqv#value", "\"verified\""),
        ("http://www.w3.org/ns/prov#generatedAtTime", "\"2026-08-20T08:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("http://www.w3.org/ns/prov#wasGeneratedBy", "<urn:sparqlwatch:activity:r1>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://a.example/sparql>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://b.example/sparql>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://c.example/sparql>"),
        ("urn:sparqlwatch:completedEndpoint", "<https://d.example/sparql>"),
        ("urn:sparqlwatch:concurrency", "\"4\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:declarationsRead", "\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:declarationsRead", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:declarationsRead", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:elapsedMs", "\"12\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:elapsedMs", "\"340\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:emission", "\"incremental\""),
        ("urn:sparqlwatch:failedEndpoints", "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:finalised", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:level", "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:level", "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:maxCost", "\"cheap\""),
        ("urn:sparqlwatch:metricDefinitionRevision", "\"abc123\""),
        ("urn:sparqlwatch:notMeasuredMetric", "<urn:sparqlwatch:metric:availability>"),
        ("urn:sparqlwatch:notMeasuredMetric", "<urn:sparqlwatch:metric:properties>"),
        ("urn:sparqlwatch:notMeasuredOn", "<https://a.example/sparql>"),
        ("urn:sparqlwatch:notMeasuredOn", "<https://d.example/sparql>"),
        ("urn:sparqlwatch:notMeasuredReason", "\"cost-ceiling\""),
        ("urn:sparqlwatch:notMeasuredReason", "\"prober-failed\""),
        (
            "urn:sparqlwatch:proberVersion",
            // Not a frozen literal: the emitter writes `env!("CARGO_PKG_VERSION")`,
            // so a version bump would otherwise red this test with "a predicate or
            // an object changed", which is not what changed.
            concat!("\"", env!("CARGO_PKG_VERSION"), "\""),
        ),
        ("urn:sparqlwatch:sampleSize", "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:sampleSize", "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        ("urn:sparqlwatch:sampleTruncated", "\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:sampleTruncated", "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"),
        ("urn:sparqlwatch:sampledBy", "<urn:sparqlwatch:metric:classes>"),
        ("urn:sparqlwatch:sampledBy", "<urn:sparqlwatch:metric:classes>"),
        ("urn:sparqlwatch:sampledFrom", "<https://a.example/sparql>"),
        ("urn:sparqlwatch:sampledFrom", "<https://b.example/sparql>"),
        ("urn:sparqlwatch:sampledValue", "<https://a.example/vocab#Apple>"),
        ("urn:sparqlwatch:sampledValue", "<https://a.example/vocab#Zebra>"),
        ("urn:sparqlwatch:sampledValue", "<https://b.example/vocab#Mango>"),
        ];
        const BASELINE_QUADS: usize = 64;
        assert_frozen(&split_baseline_emission(), BASELINE_PAIRS, BASELINE_QUADS);
    }

    /// `docs/design/section-terminators.md`, embedded at compile time so a
    /// deleted or moved file is a build failure rather than a skipped test.
    const WIRE_FORMAT: &str = include_str!("../../docs/design/section-terminators.md");

    /// The section-to-predicate table in `WIRE_FORMAT`'s fenced block, in the
    /// order it lists them.
    ///
    /// Parsed rather than restated. A restatement here would be a third copy of
    /// the table, and the point of that file is that there are two, one per
    /// language, each checked against it.
    fn wire_format_table() -> Vec<(String, String)> {
        let mut parts = WIRE_FORMAT.split("```");
        parts.next().expect("the prose before the fenced block");
        let block = parts.next().expect("a fenced block naming the terminators");
        assert_eq!(parts.count(), 1, "exactly one fenced block in the wire format file");
        block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.split_whitespace();
                let section = fields.next().expect("a section name").to_string();
                let predicate = fields.next().expect("a predicate IRI").to_string();
                assert!(fields.next().is_none(), "a section and a predicate: {line}");
                (section, predicate)
            })
            .collect()
    }

    /// The Rust half of the wire format.
    ///
    /// The four spellings are written here and recognised in
    /// `web/load_run.py`, with nothing in either language connecting them:
    /// renaming one here alone used to leave both suites green while every
    /// partial run cut back to its header, discarding every endpoint the crash
    /// preserved. `docs/design/section-terminators.md` is the one file both
    /// sides read, and `web/tests/test_load_run.py` checks the loader's set
    /// against the same table. The whole table is compared, in order, so a
    /// fifth terminator added on one side alone reds this too.
    #[test]
    fn the_four_terminators_are_the_four_the_design_doc_names() {
        assert_eq!(
            wire_format_table(),
            vec![
                ("header".to_string(), HEADER_TERMINATOR.to_string()),
                ("dormancy".to_string(), DORMANCY_TERMINATOR.to_string()),
                ("chunk".to_string(), CHUNK_TERMINATOR.to_string()),
                ("footer".to_string(), FOOTER_TERMINATOR.to_string()),
            ],
            "the emitter's terminators and docs/design/section-terminators.md have to agree"
        );
    }

    /// The three terminators as a serialized line spells them, in predicate
    /// position.
    ///
    /// Derived from the emitter's own constants, where they used to be written
    /// out again here. What that restatement bought was catching a rename in
    /// the emitter; the test above buys the same thing against a file the
    /// loader is checked against too, which a restatement in this module could
    /// not do.
    fn in_predicate_position(predicate: &str) -> String {
        format!("<{predicate}>")
    }

    #[test]
    fn each_section_ends_with_its_own_terminator() {
        // A reader of a truncated file decides what to keep by looking for
        // these, so a terminator that is not the last line of its section makes
        // the whole scheme unsound: a crash between the terminator and the rest
        // of the section would leave a fragment the loader calls complete.
        let header = emit_header(RunHeader {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            dormant: &[],
        })
        .unwrap();
        assert!(
            header.trim_end().lines().next_back().unwrap().contains(&in_predicate_position(HEADER_TERMINATOR)),
            "the header must end with its terminator: {header}"
        );

        let chunk = emit_endpoint(
            &mut EmitState::new(),
            EndpointFacts {
                run: &RunId("r1".into()),
                endpoint: "https://a.example/sparql",
                rows: &[row("https://a.example/sparql", "cors", Verdict::Verified)],
                declarations_read: &[DeclarationsRead {
                    endpoint: "https://a.example/sparql".into(),
                    read: true,
                }],
                not_measured: &[],
                content_samples: &[],
            },
        )
        .unwrap();
        assert!(
            chunk.trim_end().lines().next_back().unwrap().contains(&in_predicate_position(CHUNK_TERMINATOR)),
            "a chunk must end with its terminator: {chunk}"
        );

        let dormancy = emit_dormancy(
            &RunId("r1".into()),
            &[DormancyFact {
                endpoint: "https://slow.example/sparql".into(),
                dormant_since: Some("2026-08-13T08:00:00Z".into()),
                reason: SkipReason::Automatic,
            }],
        )
        .unwrap();
        assert!(
            dormancy.trim_end().lines().next_back().unwrap().contains(&in_predicate_position(DORMANCY_TERMINATOR)),
            "the dormancy section must end with its terminator: {dormancy}"
        );

        let footer = emit_footer(RunFooter { run: &RunId("r1".into()), failed_endpoints: 0 }).unwrap();
        assert!(
            footer.trim_end().lines().next_back().unwrap().contains(&in_predicate_position(FOOTER_TERMINATOR)),
            "the footer must end with its terminator: {footer}"
        );
    }

    // --- The dormancy section ------------------------------------------------
    //
    // The fourth section, between the header's terminator and the first chunk.
    // It is what makes a narrowed sweep honest: a run graph naming 486 of 543
    // endpoints and saying nothing about the other 57 leaves a consumer to read
    // silence about an endpoint as a claim that nothing was found there.

    /// Two facts, one relegated by the machine and one held by an operator, so
    /// a test over them covers both slugs `SkipReason` can produce.
    fn dormancy_facts() -> Vec<DormancyFact> {
        vec![
            DormancyFact {
                endpoint: "https://slow.example/sparql".into(),
                dormant_since: Some("2026-08-13T08:00:00Z".into()),
                reason: SkipReason::Automatic,
            },
            DormancyFact {
                endpoint: "https://held.example/sparql".into(),
                dormant_since: None,
                reason: SkipReason::OperatorHold,
            },
        ]
    }

    /// A sink whose bytes stay readable after the writer that owns it is gone.
    /// Same shape as `write.rs`'s own test sink, and duplicated rather than
    /// shared because a `#[cfg(test)]` item is not visible across modules.
    #[derive(Clone)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One whole run written the way production writes it.
    ///
    /// Through `RunWriter` and not by concatenating the emitters here, because
    /// the order of the header and the dormancy section is `RunWriter`'s
    /// decision: a test that called `emit_dormancy` on its own could not see
    /// that order at all, which is the thing two of the tests below are about.
    fn run_document(dormant: &[DormancyFact]) -> String {
        let run = RunId("r1".into());
        let sink = Shared(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let bytes = std::sync::Arc::clone(&sink.0);
        let mut writer = crate::write::RunWriter::with_writer(
            sink,
            RunHeader {
                run: &run,
                generated_at: AT,
                metric_revision: REV,
                max_cost: Cost::Cheap,
                concurrency: NonZeroUsize::new(1).unwrap(),
                dormant,
            },
        )
        .unwrap();
        writer
            .write_endpoint(EndpointFacts {
                run: &run,
                endpoint: "https://probed.example/sparql",
                rows: &[row("https://probed.example/sparql", "cors", Verdict::Verified)],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[],
            })
            .unwrap();
        writer.finish(RunFooter { run: &run, failed_endpoints: 0 }).unwrap();
        let written = bytes.lock().unwrap().clone();
        String::from_utf8(written).unwrap()
    }

    #[test]
    fn a_dormancy_fact_types_the_endpoint_and_names_it_on_the_activity() {
        // Three things, each of them a rule the codebase already states. The
        // `sw:dormantEndpoint` quad on the activity mirrors
        // `sw:completedEndpoint`: without it an endpoint's dormancy triples
        // hang off nothing an activity reaches, so no CONSTRUCT can date them
        // without inventing a triple and both read queries lose their join. The
        // `dcat:DataService` type is here because `emit_endpoint` types every
        // endpoint in its own chunk so a truncated reader never holds an untyped
        // endpoint, and a skipped endpoint has no chunk. And the reason is the
        // slug `SkipReason` publishes rather than a sentence, because a page
        // renders it.
        let out = emit_dormancy(&RunId("r1".into()), &dormancy_facts()).unwrap();
        let qs = quads_of(&out);
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:dormantEndpoint")
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            vec!["<https://held.example/sparql>", "<https://slow.example/sparql>"],
            "the activity names every endpoint the sweep declined, ordered by endpoint: {out}"
        );
        for q in qs.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:dormantEndpoint") {
            assert_eq!(
                q.subject.to_string(),
                "<urn:sparqlwatch:activity:r1>",
                "the naming quad's subject is the activity, like sw:completedEndpoint's"
            );
        }
        assert_eq!(
            typed_in(&qs),
            dormancy_facts().iter().map(|f| f.endpoint.clone()).collect::<BTreeSet<String>>(),
            "every endpoint the section names is typed in it: {out}"
        );
        let reasons: BTreeSet<String> = qs
            .iter()
            .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:dormancyReason")
            .map(|q| format!("{} {}", q.subject, q.object))
            .collect();
        assert_eq!(
            reasons,
            BTreeSet::from([
                "<https://held.example/sparql> \"operator-hold\"".to_string(),
                "<https://slow.example/sparql> \"automatic\"".to_string(),
            ]),
            "the reason is a slug on the endpoint: {out}"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:dormantSince")
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            vec!["\"2026-08-13T08:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"],
            "the instant is published as the state holds it, typed: {out}"
        );
    }

    #[test]
    fn a_fact_with_no_instant_emits_no_dormant_since_triple() {
        // An operator hold carries no relegation instant, and neither does an
        // entry a hand edit left without one. A zero or an empty literal would
        // be a claim about a date nobody recorded, which is the same reason
        // `elapsed_ms: None` publishes no quad rather than a 0.
        let out = emit_dormancy(
            &RunId("r1".into()),
            &[DormancyFact {
                endpoint: "https://held.example/sparql".into(),
                dormant_since: None,
                reason: SkipReason::OperatorHold,
            }],
        )
        .unwrap();
        let qs = quads_of(&out);
        assert!(
            objects(&qs, "urn:sparqlwatch:dormantSince").is_empty(),
            "no instant means no quad, never an empty or zero one: {out}"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:dormantEndpoint").len(),
            1,
            "the endpoint is still named and still typed: {out}"
        );
    }

    #[test]
    fn the_section_sits_between_the_header_terminator_and_the_first_chunk() {
        let out = run_document(&dormancy_facts());
        let header_end =
            out.find(&in_predicate_position(HEADER_TERMINATOR)).expect("the header's terminator");
        let count = out
            .find(&in_predicate_position(DORMANCY_TERMINATOR))
            .expect("the dormancy section's terminator");
        let first_chunk =
            out.find(&in_predicate_position(CHUNK_TERMINATOR)).expect("a chunk's terminator");
        let first_dormancy = out
            .find(&in_predicate_position("urn:sparqlwatch:dormantEndpoint"))
            .expect("a dormancy fact");
        assert!(
            header_end < first_dormancy && first_dormancy < count && count < first_chunk,
            "the header, then the dormancy section, then the chunks: {out}"
        );
    }

    #[test]
    fn the_count_is_the_sections_last_line_and_its_subject_is_the_activity() {
        // Last, because this module's rule 2 forbids a fact family summarising
        // itself before the things it summarises: a cut inside the section has
        // to lose the section, never leave a count standing beside two of the
        // 48 endpoints it counted. On the activity, because
        // `load_run._is_terminator_line` anchors on that subject as well as on
        // the predicate, and a terminator on any other subject is not a section
        // boundary this writer put there.
        let out = emit_dormancy(&RunId("r1".into()), &dormancy_facts()).unwrap();
        let last = out.trim_end().lines().next_back().unwrap();
        assert!(
            last.contains(&in_predicate_position(DORMANCY_TERMINATOR)),
            "the count closes the section: {out}"
        );
        assert!(
            last.starts_with("<urn:sparqlwatch:activity:r1>"),
            "and its subject is the activity: {last}"
        );
        assert_eq!(
            objects(&quads_of(&out), DORMANCY_TERMINATOR)
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            vec!["\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"],
            "and it counts the groups published beside it: {out}"
        );
    }

    #[test]
    fn a_run_that_skipped_nothing_still_publishes_a_zero_count() {
        // A reader has to be able to tell "this sweep declined nothing" from
        // "this run predates dormancy", and an absent quad says both at once.
        // Same rule as `sw:failedEndpoints`, published at 0 for this reason.
        let out = emit_dormancy(&RunId("r1".into()), &[]).unwrap();
        assert_eq!(
            objects(&quads_of(&out), DORMANCY_TERMINATOR)
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            vec!["\"0\"^^<http://www.w3.org/2001/XMLSchema#integer>"],
            "the count is published even at zero: {out}"
        );
        assert_eq!(quads_of(&out).len(), 1, "and nothing else is: {out}");
    }

    #[test]
    fn a_header_truncated_file_still_holds_no_endpoint_facts() {
        // The property `load_run._holds_endpoint_facts` rests on, asserted from
        // this side so an edit that moves a dormancy quad up into the header
        // reds here rather than silently in Python. A file cut inside its
        // header carries no terminator at all, and the loader then decides
        // whether to take it by asking whether every subject is the activity:
        // an endpoint-subject quad before `sw:emission` would make such a
        // fragment load whole, win the newest-run aggregate with no
        // `sw:emission` beside it, and silence unfinished-run detection for
        // every endpoint on the site.
        let out = run_document(&dormancy_facts());
        let terminator = in_predicate_position(HEADER_TERMINATOR);
        let header: String = out
            .lines()
            .take_while(|line| !line.contains(&terminator))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(!header.is_empty(), "the header has lines before its terminator: {out}");
        for q in quads_of(&header) {
            assert!(
                q.subject.to_string().starts_with("<urn:sparqlwatch:activity:"),
                "a header-truncated file must carry the activity's metadata and nothing about \
                 an endpoint, found {q}"
            );
        }
    }

    #[test]
    fn a_chunk_marks_the_endpoint_it_finished_and_types_it_itself() {
        // Two things a chunk needs to stand alone. The marker is what makes it
        // the unit of truncation, and it is per endpoint rather than per run
        // because Task 4's read tier asks "did this run reach THIS endpoint",
        // which a run-level marker cannot answer. The `dcat:DataService` type
        // has to be in the same chunk, because a chunk carrying facts about an
        // endpoint the reader has no type for is not self-contained.
        let url = "https://a.example/sparql";
        let chunk = emit_endpoint(
            &mut EmitState::new(),
            EndpointFacts {
                run: &RunId("r1".into()),
                endpoint: url,
                rows: &[row(url, "cors", Verdict::Verified)],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[],
            },
        )
        .unwrap();
        let qs = quads_of(&chunk);
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:completedEndpoint"),
            vec![&Term::NamedNode(NamedNode::new(url).unwrap())],
            "the chunk names the endpoint it finished, once"
        );
        assert_eq!(
            typed_in(&qs),
            BTreeSet::from([url.to_string()]),
            "the chunk types its own endpoint, once: {chunk}"
        );
    }

    /// Every endpoint a chunk publishes a fact about, as a set, read off the
    /// `dcat:DataService` quads.
    fn typed_in(qs: &[Quad]) -> BTreeSet<String> {
        let service = Term::NamedNode(NamedNode::new(format!("{DCAT}DataService")).unwrap());
        qs.iter()
            .filter(|q| q.predicate == rdf::TYPE && q.object == service)
            .map(|q| q.subject.to_string().trim_matches(['<', '>']).to_string())
            .collect()
    }

    #[test]
    fn a_chunk_types_every_endpoint_it_publishes_a_fact_about() {
        // The property `typed_endpoints` being per chunk actually buys, and the
        // only way it can be made to bite: two chunks, one `EmitState`, where
        // the second names an endpoint the first already typed. Per chunk, the
        // second chunk types it again; run-scoped, the second chunk would carry
        // the endpoint's facts and no type for it, and a reader holding only
        // that chunk could not tell what the subject is.
        //
        // The mixed chunk below is a caller error rather than a shape
        // `emit_nquads` produces, and Task 3 will refuse it at the writer. This
        // is the emitter's contract underneath that refusal: the chunk is what
        // a truncated file preserves, so it has to be complete on its own terms
        // even when the caller was wrong. The same reasoning covers a repeat
        // chunk for one endpoint, which `EmitState` refuses here rather than
        // letting it reach this code.
        let run = RunId("r1".into());
        let a = "https://a.example/sparql";
        let b = "https://b.example/sparql";
        let mut state = EmitState::new();

        let first = emit_endpoint(
            &mut state,
            EndpointFacts {
                run: &run,
                endpoint: a,
                rows: &[row(a, "cors", Verdict::Verified)],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[],
            },
        )
        .unwrap();
        assert_eq!(typed_in(&quads_of(&first)), BTreeSet::from([a.to_string()]));

        let second = emit_endpoint(
            &mut state,
            EndpointFacts {
                run: &run,
                endpoint: b,
                rows: &[row(b, "cors", Verdict::Verified), row(a, "availability", Verdict::Absent)],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[],
            },
        )
        .unwrap();
        assert_eq!(
            typed_in(&quads_of(&second)),
            BTreeSet::from([a.to_string(), b.to_string()]),
            "the second chunk types both endpoints it names, including the one an earlier \
             chunk already typed: {second}"
        );
    }

    #[test]
    fn a_chunk_with_no_publishable_fact_still_types_the_endpoint_it_marks() {
        // What an empty chunk means, and why it is emitted rather than skipped:
        // the marker is the run's only statement that it reached this endpoint,
        // which is the fact Task 4's read tier reads instead of inferring
        // "never attempted" from absence. So the chunk is worth writing even
        // when every fact about the endpoint was dropped as unpublishable, or
        // when the caller had none to give.
        //
        // It has to type the endpoint too. A marker naming a resource the graph
        // never types is a join a consumer cannot complete, and this is the one
        // shape where typing at the first fact would have typed nothing.
        // `emit_nquads` cannot reach it, because `endpoint_order` only yields
        // endpoints that appear in some list; a direct caller can.
        let url = "https://e.example/sparql";
        let chunk = emit_endpoint(
            &mut EmitState::new(),
            EndpointFacts {
                run: &RunId("r1".into()),
                endpoint: url,
                rows: &[],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[],
            },
        )
        .unwrap();
        let qs = quads_of(&chunk);
        assert_eq!(
            typed_in(&qs),
            BTreeSet::from([url.to_string()]),
            "an empty chunk types the endpoint it marks: {chunk}"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:completedEndpoint"),
            vec![&Term::NamedNode(NamedNode::new(url).unwrap())],
            "and it still says the run reached it: {chunk}"
        );
        assert_eq!(qs.len(), 2, "and says nothing else: {chunk}");
    }

    #[test]
    fn a_repeat_chunk_for_one_endpoint_publishes_nothing() {
        // An endpoint's facts must all be in one chunk, because that is what
        // makes the per-chunk duplicate-subject pre-scan complete. Split across
        // two chunks, a pair measured twice with differing results would be
        // published twice on one subject instead of dropped, in a graph that is
        // never rewritten. So the second chunk is refused, which is the only
        // direction that cannot publish a confident wrong answer.
        let run = RunId("r1".into());
        let url = "https://a.example/sparql";
        let mut state = EmitState::new();
        let verified = [row(url, "cors", Verdict::Verified)];
        let absent = [row(url, "cors", Verdict::Absent)];
        let chunk = |state: &mut EmitState, rows: &[MeasurementRow]| {
            emit_endpoint(
                state,
                EndpointFacts {
                    run: &run,
                    endpoint: url,
                    rows,
                    declarations_read: &[],
                    not_measured: &[],
                    content_samples: &[],
                },
            )
            .unwrap()
        };
        assert!(!chunk(&mut state, &verified).is_empty(), "the first chunk is written");
        assert_eq!(
            chunk(&mut state, &absent),
            "",
            "the repeat chunk publishes nothing, not a second verdict"
        );
    }

    #[test]
    fn a_sample_publishes_its_size_after_the_values_it_counts() {
        // The failure this ordering exists to prevent: a chunk cut inside the
        // value list used to leave `sampleSize 200, sampleTruncated false`
        // standing beside three values, which `endpoint_content.rq` matches and
        // the page renders as two hundred classes sampled, complete. With the
        // summary last, the same cut loses the sample instead of misstating it.
        let url = "https://a.example/sparql";
        let out = emit_endpoint(
            &mut EmitState::new(),
            EndpointFacts {
                run: &RunId("r1".into()),
                endpoint: url,
                rows: &[],
                declarations_read: &[],
                not_measured: &[],
                content_samples: &[ContentSample {
                    endpoint: url.into(),
                    metric_id: "classes".into(),
                    values: vec![
                        "https://a.example/vocab#Zebra".into(),
                        "https://a.example/vocab#Apple".into(),
                    ],
                    truncated: false,
                }],
            },
        )
        .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let line_of = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} in {out}"))
        };
        let last_value = lines
            .iter()
            .rposition(|l| l.contains("<urn:sparqlwatch:sampledValue>"))
            .expect("the fixture publishes values");
        assert!(
            last_value < line_of("<urn:sparqlwatch:sampleSize>"),
            "sampleSize must come after the last value it counts: {out}"
        );
        assert!(
            last_value < line_of("<urn:sparqlwatch:sampleTruncated>"),
            "sampleTruncated must come after the values it describes: {out}"
        );

        // And the size still counts what is published rather than what was
        // bound, which is the property the move must not cost.
        let qs = quads_of(&out);
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampleSize"),
            vec![&Term::Literal(Literal::new_typed_literal("2", xsd::INTEGER))]
        );
    }

    #[test]
    fn a_run_cut_after_a_chunk_carries_no_finalised_and_loses_only_the_rest() {
        // The shape a crash actually leaves, asserted on the document rather
        // than on the writer: everything up to a chunk terminator is a complete
        // header plus complete chunks, it parses, and it does not claim the run
        // finished. `failedEndpoints` goes with `finalised` because it
        // summarises the chunks, so a truncated run cannot publish a count that
        // was only true of a whole one.
        let whole = split_baseline_emission();
        let cut_at = whole
            .lines()
            .position(|l| l.contains(&in_predicate_position(CHUNK_TERMINATOR)))
            .expect("the fixture has at least one chunk");
        let truncated: String =
            whole.lines().take(cut_at + 1).map(|l| format!("{l}\n")).collect();

        let qs = quads_of(&truncated);
        assert!(
            qs.iter().any(|q| q.predicate.as_str() == "urn:sparqlwatch:emission"),
            "the header survives, so a reader knows to expect chunks"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:completedEndpoint").len(),
            1,
            "exactly the one endpoint that finished is marked"
        );
        for absent in ["urn:sparqlwatch:finalised", "urn:sparqlwatch:failedEndpoints"] {
            assert!(
                objects(&qs, absent).is_empty(),
                "{absent} must not appear in a run that did not finish: {truncated}"
            );
        }
    }

    #[test]
    fn an_endpoint_named_only_by_one_fact_list_still_gets_a_chunk() {
        // The endpoint sequence comes from the union of all four lists, because
        // an endpoint can appear in one of them only: a prober-failed endpoint
        // is in `not_measured` alone, and an endpoint whose description was read
        // but whose metrics produced no row is in `declarations_read` alone.
        // Deriving the sequence from `rows` would publish no chunk for either,
        // so neither would be marked as reached and Task 4 would read both as
        // never attempted.
        let qs = quads_of(&split_baseline_emission());
        let marked: BTreeSet<String> = objects(&qs, "urn:sparqlwatch:completedEndpoint")
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert!(
            marked.contains("<https://c.example/sparql>"),
            "the declarations-only endpoint is marked: {marked:?}"
        );
        assert!(
            marked.contains("<https://d.example/sparql>"),
            "the not-measured-only endpoint is marked: {marked:?}"
        );
    }

    /// Every subject in `qs`, as a set, so a test can compare two emissions
    /// without depending on the order the quads came out in.
    fn subjects_of(qs: &[Quad]) -> BTreeSet<String> {
        qs.iter().map(|q| q.subject.to_string()).collect()
    }

    #[test]
    fn an_emitted_subject_is_the_one_the_helper_builds() {
        // Ties emit's output to the helper, so a parameter-order mistake in a
        // four-argument function cannot pass. Two rows, two endpoints, two
        // metrics, so endpoint and metric cannot be swapped without the
        // assertion moving.
        let rows = vec![
            row("https://a.example/sparql", "cors", Verdict::Verified),
            row("https://b.example/sparql", "classes", Verdict::Absent),
        ];
        let qs = emit(&rows);
        let expected = subject_iri(
            FactKind::Measurement,
            &RunId("r1".into()),
            "https://a.example/sparql",
            "cors",
        )
        .unwrap();
        assert!(
            subjects_of(&qs).contains(&format!("<{}>", expected.as_str())),
            "emit did not use subject_iri: {:?}",
            subjects_of(&qs)
        );
    }

    #[test]
    fn a_subject_does_not_depend_on_where_its_row_sat() {
        // The property 1c-b4 needs: an endpoint's chunk can be emitted alone
        // and still name the node the full run would.
        //
        // Compared against the subjects the helper builds, not against the
        // other emission. Comparing the two emissions to each other passes
        // under the running index this stage replaced as well, because
        // reversing two rows leaves the subject SET `{...:0, ...:1}` unchanged.
        let run = RunId("r1".into());
        let expected: BTreeSet<String> = [
            ("https://a.example/sparql", "cors"),
            ("https://b.example/sparql", "classes"),
        ]
        .iter()
        .map(|(endpoint, metric)| {
            subject_iri(FactKind::Measurement, &run, endpoint, metric)
                .unwrap()
                .to_string()
        })
        .collect();
        let measured = |rows: &[MeasurementRow]| -> BTreeSet<String> {
            emit(rows)
                .iter()
                .filter(|q| {
                    q.predicate.as_ref() == rdf::TYPE
                        && q.object
                            == Term::NamedNode(
                                NamedNode::new("http://www.w3.org/ns/dqv#QualityMeasurement")
                                    .unwrap(),
                            )
                })
                .map(|q| q.subject.to_string())
                .collect()
        };
        assert_eq!(
            measured(&[
                row("https://a.example/sparql", "cors", Verdict::Verified),
                row("https://b.example/sparql", "classes", Verdict::Absent),
            ]),
            expected
        );
        assert_eq!(
            measured(&[
                row("https://b.example/sparql", "classes", Verdict::Absent),
                row("https://a.example/sparql", "cors", Verdict::Verified),
            ]),
            expected,
            "the same two rows in the other order must name the same two nodes"
        );
    }

    #[test]
    fn the_encoding_is_exactly_this() {
        // Pins uppercase hex, UTF-8-byte-wise encoding, and the two halves of
        // the encoder's contract that have no other seat: the whole unreserved
        // set is passed through, and nothing is normalised. Without an exact
        // string this test cannot fail, because the encoder's output is valid
        // IRI syntax by construction.
        //
        // Every part of the input is load-bearing, so read it before changing
        // it. `HTTP` and `A` fail a lowercasing encoder, which `registry::dedupe`
        // forbids because it keeps `http://x/sparql` and `http://X/sparql` as
        // two entries; the trailing `/` fails a normalising one, which the same
        // module keeps as two entries too; `a-b_c~d` covers all four
        // non-alphanumeric unreserved characters, so narrowing the set fails
        // rather than silently renaming every fact about `osm-planet`; the
        // space and the `\u{00e9}` cover the escaping path and the multi-byte
        // case.
        let s = subject_iri(
            FactKind::Measurement,
            &RunId("R".into()),
            "HTTP://A.example/a-b_c~d/p q\u{00e9}/",
            "cors",
        )
        .unwrap();
        assert_eq!(
            s.as_str(),
            "urn:sparqlwatch:measurement:R:\
             HTTP%3A%2F%2FA.example%2Fa-b_c~d%2Fp%20q%C3%A9%2F:cors"
        );
    }

    #[test]
    fn two_endpoints_the_registry_keeps_apart_get_different_subjects() {
        // The invariant the exact string above pins, said as the property it
        // exists for rather than as bytes. `registry::dedupe` treats a case
        // difference and a trailing slash as two entries (see
        // `a_near_duplicate_differing_by_a_trailing_slash_stays_two_entries`
        // there), so an encoder that folded either one would put two registry
        // entries on one subject. Their payloads differ by `elapsed_ms` alone,
        // so `conflicted` would then publish nothing about either of them, and
        // the `dqv:computedOn` beside the subject would still name them apart.
        let run = RunId("r1".into());
        let pairs = [
            ("http://x.example/sparql", "http://X.example/sparql"),
            ("http://x.example/sparql", "http://x.example/sparql/"),
        ];
        for (one, other) in pairs {
            assert_ne!(
                subject_iri(FactKind::Measurement, &run, one, "cors").unwrap(),
                subject_iri(FactKind::Measurement, &run, other, "cors").unwrap(),
                "{one} and {other} are two registry entries, so they are two subjects"
            );
        }
    }

    #[test]
    fn two_endpoints_differing_only_in_an_escape_get_different_subjects() {
        let run = RunId("r1".into());
        let one =
            subject_iri(FactKind::Measurement, &run, "http://a.example/x:y", "cors").unwrap();
        let two =
            subject_iri(FactKind::Measurement, &run, "http://a.example/x%3Ay", "cors").unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn an_endpoint_cannot_shift_the_metric_field() {
        let run = RunId("r1".into());
        let a = subject_iri(FactKind::Measurement, &run, "http://a.example/x", "cors").unwrap();
        let b =
            subject_iri(FactKind::Measurement, &run, "http://a.example/x:cors", "cors").unwrap();
        assert_ne!(a, b);
        assert!(a.as_str().ends_with(":cors"));
    }

    #[test]
    fn the_three_kinds_never_share_a_subject() {
        // A measurement and a not-measured fact about one pair must not land
        // on one node: it would both have and not have a verdict.
        let run = RunId("r1".into());
        let m =
            subject_iri(FactKind::Measurement, &run, "http://a.example/x", "classes").unwrap();
        let n =
            subject_iri(FactKind::NotMeasured, &run, "http://a.example/x", "classes").unwrap();
        let s =
            subject_iri(FactKind::ContentSample, &run, "http://a.example/x", "classes").unwrap();
        assert_ne!(m, n);
        assert_ne!(m, s);
        assert_ne!(n, s);
    }

    #[test]
    fn a_metric_id_that_would_make_a_subject_ambiguous_is_refused_by_the_helper() {
        // Enforced where it is relied on, not only where it is convenient.
        assert!(subject_iri(
            FactKind::Measurement,
            &RunId("r1".into()),
            "http://a.example/x",
            "has:classes"
        )
        .is_err());
    }

    #[test]
    fn a_conflicting_repeated_pair_publishes_nothing_about_that_pair() {
        // The collision the derived scheme makes possible and the old index
        // hid. ZERO values, not one: publishing the first of two
        // contradictory observations would assert `verified` about a pair also
        // measured `absent`.
        let rows = vec![
            row("https://a.example/sparql", "cors", Verdict::Verified),
            row("https://a.example/sparql", "cors", Verdict::Absent),
        ];
        let qs = emit(&rows);
        let values: Vec<&Term> = objects(&qs, "http://www.w3.org/ns/dqv#value");
        assert!(values.is_empty(), "published a verdict for a contradicted pair: {values:?}");
    }

    #[test]
    fn a_metric_id_that_cannot_be_a_subject_costs_one_fact_not_the_run() {
        // The same policy as a junk endpoint: skip, warn, keep the run. A `?`
        // here would discard the whole sweep's output at main.rs:192-202.
        let rows = vec![
            row("https://a.example/sparql", "has:classes", Verdict::Verified),
            row("https://b.example/sparql", "cors", Verdict::Absent),
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows,
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        });
        assert!(out.is_ok(), "a bad metric id must not cost the sweep its output");
        let qs = quads_of(&out.unwrap());
        assert_eq!(
            objects(&qs, "http://www.w3.org/ns/dqv#value"),
            vec![&Term::Literal(Literal::new_simple_literal("absent"))],
            "b.example's measurement survives and a.example's is the only one lost"
        );
    }

    #[test]
    fn every_quad_lands_in_the_run_graph() {
        let out = emit_nquads(RunEmission {
            run: &RunId("2026-08-20T08:00:00Z".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: REV,
            rows: &rows(),
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let expected = GraphName::NamedNode(
            NamedNode::new("urn:sparqlwatch:run:2026-08-20T08:00:00Z").unwrap(),
        );
        let qs = quads_of(&out);
        assert!(!qs.is_empty());
        for q in &qs {
            assert_eq!(q.graph_name, expected, "quad outside the run graph: {q}");
        }
    }

    #[test]
    fn measurements_carry_dqv_and_prov_terms() {
        let qs = emit(&rows());
        for p in [
            "http://www.w3.org/ns/dqv#isMeasurementOf",
            "http://www.w3.org/ns/dqv#computedOn",
            "http://www.w3.org/ns/dqv#value",
            "http://www.w3.org/ns/prov#generatedAtTime",
            "http://www.w3.org/ns/prov#wasGeneratedBy",
        ] {
            assert!(!objects(&qs, p).is_empty(), "no quad uses {p}");
        }
    }

    #[test]
    fn the_verdict_is_written_as_its_slug() {
        let qs = emit(&rows());
        let vals: Vec<String> = objects(&qs, "http://www.w3.org/ns/dqv#value")
            .iter()
            .map(|t| match t {
                Term::Literal(l) => l.value().to_string(),
                other => panic!("a verdict must be a literal, got {other}"),
            })
            .collect();
        assert_eq!(vals, vec!["undeclared-but-verified".to_string(), "declared-only".to_string()]);
    }

    #[test]
    fn a_graded_level_is_emitted_only_when_present() {
        let qs = emit(&rows());
        let levels: Vec<&Quad> =
            qs.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:level").collect();
        assert_eq!(levels.len(), 1, "exactly one row has a level");
        // ...and it hangs off the row that has one, not off the other.
        let graded = &levels[0].subject;
        let value_of_graded: Vec<&Term> = qs
            .iter()
            .filter(|q| &q.subject == graded && q.predicate.as_str() == "http://www.w3.org/ns/dqv#value")
            .map(|q| &q.object)
            .collect();
        assert_eq!(value_of_graded.len(), 1);
        assert_eq!(value_of_graded[0], &Term::Literal(Literal::new_simple_literal("declared-only")));
    }

    #[test]
    fn output_is_valid_nquads() {
        // The parser is the assertion: an unparseable document panics in
        // `quads_of`. Two rows produce more than one quad each.
        let qs = emit(&rows());
        assert!(qs.len() > rows().len());
    }

    #[test]
    fn an_unmeasured_elapsed_time_emits_no_quad_rather_than_zero() {
        // A metric that burned its whole budget must not assert it took zero
        // milliseconds: a "which endpoints are slow" query would read it as
        // the fastest observation in the dataset. Same principle as `Absent`:
        // do not state what you did not measure.
        let mut rs = rows();
        rs[0].elapsed_ms = None;
        let qs = emit(&rs);
        let elapsed: Vec<&Term> = objects(&qs, "urn:sparqlwatch:elapsedMs");
        assert_eq!(elapsed.len(), 1, "only the measured row carries an elapsed time");
        assert_eq!(
            elapsed[0],
            &Term::Literal(Literal::new_typed_literal("5714", xsd::INTEGER))
        );
    }

    #[test]
    fn a_row_with_an_invalid_endpoint_iri_is_skipped_not_fatal() {
        // A later stage seeds 548 real-world URLs from a dump known to contain
        // junk. Aborting the emission after all the probing is done would mean
        // hours of work produce no output at all because of one bad string.
        let mut rs = rows();
        rs.insert(
            1,
            MeasurementRow {
                endpoint: "not an iri at all".into(),
                metric_id: "availability".into(),
                verdict: Verdict::Verified,
                level: None,
                elapsed_ms: Some(3),
            },
        );
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: REV,
            rows: &rs,
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        })
            .expect("one junk endpoint must not discard the sweep");
        assert!(!out.contains("not an iri at all"));
        let qs = quads_of(&out);
        let measured: Vec<&Term> = objects(&qs, "http://www.w3.org/ns/dqv#computedOn");
        assert_eq!(measured.len(), 2, "the two well-formed rows survive");
        let vals: Vec<&Term> = objects(&qs, "http://www.w3.org/ns/dqv#value");
        assert!(
            !vals.contains(&&Term::Literal(Literal::new_simple_literal("verified"))),
            "the skipped row publishes no verdict: {vals:?}"
        );
    }

    #[test]
    fn measurements_endpoints_and_the_run_are_typed() {
        // Without rdf:type quads, a consumer query written against the spec's
        // data model returns zero rows.
        let qs = emit(&rows());
        let types: Vec<String> = qs
            .iter()
            .filter(|q| q.predicate.as_ref() == rdf::TYPE)
            .map(|q| match &q.object {
                Term::NamedNode(n) => n.as_str().to_string(),
                other => panic!("a type must be an IRI, got {other}"),
            })
            .collect();
        assert_eq!(types.iter().filter(|t| *t == "http://www.w3.org/ns/dqv#QualityMeasurement").count(), 2);
        assert_eq!(types.iter().filter(|t| *t == "http://www.w3.org/ns/prov#Activity").count(), 1);
        assert_eq!(types.iter().filter(|t| *t == "http://www.w3.org/ns/dcat#DataService").count(), 2);
    }

    #[test]
    fn each_distinct_endpoint_is_typed_once() {
        let mut rs = rows();
        rs.push(MeasurementRow {
            endpoint: "https://qlever.dev/api/osm-planet".into(),
            metric_id: "classes".into(),
            verdict: Verdict::Verified,
            level: None,
            elapsed_ms: Some(12),
        });
        let qs = emit(&rs);
        let services: Vec<&Quad> = qs
            .iter()
            .filter(|q| {
                q.predicate.as_ref() == rdf::TYPE
                    && q.object == Term::NamedNode(NamedNode::new("http://www.w3.org/ns/dcat#DataService").unwrap())
            })
            .collect();
        assert_eq!(services.len(), 2, "three rows over two endpoints type each endpoint once");
    }

    #[test]
    fn the_run_records_the_prober_version_and_the_metric_revision() {
        let qs = emit(&rows());
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:proberVersion"),
            vec![&Term::Literal(Literal::new_simple_literal(env!("CARGO_PKG_VERSION")))]
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:metricDefinitionRevision"),
            vec![&Term::Literal(Literal::new_simple_literal(REV))]
        );
    }

    #[test]
    fn declarations_read_emits_a_boolean_quad_shaped_for_the_run() {
        let facts = vec![DeclarationsRead { endpoint: "https://qlever.dev/api/osm-planet".into(), read: true }];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: REV,
            rows: &[],
            declarations_read: &facts,
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);
        let q = qs
            .iter()
            .find(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead")
            .expect("no declarationsRead quad emitted");
        assert_eq!(
            q.subject,
            NamedOrBlankNode::NamedNode(NamedNode::new("https://qlever.dev/api/osm-planet").unwrap()),
            "subject must be the endpoint IRI"
        );
        assert_eq!(
            q.object,
            Term::Literal(Literal::new_typed_literal("true", xsd::BOOLEAN)),
            "object must be an xsd:boolean literal"
        );
        assert_eq!(
            q.graph_name,
            GraphName::NamedNode(NamedNode::new("urn:sparqlwatch:run:r1").unwrap()),
            "the fact belongs to the run graph like everything else"
        );
    }

    #[test]
    fn every_endpoint_with_a_fact_gets_its_own_quad_whatever_the_boolean() {
        let facts = vec![
            DeclarationsRead { endpoint: "https://a.example/sparql".into(), read: true },
            DeclarationsRead { endpoint: "https://b.example/sparql".into(), read: false },
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: "2026-08-20T08:00:00Z",
            metric_revision: REV,
            rows: &[],
            declarations_read: &facts,
            not_measured: &[],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);
        let read_quads: Vec<&Quad> =
            qs.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead").collect();
        assert_eq!(read_quads.len(), 2, "one quad per endpoint, whatever the boolean");
    }

    #[test]
    fn a_not_measured_fact_carries_no_verdict_and_no_level() {
        let nq = emit_nquads(RunEmission {
            run: &RunId(AT.into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[NotMeasured {
                endpoint: "http://example.org/sparql".into(),
                metric_id: "classes".into(),
                reason: NotMeasuredReason::CostCeiling,
            }],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        })
        .unwrap();

        assert!(nq.contains("urn:sparqlwatch:NotMeasured"));
        assert!(nq.contains("cost-ceiling"));
        assert!(!nq.contains("dqv#value"),
                "nothing was measured, so there is no value to publish");
        assert!(!nq.contains("urn:sparqlwatch:level"));
    }

    #[test]
    fn a_not_measured_fact_does_not_collide_with_a_measurement() {
        // Both are subjects in the same graph. If they share an IRI, a consumer
        // joining on the measurement IRI gets a node that both has and has not a
        // verdict.
        let rows = vec![row("http://example.org/sparql", "availability", Verdict::Verified)];
        let nm = vec![NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let nq = emit_nquads(RunEmission {
            run: &RunId(AT.into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows,
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();

        // Collect the two subject sets separately, each by the rdf:type that
        // marks what kind of fact it is. Do NOT filter on a substring of one
        // IRI shape: `"measurement"` is not a substring of
        // `urn:sparqlwatch:not-measured:...`, so a single filter silently sees
        // only half the graph and the assertion becomes unfalsifiable. The two
        // fact types now share no predicate at all (see
        // `no_dqv_or_qb_predicate_ever_lands_on_a_not_measured_subject`), so
        // typing is the only honest way to tell them apart anyway.
        let subject = |line: &str| line.split_whitespace().next().unwrap_or("").to_string();
        let measured: std::collections::HashSet<String> = nq.lines()
            .filter(|l| l.contains("dqv#QualityMeasurement"))
            .map(&subject)
            .collect();
        let not_measured: std::collections::HashSet<String> = nq.lines()
            .filter(|l| l.contains("urn:sparqlwatch:NotMeasured"))
            .map(&subject)
            .collect();

        assert_eq!(measured.len(), 1, "the fixture has one measurement");
        assert_eq!(not_measured.len(), 1, "and one not-measured fact");
        assert!(
            measured.is_disjoint(&not_measured),
            "a measurement and a not-measured fact must never share a subject IRI: {measured:?} vs {not_measured:?}"
        );
    }

    #[test]
    fn a_not_measured_fact_names_the_endpoint_and_the_metric_it_skipped() {
        // The quads the spec asks for, read back as quads rather than as
        // text: a consumer asking "why is there no verdict for classes here"
        // must be able to join endpoint and metric.
        let nm = vec![NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);
        let subj = NamedOrBlankNode::NamedNode(
            subject_iri(
                FactKind::NotMeasured,
                &RunId("r1".into()),
                "http://example.org/sparql",
                "classes",
            )
            .unwrap(),
        );
        let of = |p: &str| -> Vec<&Term> {
            qs.iter()
                .filter(|q| q.subject == subj && q.predicate.as_str() == p)
                .map(|q| &q.object)
                .collect()
        };
        assert_eq!(
            of("urn:sparqlwatch:notMeasuredOn"),
            vec![&Term::NamedNode(NamedNode::new("http://example.org/sparql").unwrap())]
        );
        assert_eq!(
            of("urn:sparqlwatch:notMeasuredMetric"),
            vec![&Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:classes").unwrap())]
        );
        assert_eq!(
            of("urn:sparqlwatch:notMeasuredReason"),
            vec![&Term::Literal(Literal::new_simple_literal("cost-ceiling"))],
            "the reason is a slug from a closed set, not a free-text string"
        );
        assert_eq!(
            of(rdf::TYPE.as_str()),
            vec![&Term::NamedNode(NamedNode::new("urn:sparqlwatch:NotMeasured").unwrap())]
        );
        // Its endpoint is still a service, even in a run where nothing was
        // measured against it: a consumer joining dcat:DataService must not
        // lose the endpoint just because every one of its metrics was declined.
        assert!(qs.iter().any(|q| q.predicate.as_ref() == rdf::TYPE
            && q.object == Term::NamedNode(NamedNode::new("http://www.w3.org/ns/dcat#DataService").unwrap())));
        assert!(qs.iter().all(|q| q.graph_name
            == GraphName::NamedNode(NamedNode::new("urn:sparqlwatch:run:r1").unwrap())));
    }

    #[test]
    fn a_not_measured_fact_is_linked_to_the_run_that_declined_it() {
        // Every measurement carries `prov:wasGeneratedBy`; so must this, or a
        // consumer holding the fact can reach the policy that explains it (the
        // `urn:sparqlwatch:maxCost` on the activity) only by assuming the
        // `activity:{at}` naming convention. The fixture has no measurements at
        // all, which is the case that matters: in a run where every metric was
        // declined there is no measurement to borrow the activity from.
        let nm = vec![NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);
        let subj = NamedOrBlankNode::NamedNode(
            subject_iri(
                FactKind::NotMeasured,
                &RunId("r1".into()),
                "http://example.org/sparql",
                "classes",
            )
            .unwrap(),
        );
        let activity = NamedNode::new("urn:sparqlwatch:activity:r1").unwrap();
        assert_eq!(
            qs.iter()
                .filter(|q| q.subject == subj
                    && q.predicate.as_str() == "http://www.w3.org/ns/prov#wasGeneratedBy")
                .map(|q| &q.object)
                .collect::<Vec<_>>(),
            vec![&Term::NamedNode(activity.clone())],
            "the fact must name the run activity that declined the metric"
        );
        // ...and following that link reaches the policy, which is the whole
        // point of the link rather than of the naming convention.
        assert!(
            qs.iter().any(|q| q.subject == NamedOrBlankNode::NamedNode(activity.clone())
                && q.predicate.as_str() == "urn:sparqlwatch:maxCost"),
            "the activity the fact points at is the one carrying the ceiling"
        );
    }

    #[test]
    fn no_dqv_or_qb_predicate_ever_lands_on_a_not_measured_subject() {
        // Reusing a predicate whose declared domain is a class we are not is an
        // assertion, not a convenience. DQV declares `dqv:computedOn` with
        // domain `dqv:QualityMeasurement` and `dqv:isMeasurementOf` with domain
        // `qb:Observation`, so either one on a `NotMeasured` subject entails, by
        // `rdfs:domain` alone, that a quality measurement exists for a pair we
        // deliberately did not measure. Any consumer materialising domains then
        // sees a measurement missing its `dqv:value`, indistinguishable from
        // data we lost.
        //
        // Written as a scan over whole namespaces rather than as a check for the
        // two predicate names that once caused this, so a later addition to the
        // fact cannot quietly reintroduce it. A test that names today's mistake
        // cannot catch tomorrow's.
        const QB: &str = "http://purl.org/linked-data/cube#";
        // Measurements and not-measured facts in one document, so the scan runs
        // against a graph that genuinely contains `dqv:` predicates.
        let nm = vec![
            NotMeasured {
                endpoint: "http://example.org/sparql".into(),
                metric_id: "classes".into(),
                reason: NotMeasuredReason::CostCeiling,
            },
            NotMeasured {
                endpoint: "http://b.example/sparql".into(),
                metric_id: "classes".into(),
                reason: NotMeasuredReason::CostCeiling,
            },
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows(),
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);

        let declined: BTreeSet<String> = qs
            .iter()
            .filter(|q| {
                q.predicate.as_ref() == rdf::TYPE
                    && q.object
                        == Term::NamedNode(NamedNode::new("urn:sparqlwatch:NotMeasured").unwrap())
            })
            .map(|q| q.subject.to_string())
            .collect();
        assert_eq!(declined.len(), 2, "the fixture must actually contain declined facts, or this scan proves nothing");

        for q in &qs {
            if !declined.contains(&q.subject.to_string()) {
                continue;
            }
            let p = q.predicate.as_str();
            assert!(
                !p.starts_with(DQV) && !p.starts_with(QB),
                "a NotMeasured subject must carry no DQV or Data Cube predicate, \
                 whose domains would entail it is a measurement: {q}"
            );
        }

        // And the control: the measurement subjects in the very same document do
        // carry DQV predicates, so the loop above is scanning a real graph and
        // not passing because nothing in it uses DQV at all.
        assert!(
            qs.iter().any(|q| q.predicate.as_str().starts_with(DQV)
                && !declined.contains(&q.subject.to_string())),
            "measurements still use DQV; only the declined facts must not"
        );
    }

    #[test]
    fn several_not_measured_facts_each_get_their_own_subject() {
        let nm = vec![
            NotMeasured { endpoint: "http://a.example/sparql".into(), metric_id: "classes".into(), reason: NotMeasuredReason::CostCeiling },
            NotMeasured { endpoint: "http://b.example/sparql".into(), metric_id: "classes".into(), reason: NotMeasuredReason::CostCeiling },
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        }).unwrap();
        let qs = quads_of(&out);
        let subjects: BTreeSet<String> = qs
            .iter()
            .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:notMeasuredReason")
            .map(|q| q.subject.to_string())
            .collect();
        assert_eq!(subjects.len(), 2, "two declined (endpoint, metric) pairs are two facts");
    }

    #[test]
    fn a_not_measured_fact_with_an_invalid_endpoint_iri_is_skipped_not_fatal() {
        // Same doctrine as a measurement row: one junk URL out of 548 must not
        // cost the sweep its output.
        let nm = vec![
            NotMeasured { endpoint: "not an iri at all".into(), metric_id: "classes".into(), reason: NotMeasuredReason::CostCeiling },
            NotMeasured { endpoint: "http://b.example/sparql".into(), metric_id: "classes".into(), reason: NotMeasuredReason::CostCeiling },
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[],
        })
            .expect("one junk endpoint must not discard the sweep");
        assert!(!out.contains("not an iri at all"));
        let qs = quads_of(&out);
        assert_eq!(objects(&qs, "urn:sparqlwatch:notMeasuredReason").len(), 1,
                   "the one well-formed fact survives");
    }

    #[test]
    fn the_run_records_the_cost_ceiling_it_was_given() {
        // Without it, a consumer cannot tell a run that declined `classes`
        // from one that ran it: the not-measured facts say which metrics were
        // declined, and this says what policy declined them.
        for (ceiling, slug) in [(Cost::Cheap, "cheap"), (Cost::Expensive, "expensive")] {
            let out = emit_nquads(RunEmission {
                run: &RunId("r1".into()),
                generated_at: AT,
                metric_revision: REV,
                rows: &rows(),
                declarations_read: &[],
                not_measured: &[],
                max_cost: ceiling,
                concurrency: NonZeroUsize::new(1).unwrap(),
                failed_endpoints: 0,
                content_samples: &[],
            }).unwrap();
            let qs = quads_of(&out);
            assert_eq!(
                objects(&qs, "urn:sparqlwatch:maxCost"),
                vec![&Term::Literal(Literal::new_simple_literal(slug))],
                "the ceiling is a parameter of the emission, never read from anywhere global"
            );
            let activity = qs
                .iter()
                .find(|q| q.predicate.as_str() == "urn:sparqlwatch:maxCost")
                .map(|q| q.subject.to_string())
                .unwrap();
            assert_eq!(activity, "<urn:sparqlwatch:activity:r1>",
                       "the ceiling is a property of the run's activity, not of a measurement");
        }
    }

    /// Two more facts about the run itself, beside the ceiling above: how many
    /// hosts the sweep talked to at once, and how many endpoints it failed on.
    ///
    /// The first is what lets a consumer comparing `elapsedMs` across two runs
    /// tell a slower endpoint from a busier sweep. The second lets one tell a
    /// complete run from an incomplete one without reading a log, which is
    /// otherwise the only place the count appears.
    #[test]
    fn the_run_records_the_concurrency_it_ran_at_and_the_endpoints_it_failed() {
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows(),
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Cheap,
            content_samples: &[],
            concurrency: NonZeroUsize::new(4).unwrap(),
            failed_endpoints: 2,
        })
        .unwrap();
        let qs = quads_of(&out);
        let four = Term::Literal(Literal::new_typed_literal("4", xsd::INTEGER));
        let two = Term::Literal(Literal::new_typed_literal("2", xsd::INTEGER));
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:concurrency"),
            vec![&four],
            "the run states how many hosts it probed at once"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:failedEndpoints"),
            vec![&two],
            "the run states how many endpoints it failed on"
        );
        for p in ["urn:sparqlwatch:concurrency", "urn:sparqlwatch:failedEndpoints"] {
            let subject = qs
                .iter()
                .find(|q| q.predicate.as_str() == p)
                .map(|q| q.subject.to_string())
                .unwrap_or_else(|| panic!("no {p} quad was published at all"));
            assert_eq!(
                subject, "<urn:sparqlwatch:activity:r1>",
                "{p} is a property of the run's activity, not of a measurement"
            );
        }
    }

    /// A metric nobody measured because the prober failed on its endpoint is
    /// not a metric nobody measured because it costs too much. The two reasons
    /// are published as different slugs, so a consumer reading one run can tell
    /// an incomplete sweep from a deliberate decline.
    #[test]
    fn a_prober_failure_is_a_different_reason_from_a_cost_ceiling() {
        let nm = vec![
            NotMeasured {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "availability".into(),
                reason: NotMeasuredReason::ProberFailed,
            },
            NotMeasured {
                endpoint: "https://a.example/sparql".into(),
                metric_id: "classes".into(),
                reason: NotMeasuredReason::CostCeiling,
            },
        ];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            content_samples: &[],
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 1,
        })
        .unwrap();
        let qs = quads_of(&out);
        let mut reasons: Vec<String> = objects(&qs, "urn:sparqlwatch:notMeasuredReason")
            .iter()
            .map(|t| t.to_string())
            .collect();
        reasons.sort();
        assert_eq!(
            reasons,
            vec!["\"cost-ceiling\"".to_string(), "\"prober-failed\"".to_string()],
            "the two reasons must be distinguishable in the graph"
        );
    }

    fn sample(values: &[&str], truncated: bool) -> ContentSample {
        ContentSample {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            values: values.iter().map(|v| v.to_string()).collect(),
            truncated,
        }
    }

    #[test]
    fn a_content_sample_says_how_many_and_whether_it_hit_the_cap() {
        let s = ContentSample {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            values: vec!["http://example.org/A".into(), "http://example.org/B".into()],
            truncated: false,
        };
        let nq =
            emit_nquads(RunEmission {
                run: &RunId(AT.into()),
                generated_at: AT,
                metric_revision: REV,
                rows: &[],
                declarations_read: &[],
                not_measured: &[],
                max_cost: Cost::Cheap,
                concurrency: NonZeroUsize::new(1).unwrap(),
                failed_endpoints: 0,
                content_samples: &[s],
            }).unwrap();
        assert!(nq.contains("urn:sparqlwatch:ContentSample"));
        assert_eq!(nq.matches("urn:sparqlwatch:sampledValue").count(), 2);
        assert!(nq.contains(r#""2"^^<http://www.w3.org/2001/XMLSchema#integer>"#));
        assert!(nq.contains(r#""false"^^<http://www.w3.org/2001/XMLSchema#boolean>"#));
    }

    #[test]
    fn a_truncated_sample_publishes_that_fact_rather_than_leaving_it_inferred() {
        // The counterpart of the test above, and not redundant with it: a
        // consumer cannot recompute truncation from the size without knowing
        // the metric's `sample_limit`, which is not in this graph. Both
        // booleans must therefore be reachable, or an implementation that
        // hard-codes one is indistinguishable from a correct one.
        let out =
            emit_nquads(RunEmission {
                run: &RunId("r1".into()),
                generated_at: AT,
                metric_revision: REV,
                rows: &[],
                declarations_read: &[],
                not_measured: &[],
                max_cost: Cost::Expensive,
                concurrency: NonZeroUsize::new(1).unwrap(),
                failed_endpoints: 0,
                content_samples: &[sample(&["http://example.org/A"], true)],
            }).unwrap();
        let qs = quads_of(&out);
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampleTruncated"),
            vec![&Term::Literal(Literal::new_typed_literal("true", xsd::BOOLEAN))],
            "a sample that hit its cap must say so: a list a reader believes complete is worse than no list"
        );
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampleSize"),
            vec![&Term::Literal(Literal::new_typed_literal("1", xsd::INTEGER))]
        );
    }

    #[test]
    fn a_content_sample_uses_no_foreign_vocabulary() {
        // The not-measured fact reused dqv predicates whose domains entailed it
        // was a quality measurement, which was the one defect on this project
        // that no test could catch because nothing breaks until a consumer runs
        // inference. A sample is an observation from a bounded query, not a
        // dataset description, so it borrows nothing it has not earned:
        // `void:classPartition` and `void:class` both carry `rdfs:domain
        // void:Dataset`, and either one here would entail that this sample is a
        // dataset.
        let nq = emit_nquads(RunEmission {
            run: &RunId(AT.into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &rows(),
            declarations_read: &[],
            not_measured: &[NotMeasured {
                endpoint: "http://example.org/sparql".into(),
                metric_id: "classes".into(),
                reason: NotMeasuredReason::CostCeiling,
            }],
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[sample(&["http://example.org/A", "http://example.org/B"], false)],
        })
        .unwrap();
        let subjects: std::collections::HashSet<&str> = nq
            .lines()
            .filter(|l| l.contains("urn:sparqlwatch:ContentSample"))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(subjects.len(), 1, "the fixture must contain a sample, or this scan proves nothing");
        let mut seen = 0usize;
        for line in nq.lines() {
            let Some(s) = line.split_whitespace().next() else { continue };
            if !subjects.contains(s) {
                continue;
            }
            seen += 1;
            let p = line.split_whitespace().nth(1).unwrap_or("");
            assert!(
                p.starts_with("<urn:sparqlwatch:")
                    || p.contains("22-rdf-syntax-ns#type")
                    || p.contains("ns/prov#wasGeneratedBy"),
                "a sample carries only our own predicates, rdf:type and prov: {p}"
            );
        }
        assert_eq!(seen, 8, "type, sampledFrom, sampledBy, truncated, size, two values, wasGeneratedBy");
        // The control: the same document really does contain foreign
        // vocabulary, on the measurements, so the loop above is not passing
        // because nothing in the graph uses any.
        assert!(nq.lines().any(|l| l.contains("http://www.w3.org/ns/dqv#computedOn")));
    }

    #[test]
    fn a_sample_never_shares_a_subject_with_a_measurement_or_a_not_measured_fact() {
        // Three fact types in one graph. If any two share an IRI, a consumer
        // joining on it gets a node that is several things at once: a
        // measurement that both has and has not a verdict, or a sample that is
        // also a declined pair. Each set is collected by its own rdf:type
        // rather than by a substring of one IRI shape, because
        // `"measurement"` is not a substring of `not-measured:` and a
        // substring filter would silently see half the graph.
        let nm = vec![NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[row("http://example.org/sparql", "availability", Verdict::Verified)],
            declarations_read: &[],
            not_measured: &nm,
            max_cost: Cost::Cheap,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[sample(&["http://example.org/A"], false)],
        })
        .unwrap();
        let qs = quads_of(&out);
        let typed = |iri: &str| -> BTreeSet<String> {
            qs.iter()
                .filter(|q| {
                    q.predicate.as_ref() == rdf::TYPE
                        && q.object == Term::NamedNode(NamedNode::new(iri).unwrap())
                })
                .map(|q| q.subject.to_string())
                .collect()
        };
        let measurements = typed("http://www.w3.org/ns/dqv#QualityMeasurement");
        let declined = typed("urn:sparqlwatch:NotMeasured");
        let samples = typed("urn:sparqlwatch:ContentSample");
        assert_eq!(measurements.len(), 1, "the fixture has one measurement");
        assert_eq!(declined.len(), 1, "and one not-measured fact");
        assert_eq!(samples.len(), 1, "and one content sample");
        assert!(measurements.is_disjoint(&declined), "{measurements:?} vs {declined:?}");
        assert!(measurements.is_disjoint(&samples), "{measurements:?} vs {samples:?}");
        assert!(declined.is_disjoint(&samples), "{declined:?} vs {samples:?}");
    }

    #[test]
    fn a_sample_names_its_endpoint_its_metric_and_the_run_that_took_it() {
        // Read back as quads, and in the endpoint's order: the order is
        // evidence about the endpoint, so the fixture is deliberately not
        // alphabetical and an implementation that sorts must fail here.
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[sample(
                &["http://example.org/Zebra", "http://example.org/Apple", "http://example.org/Mango"],
                false,
            )],
        })
        .unwrap();
        let qs = quads_of(&out);
        let subj = NamedOrBlankNode::NamedNode(
            subject_iri(
                FactKind::ContentSample,
                &RunId("r1".into()),
                "http://example.org/sparql",
                "classes",
            )
            .unwrap(),
        );
        let of = |p: &str| -> Vec<&Term> {
            qs.iter()
                .filter(|q| q.subject == subj && q.predicate.as_str() == p)
                .map(|q| &q.object)
                .collect()
        };
        assert_eq!(
            of("urn:sparqlwatch:sampledFrom"),
            vec![&Term::NamedNode(NamedNode::new("http://example.org/sparql").unwrap())]
        );
        assert_eq!(
            of("urn:sparqlwatch:sampledBy"),
            vec![&Term::NamedNode(NamedNode::new("urn:sparqlwatch:metric:classes").unwrap())]
        );
        assert_eq!(
            of("urn:sparqlwatch:sampledValue"),
            vec![
                &Term::NamedNode(NamedNode::new("http://example.org/Zebra").unwrap()),
                &Term::NamedNode(NamedNode::new("http://example.org/Apple").unwrap()),
                &Term::NamedNode(NamedNode::new("http://example.org/Mango").unwrap()),
            ],
            "the IRIs the endpoint returned, in its order, not ours"
        );
        assert_eq!(
            of("http://www.w3.org/ns/prov#wasGeneratedBy"),
            vec![&Term::NamedNode(NamedNode::new("urn:sparqlwatch:activity:r1").unwrap())]
        );
        // Its endpoint is still a service: a consumer joining dcat:DataService
        // must not lose an endpoint that has a sample and nothing else.
        assert!(qs.iter().any(|q| q.predicate.as_ref() == rdf::TYPE
            && q.object == Term::NamedNode(NamedNode::new("http://www.w3.org/ns/dcat#DataService").unwrap())));
        assert!(qs.iter().all(|q| q.graph_name
            == GraphName::NamedNode(NamedNode::new("urn:sparqlwatch:run:r1").unwrap())));
    }

    #[test]
    fn several_samples_each_get_their_own_subject() {
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[
                sample(&["http://example.org/A"], false),
                ContentSample {
                    endpoint: "http://b.example/sparql".into(),
                    metric_id: "classes".into(),
                    values: vec!["http://example.org/B".into()],
                    truncated: true,
                },
            ],
        })
        .unwrap();
        let qs = quads_of(&out);
        let subjects: BTreeSet<String> = qs
            .iter()
            .filter(|q| q.predicate.as_str() == "urn:sparqlwatch:sampleSize")
            .map(|q| q.subject.to_string())
            .collect();
        assert_eq!(subjects.len(), 2, "two sampled (endpoint, metric) pairs are two samples");
    }

    #[test]
    fn a_conflicting_repeated_sample_publishes_nothing_about_that_pair() {
        // The same policy as a contradicted measurement, on the fact type
        // whose loop has the most control flow around the guard (the
        // `writable` pre-pass and its early `continue`), so a later edit to
        // that loop cannot quietly drop the guard. Publishing the first of two
        // disagreeing samples would assert that this endpoint's classes are
        // {A}, permanently, having discarded an observation that said {B}.
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[
                sample(&["https://a.example/A"], false),
                sample(&["https://a.example/B"], false),
            ],
        })
        .expect("a contradicted sample must not cost the sweep its output");
        let qs = quads_of(&out);
        assert!(
            objects(&qs, "urn:sparqlwatch:sampledValue").is_empty(),
            "published a sampled value for a contradicted pair: {out}"
        );
        assert!(
            objects(&qs, "urn:sparqlwatch:sampleSize").is_empty(),
            "published a sample size for a contradicted pair: {out}"
        );
        assert!(
            !qs.iter().any(|q| q.predicate.as_ref() == rdf::TYPE
                && q.object
                    == Term::NamedNode(NamedNode::new("urn:sparqlwatch:ContentSample").unwrap())),
            "published a sample node for a contradicted pair: {out}"
        );
    }

    #[test]
    fn a_sample_with_an_invalid_endpoint_or_value_iri_is_skipped_not_fatal() {
        // Same doctrine as a measurement row and a not-measured fact: this runs
        // after all the probing, so one junk string must not turn a whole sweep
        // into no output at all.
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[
                ContentSample {
                    endpoint: "not an iri at all".into(),
                    metric_id: "classes".into(),
                    values: vec!["http://example.org/A".into()],
                    truncated: false,
                },
                sample(&["http://example.org/A", "not an iri either"], false),
            ],
        })
        .expect("one junk string must not discard the sweep");
        assert!(!out.contains("not an iri"));
        let qs = quads_of(&out);
        assert_eq!(objects(&qs, "urn:sparqlwatch:sampleSize").len(), 1, "the well-formed sample survives");
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampledValue"),
            vec![&Term::NamedNode(NamedNode::new("http://example.org/A").unwrap())],
            "and its unwritable value is dropped rather than guessed at"
        );
        // The size counts what was published, not what was bound. A "2" here
        // beside one `sampledValue` quad would be two published answers to
        // "how many classes did we see", with nothing in the graph to explain
        // the gap. The dropped value is recorded in the log instead.
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampleSize"),
            vec![&Term::Literal(Literal::new_typed_literal("1", xsd::INTEGER))]
        );
    }

    #[test]
    fn a_sample_whose_values_are_all_unwritable_publishes_no_sample_node() {
        // The degenerate case the rule above creates: counting only what is
        // published means a sample of nothing but unwritable values would
        // otherwise publish `sampleSize 0`, which reads as "this endpoint has
        // no classes" when what happened is that we could not write down the
        // ones it named. Same doctrine as a probe that bound nothing.
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[sample(&["not an iri", "nor this one"], false)],
        })
        .unwrap();
        assert!(
            !out.contains("urn:sparqlwatch:sample"),
            "no sample node at all, so there is nothing to misread: {out}"
        );
    }

    #[test]
    fn sample_size_is_verifiable_against_the_values_beside_it() {
        // The invariant, rather than one instance of it: whatever the emitter
        // could not write, the published count and the published enumeration
        // agree, so a consumer can check one against the other.
        let out = emit_nquads(RunEmission {
            run: &RunId("r1".into()),
            generated_at: AT,
            metric_revision: REV,
            rows: &[],
            declarations_read: &[],
            not_measured: &[],
            max_cost: Cost::Expensive,
            concurrency: NonZeroUsize::new(1).unwrap(),
            failed_endpoints: 0,
            content_samples: &[sample(
                &["http://example.org/A", "not an iri", "http://example.org/B", "no space allowed"],
                false,
            )],
        })
        .unwrap();
        let qs = quads_of(&out);
        let values = objects(&qs, "urn:sparqlwatch:sampledValue").len();
        assert_eq!(values, 2, "two of the four values could be written");
        assert_eq!(
            objects(&qs, "urn:sparqlwatch:sampleSize"),
            vec![&Term::Literal(Literal::new_typed_literal(
                values.to_string(),
                xsd::INTEGER
            ))],
            "the size a consumer reads must be the number of values it can count"
        );
    }
}
