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
    /// now has one too -- a single queryless fetch per endpoint, issued once
    /// in `probe_endpoint` ahead of this per-metric dispatch rather than
    /// through it -- so every current kind returns `true`. The mechanism
    /// stays for a future kind that has no probe yet: it must be skipped
    /// *without issuing a request*, since the generic query path would send
    /// `GET <endpoint>?query=`, a malformed protocol request that learns
    /// nothing and looks like abuse to the operator whose logs it lands in.
    /// Such a metric still gets a row, recorded as `Indeterminate`, so the
    /// gap stays visible in the published output.
    pub fn has_probe(&self) -> bool {
        match self {
            ProbeKind::Liveness
            | ProbeKind::Cors
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris
            | ProbeKind::FetchWellKnown => true,
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
    /// The IRI whose presence in the endpoint's own declarations means it
    /// claims this capability. Absent for metrics no declaration can speak
    /// for (liveness, response time, CORS headers).
    #[serde(default)]
    pub declared_by: Option<String>,
    #[serde(default)]
    pub graded: bool,
}

#[derive(Deserialize)]
struct MetricFile {
    metric: Vec<MetricDef>,
}

/// Parse metric definitions out of TOML. An unrecognized `kind` is a loud
/// parse error, never a silent default, because the probe-kind set is closed.
///
/// The same doctrine applies to a missing `var`: the two kinds that read a
/// variable's bindings back out of the result cannot work without knowing its
/// name, and guessing one silently turns a real capability into a false
/// `Absent`. That is a broken definition, so it fails here rather than
/// publishing one worthless measurement per endpoint.
pub fn load_metrics(toml_src: &str) -> anyhow::Result<Vec<MetricDef>> {
    let f: MetricFile = toml::from_str(toml_src)?;
    for m in &f.metric {
        if matches!(m.kind, ProbeKind::AskData | ProbeKind::SelectIris) && m.var.is_none() {
            anyhow::bail!(
                "metric '{}' of kind {:?} reads a variable's bindings but declares no `var`",
                m.id,
                m.kind
            );
        }
    }
    Ok(f.metric)
}

/// A stable identifier for a set of metric definitions, so a run can record
/// which revision produced its measurements. Deliberately a pure function of
/// the definitions themselves -- no clock, no counter, no build metadata -- so
/// re-running the same definitions yields the same revision, and any edit to
/// any field yields a different one.
///
/// FNV-1a over a canonical rendering, rather than `DefaultHasher`, whose
/// output Rust explicitly does not promise to keep stable across releases.
/// This value is published, so it has to outlive the toolchain.
pub fn definitions_revision(defs: &[MetricDef]) -> String {
    let mut canonical = String::new();
    for d in defs {
        // Order and field set are part of the revision: a reordered file is a
        // different definition list, and every field affects what is measured.
        canonical.push_str(&format!(
            "{}\x1f{}\x1f{}\x1f{:?}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1e",
            d.id,
            d.label,
            d.dimension,
            d.kind,
            d.query.as_deref().unwrap_or(""),
            d.expect.map(|b| b.to_string()).unwrap_or_default(),
            d.var.as_deref().unwrap_or(""),
            d.declared_by.as_deref().unwrap_or(""),
            d.graded,
        ));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
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
    fn every_probe_kind_has_a_probe() {
        // FetchWellKnown's probe is the once-per-endpoint fetch in
        // `probe_endpoint`, dispatched ahead of this generic per-metric path
        // rather than through it, but it is implemented now: no kind in the
        // closed set currently lacks one.
        for k in [ProbeKind::Liveness, ProbeKind::Cors, ProbeKind::AskFilter,
                  ProbeKind::AskData, ProbeKind::SelectIris, ProbeKind::FetchWellKnown] {
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
    fn a_binding_kind_with_no_var_is_a_load_error_not_a_silent_measurement() {
        // Same doctrine as the unknown `kind` above: a broken definition is a
        // loud parse error. Papering over it published N endpoints x one
        // Indeterminate measurement for a metric that could never work.
        for kind in ["AskData", "SelectIris"] {
            let bad = format!(
                r#"
[[metric]]
id = "x"
label = "x"
dimension = "d"
kind = "{kind}"
query = "SELECT ?thing WHERE {{ ?s ?p ?thing }} LIMIT 1"
"#
            );
            let err = load_metrics(&bad).expect_err("{kind} with no var must be rejected");
            let msg = err.to_string();
            assert!(msg.contains("var"), "the error must name the missing field: {msg}");
            assert!(msg.contains('x'), "the error must name the metric: {msg}");
        }
    }

    #[test]
    fn the_revision_is_a_pure_function_of_the_definitions() {
        let a = load_metrics(SRC).unwrap();
        assert_eq!(definitions_revision(&a), definitions_revision(&a), "no clock, no randomness");
        let b = load_metrics(&SRC.replace("ASK { }", "ASK { ?s ?p ?o }")).unwrap();
        assert_ne!(definitions_revision(&a), definitions_revision(&b), "an edited query is a new revision");
        let c = load_metrics(&SRC.replace(
            "kind = \"AskFilter\"\nexpect = true",
            "kind = \"AskFilter\"\nexpect = true\ndeclared_by = \"http://example.org/fn\"",
        ))
        .unwrap();
        assert_ne!(
            definitions_revision(&a),
            definitions_revision(&c),
            "an edited declared_by changes what the metric is read against, so it must be a new revision too"
        );
        let reordered: Vec<MetricDef> = a.iter().rev().cloned().collect();
        assert_ne!(definitions_revision(&a), definitions_revision(&reordered));
        assert!(definitions_revision(&a).starts_with("fnv1a64:"));
    }

    #[test]
    fn a_kind_that_reads_no_bindings_needs_no_var() {
        let ok = r#"
[[metric]]
id = "availability"
label = "answers a trivial query"
dimension = "availability"
kind = "Liveness"
query = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"
"#;
        assert!(load_metrics(ok).is_ok());
    }

    #[test]
    fn the_var_field_round_trips_and_defaults_to_none() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo_data = ms.iter().find(|m| m.id == "geo-data").unwrap();
        assert_eq!(geo_data.var, Some("g".to_string()));
        let cors = ms.iter().find(|m| m.id == "cors").unwrap();
        assert_eq!(cors.var, None);
    }

    #[test]
    fn a_metric_can_name_the_declaration_that_would_satisfy_it() {
        let ms = load_metrics(include_str!("../metrics.toml")).unwrap();
        let geo = ms.iter().find(|m| m.id == "geo-functions").unwrap();
        assert_eq!(
            geo.declared_by.as_deref(),
            Some("http://www.opengis.net/def/function/geosparql/sfWithin")
        );
        // Most metrics have no declaration that could speak for them.
        assert!(ms.iter().find(|m| m.id == "availability").unwrap().declared_by.is_none());
    }
}
