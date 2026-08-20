use crate::verdict::{Level, Verdict};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfSerializer};

const DQV: &str = "http://www.w3.org/ns/dqv#";
const PROV: &str = "http://www.w3.org/ns/prov#";

pub struct RunId(pub String);

pub struct MeasurementRow {
    pub endpoint: String,
    pub metric_id: String,
    pub verdict: Verdict,
    pub level: Option<Level>,
    pub elapsed_ms: u64,
}

fn nn(s: &str) -> anyhow::Result<NamedNode> {
    Ok(NamedNode::new(s)?)
}

/// One named graph per run keeps history immutable and lets a bad run be
/// dropped wholesale.
pub fn emit_nquads(run: &RunId, generated_at: &str, rows: &[MeasurementRow]) -> anyhow::Result<String> {
    let graph = GraphName::NamedNode(nn(&format!("urn:sparqlwatch:run:{}", run.0))?);
    let activity = nn(&format!("urn:sparqlwatch:activity:{}", run.0))?;
    let mut quads: Vec<Quad> = Vec::new();

    quads.push(Quad::new(
        NamedOrBlankNode::NamedNode(activity.clone()),
        nn(&format!("{PROV}generatedAtTime"))?,
        Term::Literal(Literal::new_typed_literal(generated_at, xsd::DATE_TIME)),
        graph.clone(),
    ));

    for (i, r) in rows.iter().enumerate() {
        let m = nn(&format!("urn:sparqlwatch:measurement:{}:{}", run.0, i))?;
        let subj = NamedOrBlankNode::NamedNode(m.clone());

        quads.push(Quad::new(
            subj.clone(),
            nn(&format!("{DQV}computedOn"))?,
            Term::NamedNode(nn(&r.endpoint)?),
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
        quads.push(Quad::new(
            subj.clone(),
            nn("urn:sparqlwatch:elapsedMs")?,
            Term::Literal(Literal::new_typed_literal(r.elapsed_ms.to_string(), xsd::INTEGER)),
            graph.clone(),
        ));
        if let Some(Level(l)) = r.level {
            quads.push(Quad::new(
                subj,
                nn("urn:sparqlwatch:level")?,
                Term::Literal(Literal::new_typed_literal(l.to_string(), xsd::INTEGER)),
                graph.clone(),
            ));
        }
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

    fn rows() -> Vec<MeasurementRow> {
        vec![
            MeasurementRow {
                endpoint: "https://qlever.dev/api/osm-planet".into(),
                metric_id: "geo-functions".into(),
                verdict: Verdict::UndeclaredButVerified,
                level: None,
                elapsed_ms: 210,
            },
            MeasurementRow {
                endpoint: "https://data.kkg.kadaster.nl/query".into(),
                metric_id: "service-description".into(),
                verdict: Verdict::DeclaredOnly,
                level: Some(Level(2)),
                elapsed_ms: 5714,
            },
        ]
    }

    #[test]
    fn every_quad_lands_in_the_run_graph() {
        let out = emit_nquads(&RunId("2026-08-20T08:00:00Z".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.contains("urn:sparqlwatch:run:2026-08-20T08:00:00Z"),
                    "quad outside the run graph: {line}");
        }
    }

    #[test]
    fn measurements_carry_dqv_and_prov_terms() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        assert!(out.contains("http://www.w3.org/ns/dqv#isMeasurementOf"));
        assert!(out.contains("http://www.w3.org/ns/dqv#computedOn"));
        assert!(out.contains("http://www.w3.org/ns/dqv#value"));
        assert!(out.contains("http://www.w3.org/ns/prov#generatedAtTime"));
        assert!(out.contains("http://www.w3.org/ns/prov#wasGeneratedBy"));
    }

    #[test]
    fn the_verdict_is_written_as_its_slug() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        assert!(out.contains("\"undeclared-but-verified\""));
    }

    #[test]
    fn a_graded_level_is_emitted_only_when_present() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        // one row has a level, the other does not
        assert_eq!(out.matches("urn:sparqlwatch:level").count(), 1);
    }

    #[test]
    fn output_is_valid_nquads() {
        let out = emit_nquads(&RunId("r1".into()), "2026-08-20T08:00:00Z", &rows()).unwrap();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.trim_end().ends_with(" ."), "not an N-Quad: {line}");
        }
    }
}
