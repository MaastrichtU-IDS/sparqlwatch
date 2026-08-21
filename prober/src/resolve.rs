use crate::budget::Expired;
use crate::declare::Declarations;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::{BodyKind, Observation};
use crate::verdict::{Level, Verdict};

/// What the endpoint says about itself, from its service description.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub claimed: bool,
}

impl Declared {
    /// `claimed` is true only when `def` names a declaration that could
    /// speak for it (`declared_by: Some(iri)`) and that declaration actually
    /// appears in the fetched description. A metric with no `declared_by` --
    /// liveness, response time, CORS headers -- is never "claimed": there is
    /// no declaration in the vocabulary that could confirm or deny it.
    pub fn from(defs: &Declarations, def: &MetricDef) -> Declared {
        Declared { claimed: def.declared_by.as_deref().is_some_and(|iri| defs.declares(iri)) }
    }
}

/// Whether the request reached the endpoint and got back a successful HTTP
/// status. Used to tell "the endpoint told us it has nothing" apart from
/// "something upstream (a proxy, an outage) prevented us from finding out" —
/// only the former is safe to report as `Absent`.
fn answered_ok(o: &Observation) -> bool {
    matches!(o.status, Some(s) if (200..=299).contains(&s))
}

/// Turn observation plus declaration into a verdict. This is the only place
/// judgement happens: no probing, no I/O, pure function.
///
/// The rule applied uniformly below: **an absence claim requires
/// `answered_ok`.** `Absent` (and `DeclaredButWrong`, which the spec ranks as
/// worse still) may only be minted from a response the endpoint itself
/// authored with a 2xx status. Anything else -- a throttle, a gateway error,
/// an unparseable body, a response we never got -- is `Indeterminate`. There
/// is exactly one deliberate exception: the `Cors` *positive* case, because an
/// `access-control-allow-origin` header proves CORS at any status.
pub fn resolve(def: &MetricDef, declared: Declared, obs: Result<&Observation, Expired>) -> Verdict {
    let o = match obs {
        Err(Expired) => return Verdict::Indeterminate,
        Ok(o) => o,
    };

    // A transport failure or an HTML console tells us nothing about the
    // attribute itself.
    if o.error.is_some() || o.body_kind == BodyKind::Html {
        return Verdict::Indeterminate;
    }

    match def.kind {
        ProbeKind::AskFilter | ProbeKind::AskData => match (o.boolean, def.expect) {
            // This arm serves both probe kinds, and the two readings differ:
            // for `AskFilter` a wrong boolean means broken semantics; for
            // `AskData` a `false` means the data is absent. It is safe today
            // only because no shipped `AskData` metric sets `expect` — the
            // day one does, genuine data absence would silently become
            // `DeclaredButWrong` instead of `Absent`.
            // A capability claim needs the endpoint's own successful answer just
            // as much as an absence claim does: a 500 body that happens to carry
            // {"boolean": true} is not the engine confirming anything.
            (Some(got), Some(want)) if got == want => {
                if !answered_ok(o) {
                    Verdict::Indeterminate
                } else if declared.claimed {
                    Verdict::Verified
                } else {
                    Verdict::UndeclaredButVerified
                }
            }
            // Bound but wrong: the function answered, and answered
            // incorrectly. Only claimable when the endpoint itself answered
            // with a success status; a 502 body that happens to parse is not
            // the engine's answer.
            (Some(_), Some(_)) => {
                if answered_ok(o) { Verdict::DeclaredButWrong } else { Verdict::Indeterminate }
            }
            (Some(true), None) => {
                if !answered_ok(o) {
                    Verdict::Indeterminate
                } else if declared.claimed {
                    Verdict::Verified
                } else {
                    Verdict::UndeclaredButVerified
                }
            }
            (Some(false), None) => {
                if answered_ok(o) { Verdict::Absent } else { Verdict::Indeterminate }
            }
            (None, _) => {
                if declared.claimed { Verdict::DeclaredOnly } else { Verdict::Indeterminate }
            }
        },
        ProbeKind::Cors => {
            if o.cors {
                // The header proves CORS is configured whatever the status
                // code, so the positive case is not gated on `answered_ok`.
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            } else if answered_ok(o) {
                Verdict::Absent
            } else {
                // A proxy-generated error status cannot be attributed to the
                // endpoint's own CORS configuration.
                Verdict::Indeterminate
            }
        }
        ProbeKind::Liveness => {
            // A SPARQL JSON body is proof it speaks the protocol. A non-SPARQL
            // body only establishes absence when the endpoint answered
            // successfully: a 429 with a plain-text body, a non-HTML 502/503
            // from an intermediary, or a 401/403 all mean we never got to ask
            // the question. This metric runs against every endpoint every
            // sweep and a later stage reads it to decide admission, so a
            // throttled endpoint must not be tombstoned as unreachable.
            if o.body_kind == BodyKind::SparqlJson {
                Verdict::Verified
            } else if answered_ok(o) {
                Verdict::Absent
            } else {
                Verdict::Indeterminate
            }
        }
        ProbeKind::SelectIris => {
            if !o.bindings.is_empty() {
                Verdict::Verified
            } else if o.body_kind == BodyKind::SparqlJson && answered_ok(o) {
                // Empty bindings are only evidence of absence when we
                // actually parsed a result.
                Verdict::Absent
            } else {
                Verdict::Indeterminate
            }
        }
        // `resolve()` only carries `declared.claimed`, not the fetched
        // `Declarations` itself, so it cannot grade here; it delegates to
        // `resolve_fetch` for the verdict and discards the level. `run_sweep`
        // calls `resolve_fetch` directly for the graded row, so the level is
        // never lost, only unavailable on this path.
        ProbeKind::FetchWellKnown => resolve_fetch(&Declarations::empty(), Ok(o)).0,
    }
}

/// Grade a fetched service description by what its own content declares,
/// mapping the fields straight onto `grade_service_description`.
pub fn grade_from_declarations(defs: &Declarations) -> Level {
    grade_service_description(
        defs.triples,
        defs.names_dataset,
        defs.has_void_partitions,
        defs.has_entailment,
        !defs.extension_functions.is_empty(),
        defs.has_example_resources,
    )
}

/// Resolve the `FetchWellKnown` probe: did the endpoint publish a
/// dereferenceable, parseable service description, and if so, how informative
/// is it. The fetch behind this is a queryless GET on the endpoint URL itself
/// (see `Client::fetch_rdf`), not a request to any `/.well-known/` path; the
/// probe-kind name is legacy from an earlier design and is kept for now, but
/// no code here dereferences a well-known URL.
///
/// `Absent` is minted only from a `404` or a `410`: those are the only status
/// codes that speak to what is published at the URL -- nothing was ever
/// there, or it was and has since been removed. Every other non-2xx status,
/// `401`/`403` included, describes our request or the server's state, not
/// the endpoint's published metadata, so it stays `Indeterminate`. This is
/// deliberately narrow, not merely cautious: the survey behind this project
/// recorded 10 of 548 endpoints returning a plain `400` for a queryless
/// GET -- rejecting the very shape of request this probe makes -- plus 5
/// more unrelated `400`s, and a `429` throttle is exactly the situation
/// where we know least about what exists. Treating the wider `4xx` range as
/// `Absent` would have reported every one of those as "no service
/// description published," a false absence -- the failure mode this system
/// exists to prevent. A future reader tempted to widen this back to `4xx`
/// should re-read that number first.
pub fn resolve_fetch(defs: &Declarations, obs: Result<&Observation, Expired>) -> (Verdict, Option<Level>) {
    let o = match obs {
        Err(Expired) => return (Verdict::Indeterminate, None),
        Ok(o) => o,
    };

    if o.error.is_some() || o.body_kind == BodyKind::Html {
        return (Verdict::Indeterminate, None);
    }

    // A parseable RDF body is a published description only when the endpoint
    // answered with a 2xx. `verified` is as assertive a verdict as `absent`,
    // so it gets the same gate every other probe kind applies: a 429 throttle
    // notice, a 500 error document and a 503 maintenance page can all carry an
    // RDF-ish payload, and none of them is the endpoint publishing a service
    // description. A non-2xx falls through to the status match below, so 404
    // and 410 still resolve to a genuine absence and everything else stays
    // indeterminate.
    if o.body_kind == BodyKind::Rdf && answered_ok(o) {
        return (Verdict::Verified, Some(grade_from_declarations(defs)));
    }

    match o.status {
        Some(404) | Some(410) => (Verdict::Absent, Some(Level(0))),
        _ => (Verdict::Indeterminate, None),
    }
}

/// Grade a service description by what it actually tells a client, not by its
/// presence. 21 of 28 descriptions in the wild are the same 14-triple
/// Virtuoso stub, so a stub must not score the same as a real description.
///
/// Reached via `grade_from_declarations`, which `resolve_fetch` calls once a
/// fetch probe reports a parsed `BodyKind::Rdf` body from a 2xx. The grade is
/// carried out of `resolve_fetch` by `run_sweep` and lands on the
/// `MeasurementRow.level` of any metric its definition marks `graded`.
///
/// The ladder is the spec's (design doc line 433): level 4 is "declares an
/// entailment regime, example resources, or extension functions", so all
/// three reach it. It is monotonic by construction: every criterion is a
/// declaration the description either makes or does not, and adding one can
/// only move a description up the ladder, never down. That is what makes the
/// number readable as informativeness rather than as a taxonomy.
pub fn grade_service_description(
    triples: usize,
    names_dataset: bool,
    has_void_partitions: bool,
    has_entailment: bool,
    has_extension_functions: bool,
    has_example_resources: bool,
) -> Level {
    let n: u8 = if triples == 0 {
        0
    } else if has_entailment || has_extension_functions || has_example_resources {
        4
    } else if has_void_partitions {
        3
    } else if names_dataset {
        2
    } else {
        1
    };
    // Levels are born valid: constructing through `Level::new` rather than
    // the raw tuple keeps the 0..=4 bound enforced at the one place grades
    // are computed, even though the field itself is public.
    Level::new(n).expect("service-description grade is 0..=4 by construction")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(kind: ProbeKind, expect: Option<bool>) -> MetricDef {
        MetricDef {
            id: "t".into(),
            label: "t".into(),
            dimension: "d".into(),
            kind,
            query: None,
            expect,
            var: None,
            declared_by: None,
            graded: false,
        }
    }

    fn obs(boolean: Option<bool>) -> Observation {
        Observation { status: Some(200), cors: true, boolean, bindings: vec![], body_kind: BodyKind::SparqlJson, body: None, content_type: None, elapsed_ms: 5, error: None }
    }

    #[test]
    fn works_and_declared_is_verified() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: true }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn works_but_undeclared_is_its_own_verdict() {
        // The commonest real case: 18 endpoints evaluate geof:sfWithin and
        // none of them declares it.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn wrong_answer_is_worse_than_absent() {
        // 9 endpoints answered `false` to a filter a conformant engine must
        // answer `true`: the function is bound but the semantics are wrong.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&obs(Some(false))));
        assert_eq!(v, Verdict::DeclaredButWrong);
    }

    #[test]
    fn a_timeout_is_indeterminate_never_absent() {
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_html_front_end_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Html;
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn claimed_but_unprobeable_is_indeterminate_not_declared_only() {
        // Before `resolve_fetch` existed, an unparsed FetchWellKnown body
        // fell back to `declared.claimed` and could reach `DeclaredOnly`.
        // `resolve_fetch` judges the fetched body on its own terms and takes
        // no `claimed` flag at all, so an unparsed body is `Indeterminate`
        // regardless of what some other declaration claims elsewhere.
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: true }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_filter_a_matching_boolean_from_a_500_is_indeterminate_not_verified() {
        // A capability claim needs the endpoint's own successful answer. A 500
        // body that happens to carry {"boolean": true} is not the engine
        // confirming the function is bound.
        let mut o = obs(Some(true));
        o.status = Some(500);
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_true_with_no_expectation_from_a_503_is_indeterminate() {
        let mut o = obs(Some(true));
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn a_matching_boolean_from_a_2xx_is_still_a_capability_claim() {
        // The gate must not swallow the ordinary success case.
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn service_description_grading_separates_stub_from_substance() {
        // 21 of 28 descriptions in the wild are the same 14-triple Virtuoso
        // stub, so a stub must not score the same as a real description.
        assert_eq!(grade_service_description(14, false, false, false, false, false), Level(1));
        assert_eq!(grade_service_description(40, true, false, false, false, false), Level(2));
        assert_eq!(grade_service_description(7077, true, true, false, false, false), Level(3));
        assert_eq!(grade_service_description(7140, true, true, true, false, false), Level(4));
        assert_eq!(grade_service_description(0, false, false, false, false, false), Level(0));
    }

    #[test]
    fn extension_functions_and_example_resources_reach_level_four_like_entailment() {
        // Spec line 433: level 4 is "declares an entailment regime, example
        // resources, or extension functions". Only `has_entailment` was
        // checked, so a description declaring extension functions graded 1,
        // the same as a stub. That is precisely the rare honest declarer this
        // ladder exists to reward: 0 of 28 surveyed descriptions declared
        // anything geospatial, so when one finally does it must be credited.
        let mut fns = Declarations { triples: 20, ..Declarations::empty() };
        fns.extension_functions.insert("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        assert_eq!(grade_from_declarations(&fns), Level(4));

        let examples = Declarations { triples: 20, has_example_resources: true, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&examples), Level(4));
    }

    #[test]
    fn a_stock_sd_feature_is_not_a_level_four_declaration() {
        // `sd:feature` is exactly what the 14-triple Virtuoso stub carries, so
        // widening level 4 must not sweep the stub up with it.
        let mut stub = Declarations { triples: 14, ..Declarations::empty() };
        stub.features.insert("http://www.w3.org/ns/sparql-service-description#UnionDefaultGraph".into());
        assert_eq!(grade_from_declarations(&stub), Level(1));
    }

    #[test]
    fn the_ladder_is_monotonic_in_every_declaration() {
        // A richer description must never grade lower than a poorer one, or
        // the number stops being readable as informativeness. Exhaustive over
        // all 32 combinations of the five declaration flags at a fixed
        // non-zero triple count: adding any one flag can only raise the grade.
        let grade = |bits: u8| {
            grade_service_description(
                20,
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
                bits & 16 != 0,
            )
            .0
        };
        for bits in 0u8..32 {
            for flag in [1u8, 2, 4, 8, 16] {
                if bits & flag != 0 {
                    continue;
                }
                assert!(
                    grade(bits | flag) >= grade(bits),
                    "adding flag {flag} to {bits} lowered the grade from {} to {}",
                    grade(bits),
                    grade(bits | flag)
                );
            }
        }
    }

    #[test]
    fn level_new_at_the_lower_boundary_is_some() {
        // The brief's tests cover the upper boundary (4, and the rejected 5)
        // but never the lower one.
        assert!(Level::new(0).is_some());
    }

    #[test]
    fn a_transport_error_is_indeterminate_not_absent() {
        // Only the `Html` half of the early guard was pinned before; this
        // covers the `error.is_some()` half.
        let mut o = obs(None);
        o.error = Some("connection reset".into());
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_false_with_no_expectation_is_absent() {
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false }, Ok(&obs(Some(false))));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn cors_header_present_is_undeclared_but_verified() {
        // cors: true by default in `obs`, whatever the status.
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false }, Ok(&obs(None)));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn cors_header_absent_with_a_successful_status_is_absent() {
        let mut o = obs(None);
        o.cors = false;
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn cors_header_absent_with_a_503_is_indeterminate() {
        // A proxy-generated error status cannot be attributed to the
        // endpoint's own CORS configuration.
        let mut o = obs(None);
        o.cors = false;
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::Cors, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn liveness_sparql_json_is_verified() {
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false }, Ok(&obs(None)));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn liveness_other_body_is_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn select_iris_with_bindings_is_verified() {
        let mut o = obs(None);
        o.bindings = vec!["http://example.org/x".into()];
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Verified);
    }

    #[test]
    fn select_iris_empty_bindings_from_a_parsed_result_is_absent() {
        // body_kind SparqlJson and status 200 by default in `obs`.
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false }, Ok(&obs(None)));
        assert_eq!(v, Verdict::Absent);
    }

    #[test]
    fn select_iris_empty_bindings_from_an_unparsed_body_is_indeterminate() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::SelectIris, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn fetch_well_known_unparsed_body_unclaimed_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn liveness_a_429_is_indeterminate_not_absent() {
        // A throttled endpoint is emphatically alive. Reporting availability
        // as `Absent` here would tombstone it as unreachable, on the one
        // metric a later stage reads to decide whether to admit it at all.
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        o.status = Some(429);
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn liveness_a_503_from_an_intermediary_is_indeterminate_not_absent() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        o.status = Some(503);
        let v = resolve(&def(ProbeKind::Liveness, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_data_false_with_a_non_2xx_status_is_indeterminate_not_absent() {
        // An error body that happens to parse as SPARQL JSON with no bindings
        // is not evidence that the data is missing.
        let mut o = obs(Some(false));
        o.status = Some(500);
        let v = resolve(&def(ProbeKind::AskData, None), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn ask_filter_wrong_boolean_with_a_non_2xx_status_is_indeterminate() {
        // `DeclaredButWrong` is ranked worse than absent, so minting it from a
        // response the endpoint never authored is the worst available error.
        let mut o = obs(Some(false));
        o.status = Some(502);
        let v = resolve(&def(ProbeKind::AskFilter, Some(true)), Declared { claimed: false }, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn a_declaration_the_endpoint_actually_makes_yields_verified() {
        let mut d = Declarations::empty();
        d.features.insert("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::Verified, "declared and working is Verified");
    }

    #[test]
    fn working_but_undeclared_stays_undeclared_even_with_declarations_wired() {
        // This is the expected outcome for almost every real endpoint: 18
        // evaluate geof:sfWithin and none of them declares it. Wiring
        // declarations must NOT change these to Verified.
        let d = Declarations::empty();
        let mut def = def(ProbeKind::AskFilter, Some(true));
        def.declared_by = Some("http://www.opengis.net/def/function/geosparql/sfWithin".into());
        let v = resolve(&def, Declared::from(&d, &def), Ok(&obs(Some(true))));
        assert_eq!(v, Verdict::UndeclaredButVerified);
    }

    #[test]
    fn a_metric_no_declaration_can_speak_for_is_never_claimed() {
        let mut d = Declarations::empty();
        d.features.insert("http://example.org/anything".into());
        let def = def(ProbeKind::Cors, None); // declared_by is None
        assert!(!Declared::from(&d, &def).claimed);
    }

    #[test]
    fn a_fetched_stub_grades_low_and_a_substantial_one_grades_high() {
        let stub = Declarations { triples: 14, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&stub), Level(1));
        let rich = Declarations { triples: 7077, names_dataset: true, has_void_partitions: true, ..Declarations::empty() };
        assert_eq!(grade_from_declarations(&rich), Level(3));
    }

    #[test]
    fn a_fetched_rdf_description_is_verified_with_a_level() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Verified);
        assert_eq!(level, Some(Level(1)));
    }

    #[test]
    fn an_rdf_body_from_a_429_is_indeterminate_not_verified() {
        // The arm that returns `Verified` on a parsed RDF body used to run
        // before the status match, so any status with an RDF-ish payload
        // published a confident `verified` plus a graded level. A throttle
        // notice is the case where we know least about what is published.
        let mut o = obs(None);
        o.status = Some(429);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None, "an ungated grade is a confident wrong answer");
    }

    #[test]
    fn an_rdf_body_from_a_500_or_503_is_indeterminate_not_verified() {
        for status in [500, 502, 503] {
            let mut o = obs(None);
            o.status = Some(status);
            o.body_kind = BodyKind::Rdf;
            let d = Declarations { triples: 40, names_dataset: true, ..Declarations::empty() };
            let (v, level) = resolve_fetch(&d, Ok(&o));
            assert_eq!(v, Verdict::Indeterminate, "status {status}");
            assert_eq!(level, None, "status {status}");
        }
    }

    #[test]
    fn an_rdf_body_from_a_404_is_absent_with_level_zero_not_verified() {
        // A non-2xx with an RDF body must fall through to the status match,
        // where 404 keeps its genuine absence rather than being overtaken by
        // the body classification.
        let mut o = obs(None);
        o.status = Some(404);
        o.body_kind = BodyKind::Rdf;
        let d = Declarations { triples: 14, ..Declarations::empty() };
        let (v, level) = resolve_fetch(&d, Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_404_on_the_description_is_absent_not_indeterminate() {
        // A 404 is a real answer: the endpoint served us, and there is no
        // description there. That is one of the few honest absences.
        let mut o = obs(None);
        o.status = Some(404);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_410_on_the_description_is_absent_not_indeterminate() {
        // Gone is the other genuine absence: it was published and has since
        // been withdrawn.
        let mut o = obs(None);
        o.status = Some(410);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Absent);
        assert_eq!(level, Some(Level(0)));
    }

    #[test]
    fn a_400_from_a_queryless_get_rejection_is_indeterminate_not_absent() {
        // 10 of 548 surveyed endpoints return a plain 400 to exactly the
        // queryless GET this probe makes: the server rejected the shape of
        // the request, which says nothing about whether a description is
        // published there. Treating 400 as absence would falsely tombstone
        // every one of them.
        let mut o = obs(None);
        o.status = Some(400);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }

    #[test]
    fn a_429_throttle_is_indeterminate_not_absent() {
        // Being throttled is the one situation where we know least about
        // what exists; it must not resolve to the same verdict as knowing
        // for certain that nothing is published.
        let mut o = obs(None);
        o.status = Some(429);
        o.body_kind = BodyKind::Other;
        let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }

    #[test]
    fn a_401_or_403_on_the_description_is_indeterminate_not_absent() {
        // Refused is not the same as nothing being there: a real, well-formed
        // description can sit behind auth.
        for status in [401, 403] {
            let mut o = obs(None);
            o.status = Some(status);
            o.body_kind = BodyKind::Other;
            let (v, level) = resolve_fetch(&Declarations::empty(), Ok(&o));
            assert_eq!(v, Verdict::Indeterminate, "status {status}");
            assert_eq!(level, None, "status {status}");
        }
    }

    #[test]
    fn an_unparseable_description_body_is_indeterminate() {
        let mut o = obs(None);
        o.status = Some(200);
        o.body_kind = BodyKind::Other;
        let (v, _) = resolve_fetch(&Declarations::empty(), Ok(&o));
        assert_eq!(v, Verdict::Indeterminate);
    }

    #[test]
    fn an_expired_fetch_is_indeterminate_with_no_level() {
        let (v, level) = resolve_fetch(&Declarations::empty(), Err(Expired));
        assert_eq!(v, Verdict::Indeterminate);
        assert_eq!(level, None);
    }
}
