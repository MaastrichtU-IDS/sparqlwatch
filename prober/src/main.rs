use clap::Parser;
use sparqlwatch_prober::{
    budget::Budget,
    client::Client,
    emit::{emit_nquads, RunId},
    metrics::{definitions_revision, load_metrics},
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

/// `--at` is interpolated into two IRIs and published as an `xsd:dateTime`, so
/// a value like `banana` would produce an ill-typed literal and a nonsense
/// graph name. A focused shape check is enough here and avoids a date-time
/// dependency: an `xsd:dateTime`-shaped instant, with an explicit timezone,
/// and therefore made only of characters that are safe inside an IRI.
fn validate_instant(at: &str) -> anyhow::Result<()> {
    let bad = || anyhow::anyhow!(
        "--at must be an ISO-8601 instant such as 2026-08-20T12:00:00Z, got {at:?}"
    );
    let (date, rest) = at.split_once('T').ok_or_else(bad)?;
    let d: Vec<&str> = date.split('-').collect();
    let num = |s: &str, width: usize, lo: u32, hi: u32| -> anyhow::Result<u32> {
        if s.len() != width || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let v: u32 = s.parse().map_err(|_| bad())?;
        if (lo..=hi).contains(&v) { Ok(v) } else { Err(bad()) }
    };
    if d.len() != 3 {
        return Err(bad());
    }
    num(d[0], 4, 0, 9999)?;
    num(d[1], 2, 1, 12)?;
    num(d[2], 2, 1, 31)?;

    // Split the timezone designator off the time before parsing either.
    let (time, zone) = if let Some(t) = rest.strip_suffix('Z') {
        (t, "Z".to_string())
    } else if let Some((t, z)) = rest.rsplit_once(['+', '-']) {
        (t, z.to_string())
    } else {
        return Err(bad());
    };
    if zone != "Z" {
        let (zh, zm) = zone.split_once(':').ok_or_else(bad)?;
        num(zh, 2, 0, 23)?;
        num(zm, 2, 0, 59)?;
    }

    // Seconds may carry a fraction; everything else is fixed-width.
    let (hms, frac) = match time.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (time, None),
    };
    let t: Vec<&str> = hms.split(':').collect();
    if t.len() != 3 {
        return Err(bad());
    }
    num(t[0], 2, 0, 23)?;
    num(t[1], 2, 0, 59)?;
    num(t[2], 2, 0, 60)?; // 60 for a leap second, which xsd::dateTime permits
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    validate_instant(&args.at)?;
    let eps: EndpointFile = toml::from_str(&std::fs::read_to_string(&args.endpoints)?)?;
    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    let budget = Budget::default();
    let client = Client::new(budget)?;

    // A pure function of the definitions, so the published revision is
    // reproducible from the same metrics.toml.
    let revision = definitions_revision(&defs);
    let (rows, declarations_read) = run_sweep(&eps.endpoint, &defs, &client, budget).await;
    let nq = emit_nquads(&RunId(args.at.clone()), &args.at, &revision, &rows, &declarations_read)?;
    std::fs::write(&args.out, nq)?;
    tracing::info!(endpoints = eps.endpoint.len(), measurements = rows.len(),
                   revision = %revision, out = %args.out, "sweep complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_instant;

    #[test]
    fn a_well_formed_instant_is_accepted() {
        for ok in [
            "2026-08-20T12:00:00Z",
            "2026-08-20T12:00:00.123Z",
            "2026-08-20T12:00:00+02:00",
            "2026-08-20T12:00:00-05:00",
            "2026-12-31T23:59:60Z",
        ] {
            assert!(validate_instant(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn a_malformed_instant_is_rejected_before_anything_is_published() {
        for bad in [
            "banana",
            "",
            "2026-08-20",
            "2026-08-20T12:00:00",   // no timezone designator
            "2026-13-01T00:00:00Z",  // month 13
            "2026-08-32T00:00:00Z",  // day 32
            "2026-08-20T24:00:00Z",  // hour 24
            "2026-08-20T12:61:00Z",  // minute 61
            "2026-08-20T12:00:00.Z", // empty fraction
            "2026-8-20T12:00:00Z",   // unpadded month
            "2026-08-20T12:00:00 Z", // a space would also break the IRI
            "2026-08-20T12:00:00+2:00",
        ] {
            assert!(validate_instant(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
