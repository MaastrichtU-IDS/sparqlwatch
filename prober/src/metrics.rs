use serde::{Deserialize, Serialize};

/// The closed set of probe kinds. A metric definition names one of these plus
/// its parameters, which is what makes metrics data rather than code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    Liveness,
    /// An `access-control-allow-origin` header on a simple GET: what a `curl`
    /// user sees.
    Cors,
    /// An `OPTIONS` preflight for a cross-origin GET: what a browser sees.
    /// Deliberately separate from `Cors`, because an endpoint can genuinely
    /// have one and not the other and neither subsumes the other.
    CorsPreflight,
    /// A data-free filter, testing whether a function is bound.
    AskFilter,
    /// An ASK over data, testing presence. Uses the literal guard.
    AskData,
    SelectIris,
    FetchWellKnown,
}

impl ProbeKind {
    /// Every probe kind, for tests that must cover the whole closed set.
    ///
    /// Kept honest by `sequence` below rather than by discipline: a
    /// hand-maintained list in a test looks like it enforces coverage and does
    /// not, which is the defect shape this crate keeps finding in itself.
    pub const ALL: [ProbeKind; 7] = [
        ProbeKind::Liveness,
        ProbeKind::Cors,
        ProbeKind::CorsPreflight,
        ProbeKind::AskFilter,
        ProbeKind::AskData,
        ProbeKind::SelectIris,
        ProbeKind::FetchWellKnown,
    ];

    /// This kind's position in `ALL`. The match is exhaustive with no catch-all,
    /// so adding a variant fails to compile here; the new variant then gets the
    /// next index, and `all_is_every_variant_in_order` fails until it is added
    /// to `ALL`. That chain is what makes `ALL` complete by construction.
    ///
    /// Test-only: it exists to be checked, not called. CI runs both `cargo test`
    /// and `cargo clippy --all-targets`, so the compile-time half of the guard
    /// still fires there.
    #[cfg(test)]
    fn sequence(self) -> usize {
        match self {
            ProbeKind::Liveness => 0,
            ProbeKind::Cors => 1,
            ProbeKind::CorsPreflight => 2,
            ProbeKind::AskFilter => 3,
            ProbeKind::AskData => 4,
            ProbeKind::SelectIris => 5,
            ProbeKind::FetchWellKnown => 6,
        }
    }

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
            | ProbeKind::CorsPreflight
            | ProbeKind::AskFilter
            | ProbeKind::AskData
            | ProbeKind::SelectIris
            | ProbeKind::FetchWellKnown => true,
        }
    }
}

/// What a metric costs the endpoint we point it at. A closed set, like
/// `ProbeKind`: an unknown value is a load error, because guessing silently
/// changes what a sweep costs somebody else's server.
///
/// `Cheap` means the query can stop at its first match. `Expensive` means it
/// forces a scan. The line is not a guess: measured on qlever.dev's
/// planet-scale OSM endpoint, the same class query answers in 0.166s with
/// `LIMIT 1` and no `DISTINCT`, and times out past 45s with
/// `DISTINCT ... LIMIT 200`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Cost {
    #[default]
    Cheap,
    Expensive,
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
    /// What this metric costs the endpoint it points at. Silent about cost
    /// means cheap, but an unrecognized value is a load error, not a guess.
    #[serde(default)]
    pub cost: Cost,
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

/// Split `defs` into those to run against a sweep whose ceiling is
/// `ceiling`, and those declined because their cost exceeds it. Both halves
/// preserve the input order: a reordered result would make "declined"
/// harder to line up against the file that declared it.
pub fn within_cost(defs: &[MetricDef], ceiling: Cost) -> (Vec<MetricDef>, Vec<MetricDef>) {
    let mut run = Vec::new();
    let mut declined = Vec::new();
    for d in defs {
        let within = match ceiling {
            Cost::Cheap => d.cost == Cost::Cheap,
            Cost::Expensive => true,
        };
        if within {
            run.push(d.clone());
        } else {
            declined.push(d.clone());
        }
    }
    (run, declined)
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
            "{}\x1f{}\x1f{}\x1f{:?}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{:?}\x1e",
            d.id,
            d.label,
            d.dimension,
            d.kind,
            d.query.as_deref().unwrap_or(""),
            d.expect.map(|b| b.to_string()).unwrap_or_default(),
            d.var.as_deref().unwrap_or(""),
            d.declared_by.as_deref().unwrap_or(""),
            d.graded,
            d.cost,
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
        // The two CORS facts are both shipped, and they are different kinds.
        // A `cors-preflight` metric accidentally defined as `kind = "Cors"`
        // would publish the simple-GET header under the preflight's label,
        // which is the exact mislabelling this metric exists to end.
        assert_eq!(ms.iter().find(|m| m.id == "cors").unwrap().kind, ProbeKind::Cors);
        let preflight = ms.iter().find(|m| m.id == "cors-preflight").expect("cors-preflight must be shipped");
        assert_eq!(preflight.kind, ProbeKind::CorsPreflight);
        // A preflight carries no query, and no declaration speaks for a CORS
        // policy, so neither field may be set on it.
        assert_eq!(preflight.query, None);
        assert_eq!(preflight.declared_by, None);
    }

    #[test]
    fn every_probe_kind_has_a_probe() {
        // FetchWellKnown's probe is the once-per-endpoint fetch in
        // `probe_endpoint`, dispatched ahead of this generic per-metric path
        // rather than through it, but it is implemented now: no kind in the
        // closed set currently lacks one.
        for k in ProbeKind::ALL {
            assert!(k.has_probe(), "{k:?} should have a probe");
        }
    }

    #[test]
    fn all_is_every_variant_in_order() {
        // The guard that makes `ProbeKind::ALL` trustworthy. Adding a variant
        // breaks `sequence`'s exhaustive match at compile time, and then this
        // fails until the variant is in `ALL` at its own index. Without it,
        // `ALL` is just another hand-maintained list that a new kind can slip
        // past, and every test iterating it would silently stop covering the
        // set it claims to cover.
        for (i, k) in ProbeKind::ALL.into_iter().enumerate() {
            assert_eq!(k.sequence(), i, "{k:?} is at the wrong index in ALL");
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

        let d = load_metrics(&SRC.replace(
            "kind = \"AskFilter\"\nexpect = true",
            "kind = \"AskFilter\"\nexpect = true\ncost = \"expensive\"",
        ))
        .unwrap();
        assert_ne!(
            definitions_revision(&a),
            definitions_revision(&d),
            "an edited cost changes which metrics a sweep runs, so it must be a new revision too"
        );
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

    #[test]
    fn a_metric_without_a_cost_is_cheap() {
        let defs = load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\n"
        ).unwrap();
        assert_eq!(defs[0].cost, Cost::Cheap, "a definition silent about cost is cheap");
    }

    #[test]
    fn an_unknown_cost_is_a_load_error_not_a_silent_default() {
        // Same doctrine as an unknown `kind`: the set is closed, and guessing
        // silently changes what a sweep costs somebody else's server.
        assert!(load_metrics(
            "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"free\"\n"
        ).is_err());
    }

    #[test]
    fn within_cost_splits_and_keeps_order() {
        let defs = load_metrics(concat!(
            "[[metric]]\nid=\"a\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
            "[[metric]]\nid=\"b\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n",
            "[[metric]]\nid=\"c\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n",
        )).unwrap();

        let (run, declined) = within_cost(&defs, Cost::Cheap);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(declined.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["b"]);

        let (run, declined) = within_cost(&defs, Cost::Expensive);
        assert_eq!(run.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"],
                   "the higher ceiling runs everything, still in file order");
        assert!(declined.is_empty());
    }

    #[test]
    fn cost_is_part_of_the_definitions_revision() {
        // The revision exists so a measurement can be read against the definition
        // that produced it, and cost changes which metrics run at all.
        let one = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"cheap\"\n";
        let two = "[[metric]]\nid=\"m\"\nlabel=\"l\"\ndimension=\"d\"\nkind=\"Liveness\"\nquery=\"ASK{}\"\ncost=\"expensive\"\n";
        assert_ne!(
            definitions_revision(&load_metrics(one).unwrap()),
            definitions_revision(&load_metrics(two).unwrap())
        );
    }
}
