use crate::budget::Expired;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::{BodyKind, Observation};
use crate::verdict::{Level, Verdict};

/// What the endpoint says about itself, from its service description.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub claimed: bool,
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
        ProbeKind::FetchWellKnown => {
            // A real `.well-known` service description is RDF, which this
            // client classifies as `BodyKind::Other`, so the `Verified` arm
            // below is not reachable from a genuine document until a later
            // stage adds a fetch probe that reports parsed RDF evidence.
            // Until then, `Absent` is not achievable for this probe kind at
            // all: an unparsed or unclaimed well-known document is honestly
            // `Indeterminate`, never a confirmed absence.
            if o.body_kind == BodyKind::SparqlJson || !o.bindings.is_empty() {
                Verdict::Verified
            } else if declared.claimed {
                Verdict::DeclaredOnly
            } else {
                Verdict::Indeterminate
            }
        }
    }
}

/// Grade a service description by what it actually tells a client, not by its
/// presence. 21 of 28 descriptions in the wild are the same 14-triple
/// Virtuoso stub, so a stub must not score the same as a real description.
///
/// **Currently unreachable from any production path.** Nothing produces the
/// four booleans it takes: there is no `.well-known` fetch probe yet, no
/// `BodyKind::Rdf` to recognise a description body, and `MeasurementRow.level`
/// is always `None`, so no level is ever emitted. This is blocked on that fetch
/// probe rather than delivered, and is kept (with its tests) because the
/// grading rule is the part that was worth pinning down from the survey.
pub fn grade_service_description(
    triples: usize,
    names_dataset: bool,
    has_void_partitions: bool,
    has_entailment: bool,
) -> Level {
    let n: u8 = if triples == 0 {
        0
    } else if has_entailment {
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
            graded: false,
        }
    }

    fn obs(boolean: Option<bool>) -> Observation {
        Observation { status: Some(200), cors: true, boolean, bindings: vec![], body_kind: BodyKind::SparqlJson, body: None, elapsed_ms: 5, error: None }
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
    fn claimed_but_unprobeable_is_declared_only() {
        let mut o = obs(None);
        o.body_kind = BodyKind::Other;
        let v = resolve(&def(ProbeKind::FetchWellKnown, None), Declared { claimed: true }, Ok(&o));
        assert_eq!(v, Verdict::DeclaredOnly);
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
        assert_eq!(grade_service_description(14, false, false, false), Level(1));
        assert_eq!(grade_service_description(40, true, false, false), Level(2));
        assert_eq!(grade_service_description(7077, true, true, false), Level(3));
        assert_eq!(grade_service_description(7140, true, true, true), Level(4));
        assert_eq!(grade_service_description(0, false, false, false), Level(0));
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
}
