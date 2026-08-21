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
pub fn emit_nquads(
    run: &RunId,
    generated_at: &str,
    metric_revision: &str,
    rows: &[MeasurementRow],
    declarations_read: &[DeclarationsRead],
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
    use crate::verdict::{Level, Verdict};
    use oxrdfio::RdfParser;

    const REV: &str = "abc123";

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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, rows, &[]).unwrap();
        quads_of(&out)
    }

    fn objects<'a>(qs: &'a [Quad], predicate: &str) -> Vec<&'a Term> {
        qs.iter().filter(|q| q.predicate.as_str() == predicate).map(|q| &q.object).collect()
    }

    #[test]
    fn every_quad_lands_in_the_run_graph() {
        let out = emit_nquads(&RunId("2026-08-20T08:00:00Z".into()), "2026-08-20T08:00:00Z", REV, &rows(), &[]).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &rs, &[])
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &[], &facts).unwrap();
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
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", REV, &[], &facts).unwrap();
        let qs = quads_of(&out);
        let read_quads: Vec<&Quad> =
            qs.iter().filter(|q| q.predicate.as_str() == "urn:sparqlwatch:declarationsRead").collect();
        assert_eq!(read_quads.len(), 2, "one quad per endpoint, whatever the boolean");
    }
}
