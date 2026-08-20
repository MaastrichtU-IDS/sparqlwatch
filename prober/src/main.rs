use clap::Parser;
use sparqlwatch_prober::{
    budget::Budget,
    client::Client,
    emit::{emit_nquads, RunId},
    metrics::load_metrics,
    run_sweep,
};

#[derive(Parser)]
#[command(name = "sparqlwatch-prober")]
struct Args {
    #[arg(long, default_value = "endpoints.toml")]
    endpoints: String,
    #[arg(long, default_value = "metrics.toml")]
    metrics: String,
    #[arg(long, default_value = "run.nq")]
    out: String,
    /// ISO-8601 timestamp for the run. Passed in so runs are reproducible.
    #[arg(long)]
    at: String,
}

#[derive(serde::Deserialize)]
struct EndpointFile {
    endpoint: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let eps: EndpointFile = toml::from_str(&std::fs::read_to_string(&args.endpoints)?)?;
    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    let budget = Budget::default();
    let client = Client::new(budget)?;

    let rows = run_sweep(&eps.endpoint, &defs, &client, budget).await;
    let nq = emit_nquads(&RunId(args.at.clone()), &args.at, &rows)?;
    std::fs::write(&args.out, nq)?;
    tracing::info!(endpoints = eps.endpoint.len(), measurements = rows.len(), out = %args.out, "sweep complete");
    Ok(())
}
