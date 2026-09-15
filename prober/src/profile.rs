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
    /// The first `n` instances the store happens to return, bounded by a
    /// `LIMIT` inside the subquery that selects them.
    ///
    /// THE LAST RUNG, AND THE ONLY ONE THAT IS NOT REPRESENTATIVE. The measured
    /// bias is in this enum's own header: a `LIMIT` with no `ORDER BY` was
    /// wrong by 0.950 against 0.002 for a hash prefix, because it returns a
    /// contiguous block of the id space. Nothing about that has changed and it
    /// is why every hash rung is tried first.
    ///
    /// It exists because the alternative on some endpoints is nothing at all.
    /// The hash rungs do not reduce work on a store that refuses aggregates:
    /// `SHA256(STR(?s))` has to be computed for every subject, so a sixteenth
    /// costs what the whole costs. Measured against semopenalex.org on
    /// 2026-09-15, which answers `?s a <C>` in 0.84 s and returns HTTP 200 with
    /// zero rows after 10.8 s for the same query with a `GROUP BY`: exact
    /// failed, one-sixteenth failed in the same 10.8 s, and `LIMIT 200`
    /// answered in 0.89 s with 13 properties.
    ///
    /// WHAT IT IS SAFE TO READ FROM IT. That a property OCCURS on instances of
    /// the class is sound: we saw it on instances that exist. `subjects` and
    /// `profileDenominator` are true of the sample, as they are for every rung.
    /// What does NOT follow is a frequency for the CLASS, because the sample is
    /// a block rather than a draw. Anything extrapolating from a profile has to
    /// read `profileSampling` first, and this rung's slug is deliberately not
    /// the hash one.
    FirstN(u32),
}

impl Sampling {
    /// The slug this appears as in the published fact.
    pub fn slug(&self) -> &'static str {
        match self {
            Sampling::Exact => "exact",
            Sampling::HashPrefix(_) => "sha256-prefix",
            Sampling::FirstN(_) => "first-n",
        }
    }

    /// The `LIMIT` on the subject subquery, when there is one. `None` for every
    /// rung that samples the whole population.
    pub fn limit(&self) -> Option<u32> {
        match self {
            Sampling::Exact | Sampling::HashPrefix(_) => None,
            Sampling::FirstN(n) => Some(*n),
        }
    }

    /// The prefix, when there is one.
    pub fn prefix(&self) -> Option<&str> {
        match self {
            Sampling::Exact | Sampling::FirstN(_) => None,
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
/// Two hash rungs and not more. Both prefixes cost the same 1.8s in that
/// measurement, because the work is the SHA256 scan over every subject rather
/// than the few that survive it, so a third would buy a worse sample for the
/// same price as the second.
///
/// AND THEN ONE LAST RUNG THAT IS NOT A HASH. That same fact -- the prefixes
/// cost what the exact scan costs -- means the hash rungs rescue nothing from a
/// store that refuses aggregates rather than merely finding them slow. On
/// semopenalex.org every hash rung failed in the identical 10.8 s as exact, and
/// a `LIMIT 200` answered in 0.89 s. `Sampling::FirstN` documents what may and
/// may not be read from it; it is last because it is the only rung whose sample
/// does not generalise.
pub fn ladder_from(start: &Sampling) -> Vec<Sampling> {
    const RUNGS: [&str; 2] = ["0", "00"];
    /// The bound on the last rung. 200 because it is what answered: against
    /// semopenalex.org's largest class, `LIMIT 200` found 13 properties in
    /// 0.89 s and `LIMIT 1000` found the same 13 in 1.10 s, so a larger sample
    /// bought nothing but load on somebody else's server.
    const LAST_RESORT: u32 = 200;
    let from = match start {
        Sampling::Exact => 0,
        // A configured prefix is the floor: keep it and everything shorter-
        // sampled than it. An unrecognised prefix length is its own only rung,
        // because guessing which of these it sits between would silently widen
        // or narrow what the operator asked for.
        // Already the last rung: it is its own only ladder. Nothing is below
        // it, and stepping UP into a hash sample would be escalating into the
        // scan this rung exists to avoid.
        Sampling::FirstN(_) => return vec![start.clone()],
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
    rungs.push(Sampling::FirstN(LAST_RESORT));
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
    // INSIDE the subject subquery, which is the whole point of this rung. A
    // `LIMIT` on the outer query would bound the ROWS returned and not the work
    // done: the GROUP BY still has to scan every instance before it can discard
    // any, which is exactly the query semopenalex.org answers with 200 and no
    // rows after 10.8 s. Bounding the subjects bounds the scan.
    let limit = match sampling.limit() {
        Some(n) => format!("\n  }} LIMIT {n} }}"),
        // NOT `"\n  }} }}"`. `}}` is an escape inside `format!` and a pair of
        // literal braces everywhere else, so the plain-literal spelling emitted
        // four braces and made every unbounded query malformed. The end-to-end
        // vocabulary tests caught it; the unit test above did not, because it
        // only exercised the bounded branch.
        None => "\n  } }".to_string(),
    };
    format!(
        "SELECT ?p (COUNT(DISTINCT ?s) AS ?subjects) \
         (COUNT(DISTINCT ?dt) AS ?datatypes) (SAMPLE(?dt) AS ?anyDatatype)\n\
         WHERE {{\n  \
         {{ SELECT ?s WHERE {{\n      \
         {{ ?s a <{class}> }} UNION {{ GRAPH ?swg {{ ?s a <{class}> }} }}{filter}{limit}\n  \
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
                    // NO ROWS IS NOT A PROFILE, and this is the same rule the
                    // counting resolver states as "no row is not zero".
                    //
                    // A class we are profiling came from the enumeration, so it
                    // has instances; an instance matched `?s a <C>`, so it
                    // carries at least that triple; so `?s ?p ?o` matches at
                    // least once and a real profile of it can never be empty.
                    // An empty result is therefore a refusal wearing a success
                    // code, and recorded as a profile it would publish "this
                    // class carries no properties" about a class that has some.
                    //
                    // Not hypothetical. semopenalex.org answers every aggregate
                    // over a large set with HTTP 200 and zero rows after 10.8 s,
                    // so before this it was published as a store whose classes
                    // hold nothing.
                    Some(rows) if rows.is_empty() => {
                        // Straight to the next rung, WITHOUT consulting
                        // `worth_a_smaller_sample`: that reads the status, and
                        // the status here is 200. It is the emptiness rather
                        // than the code that says the query was too big.
                        if last {
                            out.unreached.push(class.clone());
                            break;
                        }
                    }
                    Some(rows) => {
                        out.profiles.push(ClassProfile {
                            class: class.clone(),
                            sampling: sampling.clone(),
                            rows,
                        });
                        break;
                    }
                    // A refusal, or a body we could not read.
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

    /// The ladder ends on a bound, and the bound is inside the subquery.
    ///
    /// A `LIMIT` on the outer query bounds the rows and not the work: the
    /// GROUP BY still scans every instance first, which is the query
    /// semopenalex.org answers with 200 and nothing after 10.8 s. The point of
    /// this rung is that the subjects are bounded before anything groups them.
    #[test]
    fn the_last_rung_bounds_the_subjects_and_not_the_rows() {
        let ladder = ladder_from(&Sampling::Exact);
        let last = ladder.last().expect("the ladder is never empty");
        assert!(matches!(last, Sampling::FirstN(_)), "the ladder ends on a bounded sample: {ladder:?}");

        let q = profile_query("http://example.org/C", last);
        let limit = q.find("LIMIT").expect("the last rung carries a LIMIT");
        let group = q.find("GROUP BY").expect("the profile groups");
        assert!(limit < group, "the LIMIT must bind the subjects before the grouping: {q}");
        // Inside the subject subquery: the LIMIT closes it, so the brace that
        // ends the inner SELECT comes after the LIMIT and before the outer
        // pattern that joins ?s to its properties.
        assert!(q.contains("LIMIT 200 }"), "the LIMIT closes the subject subquery: {q}");
        assert!(!q.trim_end().ends_with("LIMIT 200"), "a trailing LIMIT would bound rows, not work: {q}");
    }

    /// Every rung's query is well-formed, which the bounded one nearly was not.
    ///
    /// The `LIMIT` is spliced into a `format!` string, and the unbounded
    /// spelling of that splice was a plain literal where `}}` means two braces
    /// rather than one. Every non-bounded query came out with four, and only
    /// the end-to-end vocabulary tests noticed. Balance is a cheap thing to
    /// assert and it fails in the unit suite instead of after a mock sweep.
    #[test]
    fn every_rung_produces_a_balanced_query() {
        for rung in ladder_from(&Sampling::Exact) {
            let q = profile_query("http://example.org/C", &rung);
            let opens = q.matches('{').count();
            let closes = q.matches('}').count();
            assert_eq!(opens, closes, "{:?} produced unbalanced braces:\n{q}", rung.slug());
            assert!(!q.contains("}}"), "{:?} produced a doubled brace:\n{q}", rung.slug());
        }
    }

    /// Every hash rung is tried before the one that does not generalise.
    ///
    /// The bias is measured and recorded on `Sampling`: 0.950 against 0.002.
    /// The bounded rung exists only for endpoints where the hash rungs rescue
    /// nothing, so it must never displace one that would have worked.
    #[test]
    fn the_unrepresentative_rung_is_last_and_never_earlier() {
        let ladder = ladder_from(&Sampling::Exact);
        let bounded = ladder.iter().position(|r| matches!(r, Sampling::FirstN(_)));
        assert_eq!(bounded, Some(ladder.len() - 1), "{ladder:?}");
        assert!(
            ladder.iter().take(ladder.len() - 1).all(|r| r.limit().is_none()),
            "no rung above the last may be bounded: {ladder:?}",
        );
        // And its slug is not the hash one, so a reader cannot mistake the two.
        assert_ne!(Sampling::FirstN(200).slug(), Sampling::HashPrefix("0".into()).slug());
    }

    /// The bounded rung is its own only ladder.
    ///
    /// Stepping anywhere from it means stepping UP into the scan it exists to
    /// avoid, and an operator who configured it asked for the bound.
    #[test]
    fn a_bounded_start_does_not_escalate_into_a_scan() {
        assert_eq!(ladder_from(&Sampling::FirstN(50)), vec![Sampling::FirstN(50)]);
    }


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

    /// The hash rungs, in order, and then the bounded one.
    ///
    /// The trailing `None` is `Sampling::FirstN`, which carries no prefix. It
    /// joined the ladder on 2026-09-15 for stores that refuse aggregates
    /// outright, where every hash rung costs what the exact scan costs. The
    /// hash order above it is unchanged and is what this test has always been
    /// about.
    #[test]
    fn an_exact_start_climbs_down_through_both_prefixes_then_a_bound() {
        assert_eq!(
            prefixes(&ladder_from(&Sampling::Exact)),
            [None, Some("0"), Some("00"), None]
        );
        assert_eq!(ladder_from(&Sampling::Exact)[0], Sampling::Exact);
        assert!(matches!(ladder_from(&Sampling::Exact)[3], Sampling::FirstN(_)));
    }

    /// A configured prefix is a FLOOR, not a starting hint. An operator who
    /// asked for a sixteenth is asking not to pay for the exact scan, so the
    /// ladder must never climb back up into it.
    #[test]
    fn a_configured_prefix_is_never_escalated_upward_into_an_exact_scan() {
        let rungs = ladder_from(&Sampling::HashPrefix("0".into()));
        // The configured rung is tried FIRST, then the smaller one below it,
        // then the bound that is below every hash.
        assert_eq!(prefixes(&rungs), [Some("0"), Some("00"), None]);
        assert!(
            !rungs.iter().any(|r| matches!(r, Sampling::Exact)),
            "an exact scan is what the operator asked to avoid"
        );
    }

    /// The smallest HASH rung still has the bound below it.
    ///
    /// It had nowhere left to go until 2026-09-15, and that was the whole
    /// problem on a store where a two-character prefix costs what the exact
    /// scan costs: the ladder ended having learned nothing.
    #[test]
    fn the_smallest_hash_rung_falls_through_to_the_bound() {
        assert_eq!(
            prefixes(&ladder_from(&Sampling::HashPrefix("00".into()))),
            [Some("00"), None]
        );
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
