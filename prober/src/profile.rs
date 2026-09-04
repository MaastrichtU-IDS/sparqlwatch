//! Profiling the classes an endpoint holds: which properties their instances
//! carry, and how many carry each.
//!
//! The pass this module runs is shaped unlike every other probe in this crate,
//! and the difference is the reason it lives in its own file. Every metric is
//! one declared question with one bounded answer and one verdict, which is why
//! a 60 second metric budget fits it. This fans out over classes DISCOVERED
//! DURING THE SAME SWEEP, so its work cannot be declared in `metrics.toml` and
//! its cost cannot be known before the class list comes back.
//!
//! It publishes no verdict at all. See Ruling 2 in
//! `docs/superpowers/specs/2026-08-29-content-profiles-design.md`: a profile is
//! an inference from a sample, and the six-verdict vocabulary is for what was
//! observed. Nothing here calls `resolve`, so a profile cannot acquire a verdict
//! by accident.
//!
//! WHY THE QUERY IS BUILT HERE and not read from a metric definition: it names
//! the class, which is a runtime value. A metric can declare the SHAPE, and one
//! day may, but the shape is fixed by measurement rather than by preference (see
//! `profile_query`) and a per-class query is not a thing a config file can hold.

use crate::budget::Budget;
use crate::client::Client;
use crate::observe::ProfileRow;

/// How the instances of a class were chosen.
///
/// Carried into the published fact, because a profile drawn from a sixteenth of
/// the instances is not an exact count and a reader who could not tell would
/// take an approximation for one. Measured 2026-08-29: a hash prefix of one hex
/// character kept 12,554 of 200,000 instances and every property frequency
/// landed within 0.002 of the true value, while `LIMIT` without `ORDER BY` was
/// wrong by 0.950 because it returned a contiguous block of the id space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sampling {
    /// Every instance. The frequencies are exact.
    Exact,
    /// The instances whose subject IRI's SHA256 starts with this prefix: one hex
    /// character keeps about a sixteenth, two about a two-hundred-and-fifty-
    /// sixth. NOT combined with a `LIMIT`, which is what reintroduced the bias:
    /// the prefix length is the only knob.
    HashPrefix(String),
}

impl Sampling {
    /// The slug this appears as in the published fact.
    pub fn slug(&self) -> &'static str {
        match self {
            Sampling::Exact => "exact",
            Sampling::HashPrefix(_) => "sha256-prefix",
        }
    }

    /// The prefix, when there is one.
    pub fn prefix(&self) -> Option<&str> {
        match self {
            Sampling::Exact => None,
            Sampling::HashPrefix(p) => Some(p),
        }
    }
}

/// One class's profile: the rows, and how they were sampled.
#[derive(Debug, Clone)]
pub struct ClassProfile {
    /// The class this is about. Carried because two profiles of one endpoint are
    /// otherwise indistinguishable.
    pub class: String,
    pub sampling: Sampling,
    /// One row per property. The `rdf:type` row is the denominator, so a
    /// frequency is a row's `subjects` over that row's `subjects`, computed by
    /// whoever wants one. Nothing here divides, because nothing here thresholds.
    pub rows: Vec<ProfileRow>,
}

/// What a fan-out produced.
///
/// LENT TO the fan-out rather than returned by it, for the reason
/// `EndpointSweep` gives about itself: the endpoint budget cancels the pass by
/// dropping its future, and a future that owned its results would take them with
/// it. Everything finished before the cancellation is already in here.
///
/// A test caught this. The first version returned this by value and the
/// "an expiring budget keeps what finished" test could not be made to pass,
/// because there was nothing for it to observe.
#[derive(Debug, Default)]
pub struct ProfileOutcome {
    pub profiles: Vec<ClassProfile>,
    /// The classes this pass did not profile, whether because the budget expired
    /// before reaching them or because the endpoint refused.
    ///
    /// NAMED rather than omitted, and that is the point. A reader who sees no
    /// profile for a class cannot otherwise tell "this class carries no
    /// properties" from "we never asked", and on these pages an absent qualifier
    /// is a positive claim. The caller publishes these as `NotMeasured`.
    pub unreached: Vec<String>,
}

impl ProfileOutcome {
    /// Name every class this pass neither profiled nor already recorded.
    ///
    /// Called by the caller AFTER the endpoint budget expires, because a
    /// cancelled loop cannot name its own tail: it stops between iterations and
    /// never reaches the code that would. Same division of labour as
    /// `probe_one_endpoint`, which fills the metrics its loop never reached.
    ///
    /// Idempotent, so a caller that runs it on the success path too cannot
    /// double-name anything.
    pub fn name_unreached(&mut self, classes: &[String]) {
        for class in classes {
            let profiled = self.profiles.iter().any(|p| &p.class == class);
            let named = self.unreached.iter().any(|c| c == class);
            if !profiled && !named {
                self.unreached.push(class.clone());
            }
        }
    }
}

/// The query for one class.
///
/// One query, not two: the `rdf:type` group row supplies the denominator, which
/// is what makes a second counting query unnecessary. Measured at 0.002 error on
/// 2026-08-29 in this exact shape.
///
/// `COUNT(DISTINCT ?s)` and never `COUNT(*)`, so a multi-valued property cannot
/// push a frequency above 1.0. `COUNT(DISTINCT ?dt)` beside a `SAMPLE`, so a
/// property carrying mixed datatypes is visible as mixed rather than reduced to
/// whichever one the store returned first.
///
/// No `LIMIT` anywhere. The sample size is decided by the hash prefix, because
/// adding a `LIMIT` cuts by the store's own iteration order and reintroduces
/// exactly the bias the prefix exists to remove.
pub fn profile_query(class: &str, sampling: &Sampling) -> String {
    let filter = match sampling.prefix() {
        Some(p) => format!("\n      FILTER(STRSTARTS(SHA256(STR(?s)), \"{p}\"))"),
        None => String::new(),
    };
    format!(
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?subjects) \
         (COUNT(DISTINCT ?dt) AS ?datatypes) (SAMPLE(?dt) AS ?anyDatatype)\n\
         WHERE {{\n  \
         {{ SELECT ?s WHERE {{\n      \
         ?s a <{class}> .{filter}\n  }} }}\n  \
         ?s ?p ?o .\n  \
         BIND(IF(isIRI(?o), \"IRI\", DATATYPE(?o)) AS ?dt)\n\
         }}\n\
         GROUP BY ?p"
    )
}

/// Profile every class in `classes`, in order, until the endpoint budget ends.
///
/// EACH CLASS IS BOUNDED SEPARATELY by the metric budget, and the whole pass by
/// the endpoint budget the caller already holds. Both bounds are needed: without
/// the per-class one a single hostile class consumes the pass, and without the
/// outer one a long class list consumes the sweep.
///
/// ORDER IS THE CALLER'S. This walks the list as given rather than sorting it,
/// so a caller that wants the most-used classes first can say so, and `unreached`
/// is then the tail rather than an arbitrary subset.
pub async fn profile_classes(
    endpoint: &str,
    classes: &[String],
    sampling: Sampling,
    client: &Client,
    budget: Budget,
    out: &mut ProfileOutcome,
) {
    for class in classes.iter() {
        let query = profile_query(class, &sampling);
        let observed = budget.with_metric_budget(client.profile_class(endpoint, &query)).await;
        match observed {
            // The budget for THIS class expired. The rest of the pass may still
            // have time, so this is not the end: one slow class does not decide
            // what happens to the others.
            Err(_) => out.unreached.push(class.clone()),
            Ok(o) => match o.profile {
                Some(rows) => out.profiles.push(ClassProfile {
                    class: class.clone(),
                    sampling: sampling.clone(),
                    rows,
                }),
                // A refusal, or a body we could not read. Not an empty profile:
                // that would say the class carries no properties.
                None => out.unreached.push(class.clone()),
            },
        }
    }
    // Nothing names the tail here. The endpoint budget cancels this future
    // between iterations, so the code after the loop is exactly the code a
    // cancelled pass does not run: naming the tail here would work on the happy
    // path and silently do nothing in the case it exists for. The caller calls
    // `name_unreached` on both paths instead.
}
