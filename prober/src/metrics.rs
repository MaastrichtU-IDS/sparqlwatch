use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The closed set of probe kinds. A metric definition names one of these plus
/// its parameters, which is what makes metrics data rather than code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    Liveness,
    /// An `access-control-allow-origin` header on a simple GET: what a `curl`
    /// user sees.
    Cors,
    /// An `OPTIONS` preflight for a cross-origin GET: what a browser sees.
    /// Deliberately separate from `Cors`, because an endpoint can genuinely
    /// have one and not the other and neither subsumes the other.
    CorsPreflight,
    /// A data-free filter, testing whether a function is bound.
    AskFilter,
    /// An ASK over data, testing presence. Uses the literal guard.
    AskData,
    SelectIris,
    /// Whether the endpoint DESCRIBES the vocabulary it actually uses.
    ///
    /// Sends nothing. It is graded from two things already gathered: the
    /// classes a `void:classPartition` named in the description, and the
    /// classes the profile pass actually found. That comparison is the
    /// declared-against-observed axis this whole project is about, applied to
    /// content, and it costs an endpoint no request at all.
    ///
    /// Distinct from `ClassProfile`, which publishes the profiles themselves
    /// and no verdict, because a profile is a description and there is no
    /// threshold at which "four properties" is a pass. Whether a publisher
    /// described what they hold IS a judgement, and it has one.
    VocabularyDescribed,
    /// One number the endpoint states about itself, counted and set against the
    /// statement.
    ///
    /// Sends ONE aggregate query. The declared value is the answer this service
    /// reports for "how big is this endpoint"; the count exists to say whether
    /// the statement is true, which is the only reason to spend a scan on it.
    ///
    /// `declared_by` names the VoID predicate that speaks for it, and unlike
    /// every other metric's `declared_by` it is read for a VALUE rather than
    /// for presence. `tolerance` is what keeps a description written last year
    /// from being called wrong because the dataset grew since.
    Counted,
    FetchWellKnown,
    /// Enumerate an endpoint's classes, then profile each one: which properties
    /// its instances carry and how many carry each.
    ///
    /// UNLIKE EVERY OTHER KIND, this produces no verdict and no measurement row.
    /// Its work fans out over classes discovered during the same sweep, so it is
    /// handled by `profile::profile_classes` after the per-metric dispatch rather
    /// than through it, and it publishes `ContentSample` plus `ContentProfile`
    /// facts instead. See Ruling 2 and Ruling 4 in
    /// docs/superpowers/specs/2026-08-29-content-profiles-design.md.
    ClassProfile,
}

impl ProbeKind {
    /// Every probe kind, for tests that must cover the whole closed set.
    ///
    /// Kept honest by `sequence` below rather than by discipline: a
    /// hand-maintained list in a test looks like it enforces coverage and does
    /// not, which is the defect shape this crate keeps finding in itself.
    pub const ALL: [ProbeKind; 10] = [
        ProbeKind::Liveness,
        ProbeKind::Cors,
        ProbeKind::CorsPreflight,
        ProbeKind::AskFilter,
        ProbeKind::AskData,
        ProbeKind::SelectIris,
        ProbeKind::FetchWellKnown,
        ProbeKind::ClassProfile,
        ProbeKind::VocabularyDescribed,
        ProbeKind::Counted,
    ];

    /// Whether a metric of this kind produces a measurement row, and so a
    /// verdict and a column in the matrix.
    ///
    /// `ClassProfile` is the one kind that does not: its pass publishes
    /// `ContentSample` and `ContentProfile` facts instead, per Ruling 2 in
    /// `docs/superpowers/specs/2026-08-29-content-profiles-design.md`. A row
    /// would give it a column of verdicts it does not have.
    ///
    /// Exhaustive with no catch-all, so a new kind cannot be added without
    /// deciding which side of this line it falls on. Tests count expected rows
    /// through this method rather than against `defs.len()`, so the two can
    /// never drift apart.
    pub fn yields_measurement(self) -> bool {
        match self {
            ProbeKind::Liveness
            | ProbeKind::Cors
            | ProbeKind::CorsPreflight
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris
            | ProbeKind::FetchWellKnown
            // Derived: it sends nothing, and grades the profile pass's own
            // results against the description already fetched. A measurement
            // all the same, and the only content verdict there is.
            | ProbeKind::VocabularyDescribed
            | ProbeKind::Counted => true,
            ProbeKind::ClassProfile => false,
        }
    }

    /// Whether the per-metric dispatch in `probe_endpoint` handles this kind.
    ///
    /// False for the two kinds handled elsewhere, and they are elsewhere for
    /// opposite reasons. `ClassProfile` runs its own pass AFTER the loop,
    /// because it fans out over classes discovered during the same sweep and
    /// its cost cannot be known before the list comes back.
    /// `VocabularyDescribed` sends nothing at all: it grades that pass's
    /// results against the description, so it can only be decided once the
    /// pass has finished.
    ///
    /// Not the same question as `yields_measurement`. A `ClassProfile` answers
    /// no to both; `VocabularyDescribed` answers no here and YES there, and
    /// conflating them either sends a request for a derived metric or drops its
    /// row on the floor. The two were one predicate until this kind existed.
    ///
    /// Exhaustive with no catch-all, so a new kind must state which it is.
    pub fn dispatched_per_metric(self) -> bool {
        match self {
            ProbeKind::Liveness
            | ProbeKind::Cors
            | ProbeKind::CorsPreflight
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris
            | ProbeKind::FetchWellKnown
            // Sends one aggregate query, so the ordinary dispatch handles it.
            | ProbeKind::Counted => true,
            ProbeKind::ClassProfile | ProbeKind::VocabularyDescribed => false,
        }
    }

    /// This kind's position in `ALL`. The match is exhaustive with no catch-all,
    /// so adding a variant fails to compile here; the new variant then gets the
    /// next index, and `all_is_every_variant_in_order` fails until it is added
    /// to `ALL`. That chain is what makes `ALL` complete by construction.
    ///
    /// Test-only: it exists to be checked, not called. CI runs both `cargo test`
    /// and `cargo clippy --all-targets`, so the compile-time half of the guard
    /// still fires there.
    #[cfg(test)]
    fn sequence(self) -> usize {
        match self {
            ProbeKind::Liveness => 0,
            ProbeKind::Cors => 1,
            ProbeKind::CorsPreflight => 2,
            ProbeKind::AskFilter => 3,
            ProbeKind::AskData => 4,
            ProbeKind::SelectIris => 5,
            ProbeKind::FetchWellKnown => 6,
            ProbeKind::ClassProfile => 7,
            ProbeKind::VocabularyDescribed => 8,
            ProbeKind::Counted => 9,
        }
    }

    /// Whether a probe is actually implemented for this kind. `FetchWellKnown`
    /// now has one too -- a single queryless fetch per endpoint, issued once
    /// in `probe_endpoint` ahead of this per-metric dispatch rather than
    /// through it -- so every current kind returns `true`. The mechanism
    /// stays for a future kind that has no probe yet: it must be skipped
    /// *without issuing a request*, since the generic query path would send
    /// `GET <endpoint>?query=`, a malformed protocol request that learns
    /// nothing and looks like abuse to the operator whose logs it lands in.
    /// Such a metric still gets a row, recorded as `Indeterminate`, so the
    /// gap stays visible in the published output.
    pub fn has_probe(&self) -> bool {
        match self {
            ProbeKind::Liveness
            | ProbeKind::Cors
            | ProbeKind::CorsPreflight
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris
            | ProbeKind::FetchWellKnown
            | ProbeKind::ClassProfile
            // Derived, and therefore always "implemented": it sends no request
            // at all, so there is no probe that could be missing.
            | ProbeKind::VocabularyDescribed
            | ProbeKind::Counted => true,
        }
    }
}

/// What a metric costs the endpoint we point it at. A closed set, like
/// `ProbeKind`: an unknown value is a load error, because guessing silently
/// changes what a sweep costs somebody else's server.
///
/// `Cheap` means the query can stop at its first match. `Expensive` means it
/// forces a scan. The line is not a guess: measured on qlever.dev's
/// planet-scale OSM endpoint, the same class query answers in 0.166s with
/// `LIMIT 1` and no `DISTINCT`, and times out past 45s with
/// `DISTINCT ... LIMIT 200`.
///
/// `Exhaustive` is a third thing, and the distinction it draws is NOT "an even
/// slower scan". It is one request PER THING DISCOVERED: a metric at this tier
/// issues a query per class it found, so its cost scales with the endpoint's
/// vocabulary rather than with its size. That difference is worth a tier
/// because it is worth a different SCHEDULE, which is the whole reason this
/// variant exists.
///
/// Measured on this project's own deployment on 2026-09-13, against
/// ontoexplorer: the nine metric probes and the three counts cost 12 requests,
/// while `class-profiles` alone cost 199 -- one per class, each paying the 2s
/// per-host politeness gap. 398 seconds of deliberate waiting, which was 75%
/// of a 9m15s sweep. Under one ceiling the only way to keep the counts hourly
/// was to pay for the profile hourly too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Cost {
    #[default]
    Cheap,
    Expensive,
    Exhaustive,
}

impl Cost {
    /// The published slug, also the value accepted on the command line. One
    /// spelling for the TOML field, the CLI flag and the emitted literal, so
    /// a run cannot record a ceiling under a name no flag can set.
    pub fn slug(&self) -> &'static str {
        match self {
            Cost::Cheap => "cheap",
            Cost::Expensive => "expensive",
            Cost::Exhaustive => "exhaustive",
        }
    }
}

/// How often a metric is asked.
///
/// A SECOND AXIS, and not a finer cost tier. `Cost` says what a metric costs
/// the endpoint it points at; this says how often that cost is worth paying.
/// The four hourly metrics are all `cheap`, and so are three of the six daily
/// ones, so no ceiling can separate them -- which is why this exists at all.
///
/// `Daily` is the default, and the direction of that default is deliberate: a
/// metric added to the file without a cadence is asked once a day rather than
/// every hour, so forgetting the field makes a sweep politer rather than
/// sixteen times louder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    Hourly,
    #[default]
    Daily,
}

impl Cadence {
    /// The published slug, also the value accepted on the command line. One
    /// spelling for the TOML field, the CLI flag and the emitted literal, for
    /// the reason `Cost::slug` gives.
    pub fn slug(&self) -> &'static str {
        match self {
            Cadence::Hourly => "hourly",
            Cadence::Daily => "daily",
        }
    }

    /// Whether a sweep on `self` carries a metric declared `declared`.
    ///
    /// The daily sweep carries EVERYTHING, hourly metrics included: a daily
    /// run that skipped them would leave its own graph with holes where the
    /// hourly sweeps have verdicts, and the endpoint page reports one run's
    /// facts. So `Daily` is a superset, not a disjoint half.
    pub fn carries(self, declared: Cadence) -> bool {
        match self {
            Cadence::Daily => true,
            Cadence::Hourly => declared == Cadence::Hourly,
        }
    }
}

/// The metric whose observed count is the profile gate's VOLUME signal.
///
/// Named here rather than spelled at the call site because `lib.rs` has to find
/// this row among the sweep's results, and a metric id typed into the prober is
/// a string that can stop matching `metrics.toml` without anything failing: the
/// gate would simply never see a count and profile every sweep, which is the
/// behaviour it exists to remove and looks exactly like working.
/// `the_gate_metrics_exist_in_the_shipped_definitions` is what keeps them real.
pub const TRIPLE_COUNT_METRIC: &str = "triple-count";
/// The profile gate's SHAPE signal. See `TRIPLE_COUNT_METRIC`.
pub const CLASS_COUNT_METRIC: &str = "class-count";
/// The pass the gate decides about, named for the same reason the two above
/// are: whether it RAN is what the backstop counts from.
pub const CLASS_PROFILES_METRIC: &str = "class-profiles";

/// `deny_unknown_fields`: an unrecognised key is a load error, never a key
/// serde quietly drops. Same doctrine as an unknown `kind` and an unknown
/// `cost`. A definition file that looks like it says something and does not is
/// the worst outcome here: `cost_class = "expensive"` loaded as `cheap` and
/// silently ran a planet-scale scan against every endpoint in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDef {
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub kind: ProbeKind,
    #[serde(default)]
    pub query: Option<String>,
    /// A simpler form of `query` to try when the endpoint REFUSES the first
    /// one with a 4xx. Absent for every metric that has nothing simpler to
    /// fall back to.
    ///
    /// This exists because the default/named-graph UNION that every content
    /// query needs is not universally supported. Three of the 63 YummyData
    /// endpoints answer `SELECT ?c WHERE { ?s a ?c } LIMIT 1` and reject the
    /// UNION form outright:
    ///
    ///   HTTP 400 "Not supported: Named Graphs (FROM, GRAPH) are currently
    ///   not supported"
    ///
    /// Before this, `geo-data` recorded `indeterminate` for all three plus
    /// Wikidata -- 4 of 58 reachable endpoints on that sweep. The reason was
    /// not "we could not tell whether there is geometry", it was "our query
    /// was refused for using a construct this store does not implement", and
    /// the non-UNION half would have answered. A false `indeterminate` of
    /// exactly the kind the six-verdict vocabulary exists to prevent.
    ///
    /// REMOVING THE UNION INSTEAD IS NOT THE FIX, and the measurement that
    /// says so is in metrics.toml: on ontoexplorer's content store the
    /// default-graph-only COUNT returns 0 where the UNION returns 12,510,532.
    /// A flat zero, published as a confident fact. Named graphs are the normal
    /// arrangement in Virtuoso, GraphDB and Blazegraph. So both forms are
    /// needed, and which one an endpoint can answer is a property of the
    /// endpoint.
    ///
    /// AN EMPTY FALLBACK RESULT IS AN HONEST `absent`. A store that rejects
    /// `GRAPH` as unsupported has no named graphs for data to hide in, so the
    /// default graph is the whole store and "not there" is the complete
    /// answer. That inference is the fallback's whole licence; a store that
    /// refused the first query for any other reason still answers the second
    /// one or stays `indeterminate`.
    ///
    /// IT NEEDS ROOM, and measured against sparql.dsmz.de/api/bacdive on
    /// 2026-09-18 it does not always have it:
    ///
    ///   geo-data alone, default gap  -> absent   (the fallback ran)
    ///   all six cheap metrics, 2000ms gap -> indeterminate
    ///   all six cheap metrics, 250ms gap  -> absent
    ///
    /// The second request takes the per-host gate again, so it queues behind
    /// every other metric's request at `--min-gap-ms` apiece, and on a sweep
    /// running six cheap metrics at the default 2s gap the metric budget can
    /// run out first. That is NOT a defect in this field and the verdict it
    /// produces is not wrong: `indeterminate` says no answer was established,
    /// which is exactly true when the fallback never got to ask. What it means
    /// is that the improvement lands where there is budget headroom, and a
    /// sweep of two cheap metrics has far more of it than one of six. Raising
    /// the gap or the metric count trades this fallback away, and that trade
    /// should be made knowingly rather than discovered.
    #[serde(default)]
    pub fallback_query: Option<String>,
    #[serde(default)]
    pub expect: Option<bool>,
    /// The SPARQL variable the query binds, for probe kinds that read
    /// bindings back out of the result (e.g. `AskData`, `SelectIris`).
    /// Absent for kinds that don't read bindings.
    #[serde(default)]
    pub var: Option<String>,
    /// The IRI whose presence in the endpoint's own declarations means it
    /// claims this capability. Absent for metrics no declaration can speak
    /// for (liveness, response time, CORS headers).
    #[serde(default)]
    pub declared_by: Option<String>,
    #[serde(default)]
    pub graded: bool,
    /// What this metric costs the endpoint it points at. Silent about cost
    /// means cheap, but an unrecognized value is a load error, not a guess.
    #[serde(default)]
    pub cost: Cost,
    /// How often this metric is asked. Silent means `daily`; see `Cadence` for
    /// why that is the safe direction for a default.
    #[serde(default)]
    pub cadence: Cadence,
    /// `Some(n)` means this metric enumerates: publish up to `n` bindings and
    /// say whether the cap was hit. `None` means it publishes no sample.
    /// Checked at load time against the `LIMIT` in the metric's own query, so
    /// the two places that must agree cannot drift in silence.
    #[serde(default)]
    pub sample_limit: Option<usize>,
    /// How a `ClassProfile` pass samples each class's instances: a SHA256 prefix
    /// on the subject IRI, or absent/empty for every instance.
    ///
    /// One hex character keeps about a sixteenth of the instances, two about a
    /// two-hundred-and-fifty-sixth. Measured 2026-08-29: at one character every
    /// property frequency landed within 0.002 of the true value over 200,000
    /// instances, while `LIMIT` without `ORDER BY` was wrong by 0.950 because it
    /// returned a contiguous block of the id space. There is deliberately no
    /// `LIMIT` option here: the prefix length is the only knob, because adding a
    /// limit reintroduces exactly that bias.
    ///
    /// Ignored by every other kind. NOT VALIDATED as hex here yet, which is a
    /// known gap: a prefix of "zz" would match nothing and the pass would report
    /// every class unreached, honestly but uselessly.
    #[serde(default)]
    pub sample_prefix: Option<String>,
    /// How far a declared count may be from the counted one and still be
    /// called right, as a fraction: `0.05` is five percent.
    ///
    /// `None` demands exact equality, which is almost never what anybody wants
    /// for a live dataset. A VoID file is written once and the data keeps
    /// growing, so exact matching would report `declared-but-wrong`, the
    /// harshest verdict in the vocabulary, for a dataset that gained a dozen
    /// triples since its description was published. That is a false accusation
    /// at registry scale.
    ///
    /// Read only by `ProbeKind::Counted`. A metric of any other kind carrying
    /// one is a load error, because a tolerance that silently does nothing
    /// reads as a threshold somebody set on purpose.
    #[serde(default)]
    pub tolerance: Option<f64>,
}

/// The query with everything that is not SPARQL *code* blanked out: comments
/// removed, and the contents of IRI references and string literals replaced by
/// spaces. Offsets and line structure are preserved so a position in the result
/// is a position in the original.
///
/// This exists for `query_limit` and for nothing else. The query actually sent
/// to an endpoint is never rewritten.
///
/// Two failures, both real, motivate the shape:
///
/// Until 2026-08-29 this truncated every line at its first `#`. That is also the
/// IRI fragment separator, so a query naming `<...owl#Class>` on the same line
/// as its `LIMIT` lost the `LIMIT` and was refused for declaring a
/// `sample_limit` its query did not carry, which is a false accusation: the
/// query carries it. Two exploratory metric sets had to put every `LIMIT` on its
/// own line to get around that.
///
/// Fixing only the comment handling then exposed the second failure, which the
/// old crudeness had been hiding by accident: `query_limit` looks for the
/// substring "limit", and `<http://example.org/vocab#limit200>` contains one. A
/// query with no `LIMIT` clause at all would have loaded and been sent to a
/// stranger's server unbounded. Blanking IRIs and literals is what closes that,
/// and it is why this function blanks rather than merely uncomments.
///
/// Not a SPARQL parser, and it does not need to be. It needs one property, and
/// keeps it: where the text is malformed it blanks to the end of the construct
/// rather than guessing, so a `LIMIT` inside something it could not close is not
/// found and the metric fails to load. Failing loudly is fine; passing something
/// that should be rejected is not.
fn sparql_code_only(query: &str) -> String {
    let chars: Vec<char> = query.chars().collect();
    let mut out = String::with_capacity(query.len());
    let mut i = 0;

    // Blank one char, keeping newlines so line structure survives.
    let blank = |out: &mut String, c: char| out.push(if c == '\n' { '\n' } else { ' ' });

    while i < chars.len() {
        match chars[i] {
            // A comment runs to the end of its line.
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    blank(&mut out, chars[i]);
                    i += 1;
                }
            }

            // An IRI reference. A SPARQL IRIREF may not contain whitespace, `<`
            // or `>`, so an unclosed `<` runs into one of those and cannot
            // swallow the rest of the query.
            '<' => {
                blank(&mut out, '<');
                i += 1;
                while i < chars.len()
                    && chars[i] != '>'
                    && chars[i] != '<'
                    && !chars[i].is_whitespace()
                {
                    blank(&mut out, chars[i]);
                    i += 1;
                }
                if i < chars.len() && chars[i] == '>' {
                    blank(&mut out, '>');
                    i += 1;
                }
            }

            // A string literal. The long forms may span lines; the short ones
            // may not, so a newline ends one and a stray quote cannot swallow
            // the rest of the query.
            q @ ('"' | '\'') => {
                let long = i + 2 < chars.len() && chars[i + 1] == q && chars[i + 2] == q;
                let open = if long { 3 } else { 1 };
                for _ in 0..open {
                    blank(&mut out, q);
                }
                i += open;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        blank(&mut out, '\\');
                        i += 1;
                        if i < chars.len() {
                            blank(&mut out, chars[i]);
                            i += 1;
                        }
                        continue;
                    }
                    if chars[i] == q {
                        if !long {
                            blank(&mut out, q);
                            i += 1;
                            break;
                        }
                        if i + 2 < chars.len() && chars[i + 1] == q && chars[i + 2] == q {
                            for _ in 0..3 {
                                blank(&mut out, q);
                            }
                            i += 3;
                            break;
                        }
                    }
                    if !long && chars[i] == '\n' {
                        break;
                    }
                    blank(&mut out, chars[i]);
                    i += 1;
                }
            }

            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Pull the integer of the last `LIMIT` clause in `query`, if any.
///
/// Deliberately narrow rather than crude: this reads our own hand-written
/// `metrics.toml`, not arbitrary SPARQL, and a wrong read here is a load error
/// rather than a wrong measurement, so a full parser would be the wrong amount
/// of machinery for the risk it removes. What it does insist on is that the
/// match be a `LIMIT` CLAUSE and not the letters l-i-m-i-t appearing somewhere:
/// the token must stand alone, and a count must follow it across whitespace.
/// `?limit200` is a variable and `ex:limit200` is a name; neither bounds a
/// query, and reading either as a bound would send an unbounded enumeration to
/// somebody else's server.
fn query_limit(query: &str) -> Option<u64> {
    let code = sparql_code_only(query);
    let lower = code.to_ascii_lowercase();
    let bytes = lower.as_bytes();

    // Last match wins, so scan candidates from the end.
    let mut found: Option<u64> = None;
    for (pos, _) in lower.match_indices("limit") {
        // The token must not continue a longer word on either side.
        let before_ok = pos == 0 || {
            let c = bytes[pos - 1] as char;
            !(c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        if !before_ok {
            continue;
        }
        let after = &code[pos + "limit".len()..];
        let trimmed = after.trim_start();
        // SPARQL puts whitespace between LIMIT and its count, and requiring it
        // is what rejects `limit200` as a name rather than a clause.
        if trimmed.len() == after.len() {
            continue;
        }
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        if let Ok(n) = digits.parse() {
            found = Some(n);
        }
    }
    found
}

/// Same reason as `MetricDef` above: a stray table at the top level (a second
/// `[[metrics]]` section next to the real `[[metric]]` ones, say) must not be
/// dropped in silence.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetricFile {
    metric: Vec<MetricDef>,
}

/// Parse metric definitions out of TOML. An unrecognized `kind` is a loud
/// parse error, never a silent default, because the probe-kind set is closed.
///
/// The same doctrine applies to a missing `var`: the two kinds that read a
/// variable's bindings back out of the result cannot work without knowing its
/// name, and guessing one silently turns a real capability into a false
/// `Absent`. That is a broken definition, so it fails here rather than
/// publishing one worthless measurement per endpoint.
///
/// An `id` outside `[a-z0-9][a-z0-9-]*` fails here as well, for a reason one
/// module over: the id is a field of every subject `emit::subject_iri` builds,
/// and that subject is split on `:`, so an id carrying one would leave the
/// endpoint field and the metric field indistinguishable in a published,
/// never-rewritten identifier.
///
/// A repeated `id` fails here too. It is not a situation to resolve at runtime:
/// the id is the metric's published identity, so two definitions sharing one can
/// land on opposite sides of the cost ceiling and give the same (endpoint,
/// metric) pair both a verdict and a not-measured fact, in one run graph. A
/// consumer joining on the metric IRI then reads a pair that both was and was
/// not measured.
///
/// Note the deliberate contrast with `registry::dedupe`, which drops a duplicate
/// endpoint with a warning instead of failing. A registry is seeded from
/// real-world dumps (LOD Cloud plus YummyData) that certainly contain the same
/// endpoint twice, and refusing to load would mean refusing to monitor anything;
/// dropping the repeat loses nothing, because the survivor says the same thing.
/// `metrics.toml` is written by hand, a repeated id says two different things
/// under one name, and there is no honest way to guess which was meant. So the
/// registry deduplicates and this refuses.
pub fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>> {
    let f: MetricFile = toml::from_str(toml_src)?;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for m in &f.metric {
        // The id is a field of every published subject, and `emit::subject_iri`
        // splits a subject on `:`, so an id holding one would make the endpoint
        // field and the metric field ambiguous. Refused here, where a definition
        // is judged, rather than one fact at a time at emission after the
        // probing is already paid for. `subject_iri` checks it again because its
        // injectivity depends on the invariant; see its doc comment.
        let mut chars = m.id.chars();
        let well_formed = match chars.next() {
            Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {
                chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
            _ => false,
        };
        if !well_formed {
            anyhow::bail!(
                "metric id '{}' is not usable as a published identifier; allowed is \
                 [a-z0-9][a-z0-9-]*, because the id is a field of every subject \
                 `emit::subject_iri` builds and that subject is split on ':'",
                m.id
            );
        }
        if !seen.insert(m.id.as_str()) {
            anyhow::bail!(
                "metric id '{}' is defined more than once; an id is a metric's published identity, \
                 so two definitions under one id would publish contradictory facts about the same pair",
                m.id
            );
        }
        if matches!(m.kind, ProbeKind::AskData | ProbeKind::SelectIris) && m.var.is_none() {
            anyhow::bail!(
                "metric '{}' of kind {:?} reads a variable's bindings but declares no `var`",
                m.id,
                m.kind
            );
        }
        if let Some(limit) = m.sample_limit {
            // `SelectIris` alone, deliberately narrower than "reads bindings".
            // `AskData` reads bindings too, but through `ask_literal`, which
            // collects the LEXICAL FORMS OF LITERALS, and `emit.rs` publishes
            // every sampled value through `NamedNode::new`. So an `AskData`
            // sample would drop most literals and republish any whose lexical
            // form happens to parse as an IRI as a resource the endpoint never
            // mentioned, losing the datatype either way. That is a wrong fact
            // about somebody's data, and a half-supported path is worse than a
            // closed one, so the path is closed here rather than at emission:
            // this is where a definition is judged, and nothing ships an
            // `AskData` sample today.
            //
            // To lift this, the sample must carry per value whether it is an
            // IRI or a literal (with its datatype), and the emitter must emit
            // accordingly. Until both exist, widening this check publishes
            // wrong term types.
            // WIDENED 2026-09-04 to admit `ClassProfile`, and the reason it is
            // safe is the reason the check exists. The rule being protected is
            // that every sampled value is published as an IRI, so a kind may
            // sample only if what it binds IS one. `ClassProfile` enumerates
            // classes through `select_iris` itself, with the same literal guard,
            // so its sample is the same shape `SelectIris` produces. `AskData`
            // is still refused, because `ask_literal` reads lexical forms of
            // literals and publishing those as IRIs is the wrong-term-type
            // defect this guard was built for.
            if !matches!(m.kind, ProbeKind::SelectIris | ProbeKind::ClassProfile) {
                anyhow::bail!(
                    "metric '{}' of kind {:?} declares a `sample_limit`, but only `SelectIris` and \
                     `ClassProfile` may sample: \
                     every sampled value is published as an IRI, so any other kind would publish the wrong term type",
                    m.id,
                    m.kind
                );
            }
            let query_limit = m.query.as_deref().and_then(query_limit);
            match query_limit {
                None => anyhow::bail!(
                    "metric '{}' declares sample_limit={} but its query has no `LIMIT`",
                    m.id,
                    limit
                ),
                Some(q) if q != limit as u64 => anyhow::bail!(
                    "metric '{}' declares sample_limit={} but its query's LIMIT is {}",
                    m.id,
                    limit,
                    q
                ),
                Some(_) => {}
            }
        }
    }
    Ok(f.metric)
}

/// Split `defs` into those to run against a sweep whose ceiling is
/// `ceiling`, and those declined because their cost exceeds it. Both halves
/// preserve the input order: a reordered result would make "declined"
/// harder to line up against the file that declared it.
/// Split `defs` into those a sweep on `cadence` asks, and those it does not.
///
/// Applied AFTER `within_cost`, so a metric declined for cost is never also
/// declined for cadence: `emit`'s duplicate-subject guard refuses two
/// `NotMeasured` facts for one (endpoint, metric) pair, and the reader is
/// owed the reason that was decided first. Both halves preserve input order,
/// for the reason `within_cost` gives.
pub fn within_cadence(defs: &[MetricDef], cadence: Cadence) -> (Vec<MetricDef>, Vec<MetricDef>) {
    let mut run = Vec::new();
    let mut declined = Vec::new();
    for d in defs {
        if cadence.carries(d.cadence) {
            run.push(d.clone());
        } else {
            declined.push(d.clone());
        }
    }
    (run, declined)
}

pub fn within_cost(defs: &[MetricDef], ceiling: Cost) -> (Vec<MetricDef>, Vec<MetricDef>) {
    let mut run = Vec::new();
    let mut declined = Vec::new();
    for d in defs {
        // The tiers are ordered, and each ceiling admits everything at or
        // below it. Spelled out per ceiling rather than derived from a
        // PartialOrd, so that adding a tier fails to compile here -- which is
        // the one place that decides what a ceiling means -- instead of
        // quietly inheriting an ordering nobody chose.
        let within = match ceiling {
            Cost::Cheap => d.cost == Cost::Cheap,
            Cost::Expensive => d.cost != Cost::Exhaustive,
            Cost::Exhaustive => true,
        };
        if within {
            run.push(d.clone());
        } else {
            declined.push(d.clone());
        }
    }
    (run, declined)
}

/// A stable identifier for a set of metric definitions, so a run can record
/// which revision produced its measurements. Deliberately a pure function of
/// the definitions themselves -- no clock, no counter, no build metadata -- so
/// re-running the same definitions yields the same revision, and any edit to
/// any field yields a different one.
///
/// FNV-1a over a canonical rendering, rather than `DefaultHasher`, whose
/// output Rust explicitly does not promise to keep stable across releases.
/// This value is published, so it has to outlive the toolchain.
pub fn definitions_revision(defs: &[MetricDef]) -> String {
    let mut canonical = String::new();
    for d in defs {
        // Order and field set are part of the revision: a reordered file is a
        // different definition list, and every field affects what is measured.
        //
        // Destructured rather than field-accessed on purpose. A struct pattern
        // with no `..` fails to compile the moment `MetricDef` gains a field, so
        // a new field cannot join the definitions without somebody deciding here
        // whether it belongs in the revision. The alternative, ten field
        // accesses, lets a new field be forgotten in silence, and the cost of
        // forgetting is not a failing test: `metricDefinitionRevision` is a
        // published literal in immutable per-run graphs, so two definition sets
        // that measure different things would share one revision forever, with
        // no way to reinterpret the history afterwards.
        let MetricDef {
            id,
            label,
            dimension,
            kind,
            query,
            // IN THE REVISION. An endpoint that refuses the primary query and
            // answers the fallback publishes a verdict where it published
            // `indeterminate` before, so two definition sets differing only in
            // this measure different things and must not share a revision.
            fallback_query,
            expect,
            var,
            declared_by,
            graded,
            cost,
            // IN THE REVISION. Two definition sets differing only in a
            // metric's cadence measure different things on the same sweep:
            // one publishes a verdict where the other publishes
            // `not measured (cadence)`.
            cadence,
            sample_limit,
            // IN THE REVISION, deliberately. The prefix decides whether a
            // profile is exact or drawn from a sixteenth of the instances, so
            // two definition sets differing only in it measure different things.
            // Sharing one revision forever would be exactly the failure the
            // comment above describes.
            sample_prefix,
            // IN THE REVISION, for the same reason. The tolerance decides
            // where `verified` stops and `declared-but-wrong` begins, so two
            // definition sets differing only in it grade the same observation
            // differently and must not share a revision.
            tolerance,
        } = d;
        canonical.push_str(&format!(
            "{}\x1f{}\x1f{}\x1f{:?}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{:?}\x1f{:?}\x1f{}\x1f{}\x1f{}\x1e",
            id,
            label,
            dimension,
            kind,
            query.as_deref().unwrap_or(""),
            fallback_query.as_deref().unwrap_or(""),
            expect.map(|b| b.to_string()).unwrap_or_default(),
            var.as_deref().unwrap_or(""),
            declared_by.as_deref().unwrap_or(""),
            graded,
            cost,
            cadence,
            sample_limit.map(|n| n.to_string()).unwrap_or_default(),
            sample_prefix.as_deref().unwrap_or(""),
            // Formatted rather than Debug-printed so that 0.05 and 5e-2 hash
            // the same: they are the same threshold and a reader who wrote
            // either meant the same thing.
            tolerance.map(|t| format!("{t:.6}")).unwrap_or_default(),
        ));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
[[metric]]
id = "cors"
label = "CORS headers"
dimension = "interoperability"
kind = "Cors"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"

[[metric]]
id = "geo-functions"
label = "GeoSPARQL relation functions"
dimension = "capability"
kind = "AskFilter"
expect = true
query = "ASK { }"
"#;

    #[test]
    fn loads_definitions_from_toml() {
        let ms = load_metrics(SRC).unwrap();
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].id, "cors");
        assert_eq!(ms[0].kind, ProbeKind::Cors);
        assert_eq!(ms[1].expect, Some(true));
        assert!(!ms[1].graded);
    }

    #[test]
    fn the_shipped_metrics_file_parses() {
        // Guards against a typo in metrics.toml reaching a run.
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        assert!(ms.iter().any(|m| m.id == "geo-data"));
        assert!(ms.iter().find(|m| m.id == "service-description").unwrap().graded);
        // The two CORS facts are both shipped, and they are different kinds.
        // A `cors-preflight` metric accidentally defined as `kind = "Cors"`
        // would publish the simple-GET header under the preflight's label,
        // which is the exact mislabelling this metric exists to end.
        assert_eq!(ms.iter().find(|m| m.id == "cors").unwrap().kind, ProbeKind::Cors);
        let preflight = ms.iter().find(|m| m.id == "cors-preflight").expect("cors-preflight must be shipped");
        assert_eq!(preflight.kind, ProbeKind::CorsPreflight);
        // A preflight carries no query, and no declaration speaks for a CORS
        // policy, so neither field may be set on it.
        assert_eq!(preflight.query, None);
        assert_eq!(preflight.declared_by, None);
    }

    #[test]
    fn every_probe_kind_has_a_probe() {
        // FetchWellKnown's probe is the once-per-endpoint fetch in
        // `probe_endpoint`, dispatched ahead of this generic per-metric path
        // rather than through it, but it is implemented now: no kind in the
        // closed set currently lacks one.
        for k in ProbeKind::ALL {
            assert!(k.has_probe(), "{k:?} should have a probe");
        }
    }

    #[test]
    fn all_is_every_variant_in_order() {
        // The guard that makes `ProbeKind::ALL` trustworthy. Adding a variant
        // breaks `sequence`'s exhaustive match at compile time, and then this
        // fails until the variant is in `ALL` at its own index. Without it,
        // `ALL` is just another hand-maintained list that a new kind can slip
        // past, and every test iterating it would silently stop covering the
        // set it claims to cover.
        for (i, k) in ProbeKind::ALL.into_iter().enumerate() {
            assert_eq!(k.sequence(), i, "{k:?} is at the wrong index in ALL");
        }
    }

    #[test]
    fn an_unknown_probe_kind_is_rejected_loudly() {
        let bad = r#"
[[metric]]
id = "x"
label = "x"
dimension = "d"
kind = "Telepathy"
"#;
        assert!(load_metrics(bad).is_err());
    }

    #[test]
    fn a_binding_kind_with_no_var_is_a_load_error_not_a_silent_measurement() {
        // Same doctrine as the unknown `kind` above: a broken definition is a
        // loud parse error. Papering over it published N endpoints x one
        // Indeterminate measurement for a metric that could never work.
        for kind in ["AskData", "SelectIris"] {
            let bad = format!(
                r#"
[[metric]]
id = "x"
label = "x"
dimension = "d"
kind = "{kind}"
query = "SELECT ?thing WHERE {{ ?s ?p ?thing }} LIMIT 1"
"#
            );
            let err = load_metrics(&bad).expect_err("{kind} with no var must be rejected");
            let msg = err.to_string();
            assert!(msg.contains("var"), "the error must name the missing field: {msg}");
            assert!(msg.contains('x'), "the error must name the metric: {msg}");
        }
    }

    #[test]
    fn the_revision_is_a_pure_function_of_the_definitions() {
        // The revision exists so a published measurement can be read against the
        // definition that produced it, and it is a published literal inside
        // immutable per-run graphs. A field that stops contributing therefore
        // cannot be fixed later: two definition sets that measure different
        // things would share one revision in history that is already out. So
        // every field gets a guard, not just the three that happened to have one.
        //
        // Written as a loop over per-field variants, and the variants are built
        // from a destructured base on purpose. A struct pattern with no `..`
        // fails to compile when `MetricDef` gains a field, and every binding
        // below is used exactly once to build a variant, so a field that is
        // named but left uncovered is an unused-variable warning. CI runs
        // `cargo clippy --all-targets -- -D warnings`, so that warning is an
        // error there: the same completeness-by-construction chain
        // `ProbeKind::ALL` uses, rather than a hand-maintained list of
        // assertions that a new field can slip past.
        let base = load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
        )
        .unwrap();
        let rev = definitions_revision(&base);
        let d = base[0].clone();
        let MetricDef {
            id,
            label,
            dimension,
            kind,
            query,
            fallback_query,
            expect,
            var,
            declared_by,
            graded,
            cost,
            cadence,
            sample_limit,
            sample_prefix,
            tolerance,
        } = d.clone();

        let variants: Vec<(&str, MetricDef)> = vec![
            ("id", MetricDef { id: format!("{id}-renamed"), ..d.clone() }),
            ("label", MetricDef { label: format!("{label} (reworded)"), ..d.clone() }),
            ("dimension", MetricDef { dimension: format!("{dimension}-other"), ..d.clone() }),
            // The field this branch just proved publishes a confident false
            // `absent` when it drifts: `SelectIris` reads IRI bindings where
            // `AskData` reads a boolean.
            (
                "kind",
                MetricDef {
                    kind: if kind == ProbeKind::Liveness { ProbeKind::Cors } else { ProbeKind::Liveness },
                    ..d.clone()
                },
            ),
            (
                "query",
                MetricDef {
                    query: Some(format!("{} # edited", query.as_deref().unwrap_or(""))),
                    ..d.clone()
                },
            ),
            // A definition that gains a fallback measures something different:
            // the endpoint that refused the primary query now publishes a
            // verdict where it published `indeterminate`. `None` in the base,
            // so the variant is the one that HAS it.
            (
                "fallback_query",
                MetricDef {
                    fallback_query: Some(
                        fallback_query.unwrap_or_else(|| "ASK{} # simpler".into()),
                    ),
                    ..d.clone()
                },
            ),
            // A metric moved between cadences is asked on different sweeps:
            // one definition set publishes a verdict where the other publishes
            // `not measured (cadence)`.
            (
                "cadence",
                MetricDef {
                    cadence: if cadence == Cadence::Hourly { Cadence::Daily } else { Cadence::Hourly },
                    ..d.clone()
                },
            ),
            ("expect", MetricDef { expect: Some(!expect.unwrap_or(false)), ..d.clone() }),
            // The other field whose drift publishes a false `absent`: a probe
            // that reads the wrong variable's bindings finds nothing.
            (
                "var",
                MetricDef {
                    var: Some(var.as_deref().map(|v| format!("{v}2")).unwrap_or_else(|| "c".into())),
                    ..d.clone()
                },
            ),
            (
                "declared_by",
                MetricDef {
                    declared_by: Some(
                        declared_by.as_deref().unwrap_or("http://example.org/fn").to_string(),
                    ),
                    ..d.clone()
                },
            ),
            ("graded", MetricDef { graded: !graded, ..d.clone() }),
            (
                "cost",
                MetricDef {
                    cost: match cost {
                        Cost::Cheap => Cost::Expensive,
                        Cost::Expensive => Cost::Exhaustive,
                        Cost::Exhaustive => Cost::Cheap,
                    },
                    ..d.clone()
                },
            ),
            // It changes what we publish, so it changes the revision. Bypasses
            // `load_metrics`'s cross-check on purpose: this constructs a
            // `MetricDef` directly, and the check belongs to the loader, not
            // to the struct.
            // A profile drawn from a sixteenth of the instances is not the same
            // measurement as an exact one, so the prefix has to move the
            // revision. Without this variant the field could join the
            // definitions and two sets that sample differently would share one
            // revision forever.
            (
                "sample_prefix",
                MetricDef {
                    sample_prefix: Some(match sample_prefix.as_deref() {
                        Some("0") => "00".to_string(),
                        _ => "0".to_string(),
                    }),
                    ..d.clone()
                },
            ),
            // It decides where `verified` stops and `declared-but-wrong` begins,
            // so two sets differing only in it grade the same count
            // differently. Sharing a revision would make the history of a
            // count metric uninterpretable.
            (
                "tolerance",
                MetricDef {
                    tolerance: Some(tolerance.unwrap_or(0.0) + 0.01),
                    ..d.clone()
                },
            ),
            (
                "sample_limit",
                MetricDef {
                    sample_limit: Some(sample_limit.unwrap_or(0) + 1),
                    ..d.clone()
                },
            ),
        ];

        for (field, variant) in &variants {
            assert_ne!(
                rev,
                definitions_revision(std::slice::from_ref(variant)),
                "editing `{field}` changes what is measured or where, so it must be a new revision"
            );
        }
        // Each variant differs from the base in exactly one field, so no two
        // variants may share a revision either: that would mean two fields land
        // in the same place in the canonical string.
        let revisions: std::collections::BTreeSet<String> =
            variants.iter().map(|(_, v)| definitions_revision(std::slice::from_ref(v))).collect();
        assert_eq!(
            revisions.len(),
            variants.len(),
            "two single-field edits collided, so some field is not in its own position"
        );

        // The rest of the contract, which is not per-field: same definitions in,
        // same revision out (no clock, no counter, no build metadata), a
        // reordered file is a different definition list, and the value names the
        // algorithm so a future one can be told apart from this one.
        let a = load_metrics(SRC).unwrap();
        assert_eq!(definitions_revision(&a), definitions_revision(&a), "no clock, no randomness");
        let reordered: Vec<MetricDef> = a.iter().rev().cloned().collect();
        assert_ne!(
            definitions_revision(&a),
            definitions_revision(&reordered),
            "a reordered file is a different definition list"
        );
        assert!(definitions_revision(&a).starts_with("fnv1a64:"));
    }

    #[test]
    fn a_mistyped_key_is_a_load_error_not_a_silently_dropped_field() {
        // Serde drops unknown fields by default, which turns a spelling mistake
        // into a definition that looks like it says something and does not.
        // `cost_class = "expensive"` used to load as `cheap`, so the typo did not
        // merely lose the field: it silently ran a planet-scale scan against
        // every endpoint in the registry, which is the exact harm the closed
        // `Cost` set exists to prevent.
        for (typo, key) in [
            ("cost_class = \"expensive\"", "cost_class"),
            ("declaredBy = \"http://example.org/fn\"", "declaredBy"),
        ] {
            let bad = format!(
                "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{{}}\"\n{typo}\n"
            );
            let err = load_metrics(&bad)
                .err()
                .unwrap_or_else(|| panic!("`{key}` is not a field of MetricDef and must be refused"));
            assert!(
                err.to_string().contains(key),
                "the error must name the key that was not understood: {err}"
            );
        }
    }

    #[test]
    fn a_duplicate_metric_id_is_a_load_error_not_a_contradiction_in_the_graph() {
        // A repeated id lands the same metric in both halves of the cost split:
        // once in `run`, once in `declined`. Emission then publishes, for one
        // endpoint in one run graph, a measurement with a verdict AND a
        // not-measured fact for `urn:sparqlwatch:metric:classes`, so a consumer
        // joining on the metric IRI sees a pair that both was and was not
        // measured. Refuse the file instead of guessing which definition was
        // meant.
        //
        // Contrast `registry::dedupe`, which warns and drops. That list comes
        // from real-world dumps that contain duplicates by nature and whose
        // repeats say the same thing; this file is written by hand and its
        // repeats say different things.
        let dup = concat!(
            "[[metric]]\nid=\"classes\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
            "[[metric]]\nid=\"classes\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n",
        );
        let err = load_metrics(dup).expect_err("a repeated metric id must be refused");
        let msg = err.to_string();
        assert!(msg.contains("classes"), "the error must name the duplicate: {msg}");

        // Non-adjacent repeats too: the check is over the whole file, not over
        // neighbouring pairs.
        let far = concat!(
            "[[metric]]\nid=\"a\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
            "[[metric]]\nid=\"b\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
            "[[metric]]\nid=\"a\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
        );
        assert!(load_metrics(far).is_err(), "a repeat anywhere in the file is a repeat");

        // And the shipped file is not accidentally in breach.
        assert!(load_metrics(include_str!("../metrics.toml")).is_ok());
    }

    #[test]
    fn a_metric_id_with_a_colon_is_a_load_error() {
        // `emit::subject_iri` right-splits a subject on `:`, so a colon in an
        // id would make the endpoint field and the metric field ambiguous. The
        // control below is the same definition with a legal id, so a
        // missing-field rejection cannot satisfy this test.
        let ok = "[[metric]]\nid=\"has-classes\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n";
        assert!(load_metrics(ok).is_ok(), "the control definition must load");
        let bad = ok.replace("has-classes", "has:classes");
        let err = load_metrics(&bad).expect_err("a colon in an id must be refused");
        let msg = err.to_string();
        assert!(msg.contains("has:classes"), "did not name the offending id: {msg}");
        assert!(msg.contains("a-z"), "did not say what is allowed: {msg}");
    }

    #[test]
    fn a_stray_top_level_table_is_a_load_error_too() {
        // Same doctrine one level up: `[[metrics]]` alongside the real
        // `[[metric]]` tables would otherwise be dropped, and the reader would
        // never learn that half the file was ignored.
        let bad = concat!(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
            "[[metrics]]\nid=\"n\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n",
        );
        assert!(load_metrics(bad).is_err(), "a stray top-level table must not be ignored");
    }

    #[test]
    fn a_kind_that_reads_no_bindings_needs_no_var() {
        let ok = r#"
[[metric]]
id = "availability"
label = "answers a trivial query"
dimension = "availability"
kind = "Liveness"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"
"#;
        assert!(load_metrics(ok).is_ok());
    }

    #[test]
    fn the_var_field_round_trips_and_defaults_to_none() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo_data = ms.iter().find(|m| m.id == "geo-data").unwrap();
        assert_eq!(geo_data.var, Some("g".to_string()));
        let cors = ms.iter().find(|m| m.id == "cors").unwrap();
        assert_eq!(cors.var, None);
    }

    #[test]
    fn a_metric_can_name_the_declaration_that_would_satisfy_it() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo = ms.iter().find(|m| m.id == "geo-functions").unwrap();
        assert_eq!(
            geo.declared_by.as_deref(),
            Some("http://www.opengis.net/def/function/geosparql/sfWithin")
        );
        // Most metrics have no declaration that could speak for them.
        assert!(ms.iter().find(|m| m.id == "availability").unwrap().declared_by.is_none());
    }

    #[test]
    fn a_metric_without_a_cost_is_cheap() {
        let defs = load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n"
        ).unwrap();
        assert_eq!(defs[0].cost, Cost::Cheap, "a definition silent about cost is cheap");
    }

    #[test]
    fn an_unknown_cost_is_a_load_error_not_a_silent_default() {
        // Same doctrine as an unknown `kind`: the set is closed, and guessing
        // silently changes what a sweep costs somebody else's server.
        assert!(load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"free\"\n"
        ).is_err());
    }

    #[test]
    fn within_cost_splits_and_keeps_order() {
        let defs = load_metrics(concat!(
            "[[metric]]\nid=\"a\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
            "[[metric]]\nid=\"b\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n",
            "[[metric]]\nid=\"c\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
        )).unwrap();

        let (run, declined) = within_cost(&defs, Cost::Cheap);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(declined.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["b"]);

        let (run, declined) = within_cost(&defs, Cost::Expensive);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"],
                   "the higher ceiling runs everything, still in file order");
        assert!(declined.is_empty());
    }

    #[test]
    fn the_exhaustive_tier_is_what_expensive_does_not_reach() {
        // The tier exists so a sweep can take the counts without taking the
        // class profile, which costs one request per class rather than one
        // request. A ceiling of `expensive` that dragged the profile along
        // would leave the split unexpressible and the tier pointless, so the
        // gap between the two ceilings is the thing asserted here.
        let defs = load_metrics(concat!(
            "[[metric]]\nid=\"cheap\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
            "[[metric]]\nid=\"scan\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n",
            "[[metric]]\nid=\"fanout\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"exhaustive\"\n",
        )).unwrap();

        let (run, declined) = within_cost(&defs, Cost::Cheap);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["cheap"]);
        assert_eq!(declined.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["scan", "fanout"]);

        let (run, declined) = within_cost(&defs, Cost::Expensive);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["cheap", "scan"],
                   "expensive takes the scan and stops short of the fan-out");
        assert_eq!(declined.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["fanout"]);

        let (run, declined) = within_cost(&defs, Cost::Exhaustive);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["cheap", "scan", "fanout"],
                   "the top ceiling runs everything, still in file order");
        assert!(declined.is_empty());
    }

    #[test]
    fn the_shipped_profile_pass_and_its_grader_decline_together() {
        // `vocabulary-described` grades what `class-profiles` found. A ceiling
        // that ran the grader without the pass would publish a verdict about
        // an endpoint nobody asked -- `indeterminate`, hourly, forever. They
        // are tiered together so they decline together, and this is the test
        // that says so about the SHIPPED definitions rather than about a
        // fixture.
        let defs = crate::metrics::load_metrics(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("metrics.toml"),
            )
            .expect("the shipped metrics.toml"),
        )
        .expect("the shipped metrics.toml parses");

        let (run, declined) = within_cost(&defs, Cost::Expensive);
        let ran: Vec<&str> = run.iter().map(|d| d.id.as_str()).collect();
        let out: Vec<&str> = declined.iter().map(|d| d.id.as_str()).collect();
        for id in ["class-profiles", "vocabulary-described"] {
            assert!(out.contains(&id), "{id} must be declined below the exhaustive ceiling");
            assert!(!ran.contains(&id), "{id} must not run below the exhaustive ceiling");
        }
        // The counts are the reason the middle tier exists at all: they are the
        // expensive thing an hourly sweep still wants.
        for id in ["triple-count", "graph-count", "class-count"] {
            assert!(ran.contains(&id), "{id} is what `expensive` is for");
        }
    }

    #[test]
    fn cost_is_part_of_the_definitions_revision() {
        // The revision exists so a measurement can be read against the definition
        // that produced it, and cost changes which metrics run at all.
        let one = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n";
        let two = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n";
        assert_ne!(
            definitions_revision(&load_metrics(one).unwrap()),
            definitions_revision(&load_metrics(two).unwrap())
        );
    }

    #[test]
    fn a_cost_slug_is_the_spelling_the_toml_and_the_cli_both_use() {
        // The slug is published on the run's activity, so it has to be the same
        // token `cost = "..."` accepts and the same one `--max-cost` accepts.
        // Three spellings of one ceiling would make the published value
        // unjoinable against the definitions that produced it.
        for c in [Cost::Cheap, Cost::Expensive, Cost::Exhaustive] {
            let src = format!(
                "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{{}}\"\ncost=\"{}\"\n",
                c.slug()
            );
            assert_eq!(load_metrics(&src).unwrap()[0].cost, c, "TOML must accept {:?}", c.slug());
            assert_eq!(
                clap::ValueEnum::to_possible_value(&c).unwrap().get_name(),
                c.slug(),
                "the CLI must accept the same token"
            );
        }
    }

    #[test]
    fn a_metric_without_a_sample_limit_publishes_no_sample() {
        let defs = load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n"
        ).unwrap();
        assert_eq!(defs[0].sample_limit, None);
    }

    #[test]
    fn a_sample_limit_must_match_the_querys_limit() {
        // The id is deliberately distinctive. An earlier version used id = "m"
        // and asserted err.contains("m"), which is satisfied by any message
        // containing the letter m, the word "metric" included: it could not
        // fail, while its failure message claimed to check that the error names
        // the metric.
        // Two places that must agree will drift. The loader is where that is caught,
        // and a mismatch is a broken definition file, not something to guess about.
        let src = |lim: &str, q_lim: &str| format!(
            "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit={lim}\nquery=\"SELECT DISTINCT ?c WHERE {{ ?s a ?c }} LIMIT {q_lim}\"\n"
        );
        assert!(load_metrics(&src("200", "200")).is_ok());
        let err = load_metrics(&src("200", "50")).unwrap_err().to_string();
        assert!(err.contains("zebra-sample"), "the error must name the metric: {err}");
        assert!(err.contains("200") && err.contains("50"), "and both numbers: {err}");
    }

    #[test]
    fn ask_data_may_not_declare_a_sample_limit_yet() {
        // AskData does read bindings, but `ask_literal` reads the lexical forms of
        // LITERALS, and `emit.rs` publishes every sampled value as an IRI. So an
        // AskData sample would drop most literals and republish any whose lexical
        // form parses as an IRI as a resource the endpoint never mentioned:
        // "http://example.org/NotActuallyAnIri" as a plain string comes back out of
        // the graph as `<http://example.org/NotActuallyAnIri>`, and the datatype is
        // lost either way. That is a wrong fact about somebody's data, so the path
        // is closed rather than half-supported. Nothing ships an AskData sample
        // today, so nothing is lost by closing it.
        //
        // To lift this, `ContentSample` must carry per value whether it is an IRI
        // or a literal (with its datatype) and the emitter must emit accordingly.
        // Then this test inverts back, and a geometry sample becomes possible.
        let src = "[[metric]]\nid=\"wkt-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"AskData\"\nvar=\"g\"\n\
                   sample_limit=25\nquery=\"SELECT ?g WHERE { ?s ?p ?g } LIMIT 25\"\n";
        let err = load_metrics(src).unwrap_err().to_string();
        assert!(err.contains("wkt-sample"), "the error must name the metric: {err}");
        assert!(err.contains("SelectIris"), "and say which kind may sample: {err}");
    }

    #[test]
    fn a_comment_cannot_stand_in_for_the_querys_real_limit() {
        // The reviewer's input, verbatim in shape: `metrics.toml` uses
        // triple-quoted multi-line query blocks, so a SPARQL comment inside one is
        // plausible, and `rfind("limit")` landed in the comment. The query is
        // bounded at 50, truncation is then computed as `50 >= 200` = false, and a
        // list truncated at 50 is published as COMPLETE: the single failure this
        // cross-check exists to prevent.
        let commented = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"\"\"\nSELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 50\n\
             # lowered from 200 after the qlever timeout; raise back to limit 200 when budgets allow\n\"\"\"\n";
        let err = load_metrics(commented).unwrap_err().to_string();
        assert!(err.contains("zebra-sample"), "the error must name the metric: {err}");
        assert!(
            err.contains("50"),
            "and report the LIMIT the query really carries, not the one the comment mentions: {err}"
        );

        // A `#` in an IRI is not a comment, and stripping to end of line takes the
        // rest of the line with it. This query has no real `LIMIT` at all, and used
        // to load because `limit200` sat inside the IRI: an unbounded `SELECT
        // DISTINCT ?c` sent to a stranger's server.
        let in_an_iri = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?s <http://example.org/vocab#limit200> ?c }\"\n";
        assert!(
            load_metrics(in_an_iri).is_err(),
            "no LIMIT is no LIMIT, whatever an identifier happens to spell"
        );

        // A `#` inside a string literal is data, not the start of a comment, so
        // the line keeps its `LIMIT` and the metric loads. This asserted
        // `is_err()` until 2026-08-29, when the scanner replaced a truncate-at-
        // the-first-`#` pass that could not tell data from a comment.
        let hash_in_a_literal = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?s a ?c FILTER(?c != \\\"#\\\") } LIMIT 200\"\n";
        assert!(
            load_metrics(hash_in_a_literal).is_ok(),
            "a `#` in a literal is data: {:?}",
            load_metrics(hash_in_a_literal).err().map(|e| e.to_string())
        );

        // The case that actually cost this project time, twice. `owl#Class` is an
        // IRI whose `#` is a fragment separator. Truncating there ate the `LIMIT`
        // and the metric was refused for declaring a `sample_limit` its query did
        // not carry, which is a false accusation: the query carries it. Two
        // exploratory metric sets had to put every `LIMIT` on its own line to get
        // around this, and that workaround is what this test retires.
        let hash_in_an_iri = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?c a <http://www.w3.org/2002/07/owl#Class> } LIMIT 200\"\n";
        assert!(
            load_metrics(hash_in_an_iri).is_ok(),
            "an IRI fragment is not a comment: {:?}",
            load_metrics(hash_in_an_iri).err().map(|e| e.to_string())
        );

        // The safety property the crude version was protecting, which must survive
        // the fix: a TRAILING comment mentioning a limit is still stripped. Without
        // this, `rfind("limit")` finds the one in the comment, reads 200 from it,
        // and a query that returns 50 satisfies `sample_limit = 200`. Fifty of a cap
        // of two hundred is then published as COMPLETE, which is a wrong fact about
        // somebody's data and the whole reason the cross-check exists.
        let trailing_comment_lies = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 50 # raise back to limit 200\"\n";
        assert!(
            load_metrics(trailing_comment_lies).is_err(),
            "a comment must never satisfy the cross-check"
        );

        // And both at once, which is the shape a real content metric takes: an IRI
        // carrying a fragment, a real LIMIT, and a comment after it that must not be
        // read as one.
        let both = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?c a <http://www.w3.org/2002/07/owl#Class> } LIMIT 200 # bounded on purpose\"\n";
        assert!(
            load_metrics(both).is_ok(),
            "an IRI fragment, a real limit, and a trailing comment: {:?}",
            load_metrics(both).err().map(|e| e.to_string())
        );

        // And an ordinary comment that says nothing about a limit still loads, so
        // the fix does not cost the file its comments.
        let harmless = "[[metric]]\nid=\"zebra-sample\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"\"\"\n# every class, bounded, in whichever graph the endpoint defaults to\n\
             SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 200\n\"\"\"\n";
        assert!(load_metrics(harmless).is_ok(), "a comment is not a limit, and not a problem either");
    }

    #[test]
    fn a_sample_limit_without_a_query_limit_is_a_load_error() {
        // An unbounded enumeration is not something we send to a stranger's server.
        assert!(load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
             sample_limit=200\nquery=\"SELECT DISTINCT ?c WHERE { ?s a ?c }\"\n"
        ).is_err());
    }

    #[test]
    fn a_sample_limit_on_a_kind_that_reads_no_bindings_is_a_load_error() {
        // Liveness and Cors never populate `bindings`, so a sample limit on one is a
        // promise the probe cannot keep. Same doctrine as the `var` check. The check
        // is now narrower than this test needs (only `SelectIris` may sample, see
        // the AskData test above), and this case stays because a kind that reads no
        // bindings at all is a different mistake from one that reads the wrong term
        // type.
        assert!(load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\n\
             sample_limit=200\nquery=\"ASK{} LIMIT 200\"\n"
        ).is_err());
    }

    #[test]
    fn sample_limit_is_part_of_the_definitions_revision() {
        let with = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
                    sample_limit=200\nquery=\"SELECT ?c WHERE { ?s a ?c } LIMIT 200\"\n";
        let without = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"SelectIris\"\nvar=\"c\"\n\
                       query=\"SELECT ?c WHERE { ?s a ?c } LIMIT 200\"\n";
        assert_ne!(
            definitions_revision(&load_metrics(with).unwrap()),
            definitions_revision(&load_metrics(without).unwrap()),
            "it changes what we publish, so it changes the revision"
        );
    }
}
