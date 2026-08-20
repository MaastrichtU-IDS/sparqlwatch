use serde::{Deserialize, Serialize};

/// The closed set of probe kinds. A metric definition names one of these plus
/// its parameters, which is what makes metrics data rather than code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    Liveness,
    Cors,
    /// A data-free filter, testing whether a function is bound.
    AskFilter,
    /// An ASK over data, testing presence. Uses the literal guard.
    AskData,
    SelectIris,
    FetchWellKnown,
}

impl ProbeKind {
    /// Whether a probe is actually implemented for this kind. `FetchWellKnown`
    /// is defined but not yet built (the fetch probe is a later stage), and a
    /// kind with no probe must be skipped *without issuing a request*: the
    /// generic query path would send `GET <endpoint>?query=`, a malformed
    /// protocol request that learns nothing and looks like abuse to the
    /// operator whose logs it lands in. The metric still gets a row, recorded
    /// as `Indeterminate`, so the gap stays visible in the published output.
    pub fn has_probe(&self) -> bool {
        match self {
            ProbeKind::Liveness
            | ProbeKind::Cors
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris => true,
            ProbeKind::FetchWellKnown => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDef {
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub kind: ProbeKind,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub expect: Option<bool>,
    /// The SPARQL variable the query binds, for probe kinds that read
    /// bindings back out of the result (e.g. `AskData`, `SelectIris`).
    /// Absent for kinds that don't read bindings.
    #[serde(default)]
    pub var: Option<String>,
    #[serde(default)]
    pub graded: bool,
}

#[derive(Deserialize)]
struct MetricFile {
    metric: Vec<MetricDef>,
}

/// Parse metric definitions out of TOML. An unrecognized `kind` is a loud
/// parse error, never a silent default, because the probe-kind set is closed.
pub fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>> {
    let f: MetricFile = toml::from_str(toml_src)?;
    Ok(f.metric)
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
    }

    #[test]
    fn fetch_well_known_has_no_probe_yet_and_every_other_kind_does() {
        assert!(!ProbeKind::FetchWellKnown.has_probe());
        for k in [ProbeKind::Liveness, ProbeKind::Cors, ProbeKind::AskFilter,
                  ProbeKind::AskData, ProbeKind::SelectIris] {
            assert!(k.has_probe(), "{k:?} should have a probe");
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
    fn the_var_field_round_trips_and_defaults_to_none() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo_data = ms.iter().find(|m| m.id == "geo-data").unwrap();
        assert_eq!(geo_data.var, Some("g".to_string()));
        let cors = ms.iter().find(|m| m.id == "cors").unwrap();
        assert_eq!(cors.var, None);
    }
}
