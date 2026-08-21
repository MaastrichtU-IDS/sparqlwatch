use crate::metrics::Cost;
use crate::verdict::{Level, Verdict};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfSerializer};
use std::collections::BTreeSet;

const DQV: &str = "http://www.w3.org/ns/dqv#";
const PROV: &str = "http://www.w3.org/ns/prov#";
const DCAT: &str = "http://www.w3.org/ns/dcat#";

pub struct RunId(pub String);

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
}

impl NotMeasuredReason {
    /// The published slug. Stable: it goes into the graph.
    pub fn slug(&self) -> &'static str {
        match self {
            NotMeasuredReason::CostCeiling => "cost-ceiling",
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
pub struct NotMeasured {
    pub endpoint: String,
    pub metric_id: String,
    pub reason: NotMeasuredReason,
}

fn nn(s: &str) -> anyhow::Result<NamedNode> {
    Ok(NamedNode::new(s)?)
}

/// One named graph per run keeps history immutable and lets a bad run be
/// dropped wholesale.
///
/// `metric_revision` identifies the metric definitions the run used. The spec
/// requires a run to record both it and the prober version, because a
/// measurement is only interpretable against the definition that produced it.
/// It must be a pure function of the definitions (see
/// `metrics::definitions_revision`), never a clock or a counter, so that
/// re-running the same definitions yields the same revision.
///
/// `max_cost` is the ceiling the sweep was run with, recorded on the run's
/// activity. It is a parameter rather than something this function discovers:
/// `emit_nquads` reads no clock, no environment and no global, so the same
/// inputs always produce the same document.
pub fn emit_nquads(
    run: &RunId,
    generated_at: &str,
    metric_revision: &str,
    rows: &[MeasurementRow],
    declarations_read: &[DeclarationsRead],
    not_measured: &[NotMeasured],
    max_cost: Cost,
) -> anyhow::Result<String> {
    let graph = GraphName::NamedNode(nn(&format!("urn:sparqlwatch:run:{}", run.0))?);
    let activity = nn(&format!("urn:sparqlwatch:activity:{}", run.0))?;
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
    // that ran it, and the not-measured facts below say which metrics were
    // declined but not what policy declined them.
    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn("urn:sparqlwatch:maxCost")?,
        Term::Literal(Literal::new_simple_literal(max_cost.slug())),
        graph.clone(),
    ));

    let mut typed_endpoints: BTreeSet<String> = BTreeSet::new();

    for (i, r) in rows.iter().enumerate() {
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
        let m = nn(&format!("urn:sparqlwatch:measurement:{}:{}", run.0, i))?;
        let subj = NamedOrBlankNode::NamedNode(m.clone());

        if typed_endpoints.insert(r.endpoint.clone()) {
            // The endpoint is the same resource across every metric, so it is
            // typed once per run rather than once per measurement.
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
    // lands on a node that both has and has not a verdict. It also does not
    // reuse the row counter above, so the two cannot collide by arithmetic
    // accident when one of the lists is empty.
    for (i, fact) in not_measured.iter().enumerate() {
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
        let subj = NamedOrBlankNode::NamedNode(nn(&format!(
            "urn:sparqlwatch:not-measured:{}:{}",
            run.0, i
        ))?);
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
        // No `dqv:value` and no `sw:level`: nothing was measured, so there is
        // nothing to state. The reason is all the new information there is.
        quads.push(Quad::new(
            subj,
            nn("urn:sparqlwatch:notMeasuredReason")?,
            Term::Literal(Literal::new_simple_literal(fact.reason.slug())),
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

    let mut out = Vec::new();
    let mut ser = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for q in &quads {
        ser.serialize_quad(q.as_ref())?;
    }
    ser.finish()?;
    Ok(String::from_utf8(out)?)
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, rows, &[], &[], Cost::Cheap).unwrap();
        quads_of(&out)
    }

    fn objects<'a>(qs: &'a [Quad], predicate: &str) -> Vec<&'a Term> {
        qs.iter().filter(|q| q.predicate.as_str() == predicate).map(|q| &q.object).collect()
    }

    #[test]
    fn every_quad_lands_in_the_run_graph() {
        let out = emit_nquads(&RunId("2026-08-20T08:00:00Z".into()), "2026-08-20T08:00:00Z", REV, &rows(), &[], &[], Cost::Cheap).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &rs, &[], &[], Cost::Cheap)
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &[], &facts, &[], Cost::Cheap).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &[], &facts, &[], Cost::Cheap).unwrap();
        let qs = quads_of(&out);
        let read_quads: Vec<&Quad> =
            qs.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead").collect();
        assert_eq!(read_quads.len(), 2, "one quad per endpoint, whatever the boolean");
    }

    #[test]
    fn a_not_measured_fact_carries_no_verdict_and_no_level() {
        let nq = emit_nquads(&RunId(AT.into()), AT, REV, &[], &[], &[NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }], Cost::Cheap).unwrap();

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
        let nq = emit_nquads(&RunId(AT.into()), AT, REV, &rows, &[], &nm, Cost::Cheap).unwrap();

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
        // The four quads the spec asks for, read back as quads rather than as
        // text: a consumer asking "why is there no verdict for classes here"
        // must be able to join endpoint and metric.
        let nm = vec![NotMeasured {
            endpoint: "http://example.org/sparql".into(),
            metric_id: "classes".into(),
            reason: NotMeasuredReason::CostCeiling,
        }];
        let out = emit_nquads(&RunId("r1".into()), AT, REV, &[], &[], &nm, Cost::Cheap).unwrap();
        let qs = quads_of(&out);
        let subj = NamedOrBlankNode::NamedNode(
            NamedNode::new("urn:sparqlwatch:not-measured:r1:0").unwrap(),
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
        let out = emit_nquads(&RunId("r1".into()), AT, REV, &rows(), &[], &nm, Cost::Cheap).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), AT, REV, &[], &[], &nm, Cost::Cheap).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), AT, REV, &[], &[], &nm, Cost::Cheap)
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
            let out = emit_nquads(&RunId("r1".into()), AT, REV, &rows(), &[], &[], ceiling).unwrap();
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
}
