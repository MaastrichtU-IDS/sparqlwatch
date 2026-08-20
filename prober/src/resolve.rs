use crate::budget::Expired;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::{BodyKind, Observation};
use crate::verdict::{Level, Verdict};

/// What the endpoint says about itself, from its service description.
#[derive(Debug, Clone, Copy)]
pub struct Declared {
    pub claimed: bool,
}

/// Turn observation plus declaration into a verdict. This is the only place
/// judgement happens: no probing, no I/O, pure function.
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
            (Some(got), Some(want)) if got == want => {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            }
            // Bound but wrong: the function answered, and answered incorrectly.
            (Some(_), Some(_)) => Verdict::DeclaredButWrong,
            (Some(true), None) => {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            }
            (Some(false), None) => Verdict::Absent,
            (None, _) => {
                if declared.claimed { Verdict::DeclaredOnly } else { Verdict::Indeterminate }
            }
        },
        ProbeKind::Cors => {
            if o.cors {
                if declared.claimed { Verdict::Verified } else { Verdict::UndeclaredButVerified }
            } else {
                Verdict::Absent
            }
        }
        ProbeKind::Liveness => {
            if o.body_kind == BodyKind::SparqlJson { Verdict::Verified } else { Verdict::Absent }
        }
        ProbeKind::SelectIris => {
            if !o.bindings.is_empty() { Verdict::Verified } else { Verdict::Absent }
        }
        ProbeKind::FetchWellKnown => {
            if o.body_kind == BodyKind::SparqlJson || !o.bindings.is_empty() {
                Verdict::Verified
            } else if declared.claimed {
                Verdict::DeclaredOnly
            } else {
                Verdict::Absent
            }
        }
    }
}

/// Grade a service description by what it actually tells a client, not by its
/// presence. 21 of 28 descriptions in the wild are the same 14-triple
/// Virtuoso stub, so a stub must not score the same as a real description.
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
    use crate::budget::Expired;
    use crate::metrics::{MetricDef, ProbeKind};
    use crate::observe::{BodyKind, Observation};
    use crate::verdict::{Level, Verdict};

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
        Observation { status: Some(200), cors: true, boolean, bindings: vec![], body_kind: BodyKind::SparqlJson, elapsed_ms: 5, error: None }
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
}
