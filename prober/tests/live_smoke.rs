use sparqlwatch_prober::{budget::Budget, client::Client, metrics::load_metrics, politeness::Politeness, run_sweep};
use std::time::{Duration, Instant};

/// These two tests probe real third-party endpoints, so they get real
/// politeness rather than `Politeness::unlimited()`: the gap is exactly what
/// the rest of the suite is allowed to skip and a live run is not.
fn polite() -> Politeness {
    Politeness::new(Duration::from_secs(2))
}

/// Hits real third-party endpoints, so it is not part of the default run.
/// Enable with: cargo test --test live_smoke -- --ignored
#[tokio::test]
#[ignore]
async fn probes_three_real_endpoints() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default(), polite()).unwrap();
    let eps = vec![
        "https://data.kkg.kadaster.nl/query".to_string(),
        "https://ontop.certain.ai.ustp.at/sparql".to_string(),
    ];
    let (rows, declarations_read, _not_measured) = run_sweep(&eps, &defs, &[], &client, Budget::default()).await;
    for r in &rows {
        println!("{} {} -> {}", r.endpoint, r.metric_id, r.verdict.slug());
    }
    for f in &declarations_read {
        println!("{} declarationsRead -> {}", f.endpoint, f.read);
    }
    assert_eq!(rows.len(), eps.len() * defs.len());
    assert_eq!(declarations_read.len(), eps.len());
}

/// A structural test cannot show a query returns rows: `GRAPH ?g { ?s
/// geo:asWKT ?g }` parses, runs, and can never match, and only a real engine
/// tells the difference between that and a query that actually reaches named
/// graphs. Runs the shipped `geo-data` and `classes` queries, unmodified from
/// `metrics.toml`, against a real endpoint that is known to hold data (also
/// measured directly by hand: geo-data 0.26s, classes 15.6s, both with a
/// non-empty result), and asserts each returns a non-empty binding set.
///
/// What it does NOT establish: that the named-graph branch is reached.
/// kadaster behaves as a union-default-graph store, so its default graph
/// answers both branches on its own and this test would pass with the `GRAPH`
/// branch deleted. Proving that half needs an endpoint whose data really sits
/// in named graphs; see the named-graph entry in README's known limitations.
///
/// Enable with: cargo test --test live_smoke -- --ignored
#[tokio::test]
#[ignore]
async fn the_widened_content_queries_return_real_bindings() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default(), polite()).unwrap();
    let url = "https://data.kkg.kadaster.nl/query";

    let geo = defs.iter().find(|d| d.id == "geo-data").expect("geo-data must be shipped");
    let started = Instant::now();
    let o = client
        .ask_literal(url, geo.query.as_deref().unwrap(), geo.var.as_deref().unwrap())
        .await;
    println!(
        "geo-data: boolean={:?} bindings={} elapsed={:?}",
        o.boolean,
        o.bindings.len(),
        started.elapsed()
    );
    assert_eq!(o.boolean, Some(true), "kadaster is known to hold at least one WKT literal");
    assert!(!o.bindings.is_empty(), "geo-data must return a non-empty binding set against a real engine");

    let classes = defs.iter().find(|d| d.id == "classes").expect("classes must be shipped");
    let started = Instant::now();
    let o = client
        .select_iris(url, classes.query.as_deref().unwrap(), classes.var.as_deref().unwrap())
        .await;
    println!("classes: bindings={} elapsed={:?}", o.bindings.len(), started.elapsed());
    assert!(!o.bindings.is_empty(), "classes must return a non-empty binding set against a real engine");
}
