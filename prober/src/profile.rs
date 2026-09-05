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

/// The rungs to try for one class, from `start` down to the smallest sample.
///
/// Escalation goes toward SMALLER samples, never larger: a class the endpoint
/// could not profile exactly might manage a sixteenth of it. Starting at the
/// metric's configured sampling rather than always at `Exact` means an operator
/// who set `sample_prefix = "0"` gets a ladder from there, and is never
/// escalated back up into the exact scan they asked to avoid.
///
/// Measured 2026-09-05 against ontoexplorer's content store, 12.5M triples:
/// `owl:Restriction` (1,352,666 instances) answered 504 at its gateway's 30
/// second limit on the exact scan and 1.8s with a one character prefix. Without
/// this ladder that class is simply unprofiled, and it is the biggest class on
/// the endpoint.
///
/// Two extra rungs and not more. Both prefixes cost the same 1.8s in that
/// measurement, because the work is the SHA256 scan over every subject rather
/// than the few that survive it, so a third rung would buy a worse sample for
/// the same price as the second.
pub fn ladder_from(start: &Sampling) -> Vec<Sampling> {
    const RUNGS: [&str; 2] = ["0", "00"];
    let from = match start {
        Sampling::Exact => 0,
        // A configured prefix is the floor: keep it and everything shorter-
        // sampled than it. An unrecognised prefix length is its own only rung,
        // because guessing which of these it sits between would silently widen
        // or narrow what the operator asked for.
        Sampling::HashPrefix(p) => match RUNGS.iter().position(|r| r == p) {
            // `i` and not `i + 1`: the configured rung is the ladder's FIRST
            // attempt, not the one above it. Skipping past it returned an EMPTY
            // ladder for the last rung, which profiled nothing at all rather
            // than profiling it once. A test caught that.
            Some(i) => i,
            None => return vec![start.clone()],
        },
    };
    let mut rungs = Vec::new();
    if matches!(start, Sampling::Exact) {
        rungs.push(Sampling::Exact);
    }
    rungs.extend(RUNGS[from..].iter().map(|r| Sampling::HashPrefix(r.to_string())));
    rungs
}

/// Whether a failed profile query is worth retrying on a smaller sample.
///
/// The distinction is "too much work" against "the wrong question". A gateway
/// timeout, a 5xx, or no status at all (our own request budget ran out) all say
/// the query was too big, and a smaller sample is a different query worth
/// asking. A 4xx says the endpoint understood and refused, so every rung below
/// would be refused too: escalating there would triple the cost of a malformed
/// query against every class on the endpoint.
fn worth_a_smaller_sample(status: Option<u16>) -> bool {
    match status {
        // Our own budget, or a connection that never produced a response.
        None => true,
        Some(s) if (500..=599).contains(&s) => true,
        Some(_) => false,
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
///
/// BOTH patterns are `{ ... } UNION { GRAPH ?g { ... } }`, and that is not
/// decoration. Without it each pattern reads the DEFAULT GRAPH ONLY, so an
/// endpoint keeping its data in named graphs profiles nothing at all, silently:
/// the query returns no rows, an empty profile publishes nothing, and the run
/// carries fewer profiles with no fact saying why. Measured 2026-09-05 against
/// the synthetic endpoint, whose five classes sit one in the default graph and
/// four in named ones: the previous shape returned rows for exactly the one.
///
/// It is the same defect and the same fix as `geo-data` and the class
/// enumeration, both of which have carried the union since they shipped. Named
/// graphs are the normal arrangement in Virtuoso, GraphDB and Blazegraph, so
/// the default-graph-only form is wrong about most real endpoints rather than
/// about an unusual one.
///
/// The graph variables are `?swg` and `?spg` and never `?p`, `?s`, `?o` or
/// `?dt`: a `GRAPH ?p { ?s ?p ?o }` parses, runs, and can never bind, which is
/// the collision `metrics.rs` refuses for the shipped queries.
///
/// The filter sits OUTSIDE the union rather than inside both arms, so the
/// sampling rule is stated once and cannot come to differ between them.
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
         {{ ?s a <{class}> }} UNION {{ GRAPH ?swg {{ ?s a <{class}> }} }}{filter}\n  }} }}\n  \
         {{ ?s ?p ?o }} UNION {{ GRAPH ?spg {{ ?s ?p ?o }} }}\n  \
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
    let rungs = ladder_from(&sampling);
    for class in classes.iter() {
        // The ladder, per class. Each rung is a smaller sample of the same
        // class, tried only when the rung above failed in a way a smaller
        // sample could fix.
        //
        // THE WHOLE LADDER SHARES ONE METRIC BUDGET, taken per rung so the
        // remaining time shrinks as the ladder is climbed. That is what stops a
        // class from costing three full request budgets: an exact scan that
        // burns 30 of the 60 seconds leaves 30 for the rest, and a second rung
        // that burns the remainder leaves none for a third. The budget decides
        // how far the ladder gets rather than the rung count.
        for (rung, sampling) in rungs.iter().enumerate() {
            let last = rung + 1 == rungs.len();
            let query = profile_query(class, sampling);
            let observed =
                budget.with_metric_budget(client.profile_class(endpoint, &query)).await;
            match observed {
                // The metric budget for THIS class is gone, so there is no time
                // for a lower rung either. The rest of the pass may still have
                // time: one slow class does not decide what happens to the
                // others.
                Err(_) => {
                    out.unreached.push(class.clone());
                    break;
                }
                Ok(o) => match o.profile {
                    Some(rows) => {
                        out.profiles.push(ClassProfile {
                            class: class.clone(),
                            sampling: sampling.clone(),
                            rows,
                        });
                        break;
                    }
                    // A refusal, or a body we could not read. Not an empty
                    // profile: that would say the class carries no properties.
                    None => {
                        if last || !worth_a_smaller_sample(o.status) {
                            out.unreached.push(class.clone());
                            break;
                        }
                        // Fall through to the next rung. Nothing is recorded
                        // yet: a class that succeeds on a smaller sample is
                        // profiled, not unreached, and one that never succeeds
                        // is named once by whichever arm above ends its ladder.
                    }
                },
            }
        }
    }
    // Nothing names the tail here. The endpoint budget cancels this future
    // between iterations, so the code after the loop is exactly the code a
    // cancelled pass does not run: naming the tail here would work on the happy
    // path and silently do nothing in the case it exists for. The caller calls
    // `name_unreached` on both paths instead.
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The defect the synthetic endpoint found on 2026-09-05, pinned.
    ///
    /// Both patterns must look in named graphs. Without it the pass profiles
    /// nothing on any endpoint that keeps its data in one, and does so
    /// SILENTLY: no rows, an empty profile, nothing published, and no fact
    /// saying why. Wiremock cannot catch this, because a canned body answers
    /// the same whatever the query asks, which is exactly why it shipped.
    #[test]
    fn both_patterns_look_in_named_graphs_as_well_as_the_default_one() {
        let q = profile_query("http://example.org/C", &Sampling::Exact);
        assert!(
            q.contains("{ ?s a <http://example.org/C> } UNION { GRAPH ?swg"),
            "the subject selection must union over named graphs: {q}"
        );
        assert!(
            q.contains("{ ?s ?p ?o } UNION { GRAPH ?spg { ?s ?p ?o } }"),
            "and so must the property walk: {q}"
        );
    }

    /// A graph variable colliding with a reported one parses, runs, and can
    /// never bind. `metrics.rs` refuses that collision for the queries in
    /// `metrics.toml`; this query is built in code, so it needs its own guard.
    #[test]
    fn no_graph_variable_collides_with_a_variable_the_query_reports() {
        let q = profile_query("http://example.org/C", &Sampling::HashPrefix("0".into()));
        for reported in ["?p", "?s", "?o", "?dt", "?subjects", "?datatypes", "?anyDatatype"] {
            assert!(
                !q.contains(&format!("GRAPH {reported} ")),
                "GRAPH {reported} can never bind: {q}"
            );
        }
    }

    /// One filter, outside the union, so the sampling rule is stated once and
    /// cannot come to differ between the two arms.
    #[test]
    fn the_sampling_filter_is_stated_once() {
        let q = profile_query("http://example.org/C", &Sampling::HashPrefix("00".into()));
        assert_eq!(q.matches("STRSTARTS").count(), 1, "{q}");
        assert!(q.contains("\"00\""), "the prefix reaches the query: {q}");
    }

    /// An exact profile sends no filter at all, rather than one that matches
    /// everything: `STRSTARTS(SHA256(...), "")` is true for every subject, so
    /// it would hash every subject in the store to decide nothing.
    #[test]
    fn an_exact_profile_hashes_nothing() {
        let q = profile_query("http://example.org/C", &Sampling::Exact);
        assert!(!q.contains("SHA256"), "{q}");
    }

    // -----------------------------------------------------------------------
    // The fallback ladder
    // -----------------------------------------------------------------------

    fn prefixes(rungs: &[Sampling]) -> Vec<Option<&str>> {
        rungs.iter().map(|r| r.prefix()).collect()
    }

    #[test]
    fn an_exact_start_climbs_down_through_both_prefixes() {
        assert_eq!(
            prefixes(&ladder_from(&Sampling::Exact)),
            [None, Some("0"), Some("00")]
        );
    }

    /// A configured prefix is a FLOOR, not a starting hint. An operator who
    /// asked for a sixteenth is asking not to pay for the exact scan, so the
    /// ladder must never climb back up into it.
    #[test]
    fn a_configured_prefix_is_never_escalated_upward_into_an_exact_scan() {
        let rungs = ladder_from(&Sampling::HashPrefix("0".into()));
        // The configured rung is tried FIRST, then the smaller one below it.
        assert_eq!(prefixes(&rungs), [Some("0"), Some("00")]);
        assert!(
            !rungs.iter().any(|r| matches!(r, Sampling::Exact)),
            "an exact scan is what the operator asked to avoid"
        );
    }

    #[test]
    fn the_smallest_rung_has_nowhere_left_to_go() {
        assert_eq!(prefixes(&ladder_from(&Sampling::HashPrefix("00".into()))), [Some("00")]);
    }

    /// An unrecognised prefix is its own only rung. Guessing where "abc" sits
    /// between the known rungs would silently widen or narrow the sample an
    /// operator configured, and either direction is a change they did not ask
    /// for.
    #[test]
    fn an_unrecognised_prefix_is_its_own_only_rung() {
        let rungs = ladder_from(&Sampling::HashPrefix("abc".into()));
        assert_eq!(prefixes(&rungs), [Some("abc")]);
    }

    /// The rule that keeps the ladder from tripling the cost of every class on
    /// an endpoint that refuses the query outright.
    #[test]
    fn only_a_failure_a_smaller_sample_could_fix_escalates() {
        // Too much work: worth asking a smaller question.
        assert!(worth_a_smaller_sample(None), "our own request budget ran out");
        assert!(worth_a_smaller_sample(Some(504)), "the measured gateway timeout");
        assert!(worth_a_smaller_sample(Some(500)));
        assert!(worth_a_smaller_sample(Some(503)));
        // The wrong question: every rung below is refused the same way.
        assert!(!worth_a_smaller_sample(Some(400)), "a malformed query stays malformed");
        assert!(!worth_a_smaller_sample(Some(404)));
        assert!(!worth_a_smaller_sample(Some(403)));
        // Not a failure at all, so this is never asked; pinned anyway, because
        // a `true` here would retry a class that already answered.
        assert!(!worth_a_smaller_sample(Some(200)));
    }

    /// Every rung is a real query, and a distinct one. Two rungs generating the
    /// same text would make the ladder a retry loop that asks the identical
    /// question and cannot succeed the second time.
    #[test]
    fn each_rung_asks_a_different_question() {
        let rungs = ladder_from(&Sampling::Exact);
        let queries: std::collections::BTreeSet<String> =
            rungs.iter().map(|r| profile_query("http://example.org/C", r)).collect();
        assert_eq!(queries.len(), rungs.len(), "every rung must be its own query");
    }
}
