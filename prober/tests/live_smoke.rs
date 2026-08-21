use sparqlwatch_prober::{budget::Budget, client::Client, metrics::load_metrics, run_sweep};

/// Hits real third-party endpoints, so it is not part of the default run.
/// Enable with: cargo test --test live_smoke -- --ignored
#[tokio::test]
#[ignore]
async fn probes_three_real_endpoints() {
    let defs = load_metrics(include_str!("../metrics.toml")).unwrap();
    let client = Client::new(Budget::default()).unwrap();
    let eps = vec![
        "https://data.kkg.kadaster.nl/query".to_string(),
        "https://ontop.certain.ai.ustp.at/sparql".to_string(),
    ];
    let (rows, declarations_read) = run_sweep(&eps, &defs, &client, Budget::default()).await;
    for r in &rows {
        println!("{} {} -> {}", r.endpoint, r.metric_id, r.verdict.slug());
    }
    for f in &declarations_read {
        println!("{} declarationsRead -> {}", f.endpoint, f.read);
    }
    assert_eq!(rows.len(), eps.len() * defs.len());
    assert_eq!(declarations_read.len(), eps.len());
}
