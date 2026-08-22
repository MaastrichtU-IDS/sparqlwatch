use clap::Parser;
use sparqlwatch_prober::{
    budget::Budget,
    client::Client,
    emit::{emit_nquads, RunEmission, RunId},
    metrics::{definitions_revision, load_metrics, within_cost, Cost},
    politeness::{Politeness, DEFAULT_MIN_GAP, DEFAULT_RETRY_AFTER_CAP},
    registry::load_endpoints,
    run_sweep, Sweep,
};
use std::time::Duration;

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
    /// The most a single metric may cost the endpoint it points at. Metrics
    /// declared more expensive than this are not run and not measured: they are
    /// published as a `NotMeasured` fact naming the ceiling as the reason,
    /// never as a zero or an `indeterminate` verdict. Defaults to `cheap`,
    /// because the default has to be safe to point at somebody else's server.
    #[arg(long, value_enum, default_value_t = Cost::Cheap)]
    max_cost: Cost,
    /// The minimum pause between two consecutive requests to one host,
    /// measured from the end of one to the start of the next. Requests to one
    /// host are also never in flight together, whatever this is set to.
    ///
    /// Validated at startup by `validate_min_gap`: the pause happens inside
    /// the metric budget, so `min gap + request budget < metric budget` or the
    /// gap alone consumes the budget the measurement needed.
    #[arg(long, default_value_t = DEFAULT_MIN_GAP.as_millis() as u64)]
    min_gap_ms: u64,
    /// The longest `Retry-After` we will wait out before retrying a throttled
    /// request once. A longer delay than this is a server telling us to come
    /// back after this sweep, so we report the throttle instead of waiting.
    ///
    /// Twenty seconds, and the ceiling is arithmetic rather than taste. The
    /// wait happens inside the held per-host guard, which sits inside the
    /// metric budget along with the gap, the first request and the retried
    /// request. The case the cap protects is the ordinary one, where the
    /// throttle came back quickly:
    ///
    ///     gap + cap + request budget < metric budget
    ///     2s  + 20s +      30s       = 52s < 60s
    ///
    /// Above that, `tokio::time::timeout` cancels the retry and the metric
    /// reports `indeterminate` after burning its whole budget, so raising this
    /// cap makes cancellation MORE likely, not less.
    ///
    /// No value here makes the worst case fit: a first request that runs its
    /// full 30s before the throttle arrives costs 2 + 30 + 30 = 62s with a cap
    /// of zero. That case is cancelled by design and reported as
    /// `indeterminate`, which is what never getting an answer looks like. A
    /// retry guaranteed to fit would need `Budget::metric` above 82s, which is
    /// a decision nobody has taken.
    #[arg(long, default_value_t = DEFAULT_RETRY_AFTER_CAP.as_secs())]
    retry_after_cap_s: u64,
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

/// `--min-gap-ms` is a pause that happens INSIDE the metric budget, before the
/// request it spaces, so it competes with the measurement for that budget. A
/// gap that leaves no room for one request under the metric budget turns a
/// healthy endpoint into a row set of `indeterminate` verdicts: the reviewer
/// measured 4 of 8 metrics lost to the gap alone against a mock that answered
/// everything. Nothing warns when that happens, because `run_sweep` warns on an
/// expired ENDPOINT budget and a cancelled metric budget is silent, so the
/// operator would read the run as a finding about the endpoints.
///
/// It is therefore a configuration error and not a slow sweep, rejected here
/// beside `--at` before a single request is sent. The relationship enforced is
/// named in the message, because an operator who set this deliberately needs to
/// know which of the two numbers to move.
///
/// `checked_sub` rather than an addition: `--min-gap-ms` is arbitrary operator
/// input, and `Duration` addition panics on overflow, which would turn a typo
/// into a crash instead of this message.
fn validate_min_gap(min_gap: Duration, budget: Budget) -> anyhow::Result<()> {
    let room = budget.metric.checked_sub(budget.request).unwrap_or(Duration::ZERO);
    if min_gap >= room {
        anyhow::bail!(
            "--min-gap-ms {} does not fit inside the metric budget: the gap plus the {}s \
             request budget must stay under the {}s metric budget, or the pause alone \
             consumes the budget the measurement needs and every metric reports \
             indeterminate. Use a gap below {}ms, or raise the metric budget.",
            min_gap.as_millis(),
            budget.request.as_secs(),
            budget.metric.as_secs(),
            room.as_millis(),
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    validate_instant(&args.at)?;
    let budget = Budget::default();
    validate_min_gap(Duration::from_millis(args.min_gap_ms), budget)?;
    // Deduplicated by the loader: one URL listed twice would otherwise be
    // probed twice and publish two `declarationsRead` facts about one endpoint
    // IRI in one run graph, which can and do disagree.
    let endpoints = load_endpoints(&std::fs::read_to_string(&args.endpoints)?)?;
    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    // The real settings, from flags a reader can see. `Politeness::unlimited()`
    // exists for tests and must never appear here.
    let politeness = Politeness::with_retry_after_cap(
        Duration::from_millis(args.min_gap_ms),
        Duration::from_secs(args.retry_after_cap_s),
    );
    let client = Client::new(budget, politeness)?;

    // A pure function of the definitions, so the published revision is
    // reproducible from the same metrics.toml. It identifies the definitions,
    // all of them, not the subset this
    // run chose to probe: the ceiling is published separately, on the
    // activity, so two runs of one file at different ceilings stay comparable.
    let revision = definitions_revision(&defs);
    // The policy lives here, in one place. `run_sweep` receives both halves as
    // data and never learns what a ceiling is.
    let (run, declined) = within_cost(&defs, args.max_cost);
    let Sweep { rows, declarations_read, not_measured, content_samples } =
        run_sweep(&endpoints, &run, &declined, &client, budget).await;
    let nq = emit_nquads(RunEmission {
        run: &RunId(args.at.clone()),
        generated_at: &args.at,
        metric_revision: &revision,
        rows: &rows,
        declarations_read: &declarations_read,
        not_measured: &not_measured,
        max_cost: args.max_cost,
        content_samples: &content_samples,
    })?;
    std::fs::write(&args.out, nq)?;
    tracing::info!(endpoints = endpoints.len(), measurements = rows.len(),
                   not_measured = not_measured.len(), content_samples = content_samples.len(),
                   max_cost = args.max_cost.slug(),
                   revision = %revision, out = %args.out, "sweep complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_instant, validate_min_gap, Args};
    use clap::Parser;
    use sparqlwatch_prober::budget::Budget;
    use sparqlwatch_prober::metrics::Cost;
    use std::time::Duration;

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

    /// The default ceiling is the whole safety property of the cost class:
    /// somebody who runs this without reading the flags must not fire a
    /// 45-second scan at 548 strangers' servers. Asserting on the PARSED args
    /// rather than on `Cost::default()` is deliberate: the latter would still
    /// pass if `default_value_t` were changed to name something else.
    /// The two politeness defaults, pinned on the PARSED args rather than on
    /// the constants, for the same reason as the cost ceiling below: asserting
    /// on `DEFAULT_RETRY_AFTER_CAP` would still pass if `default_value_t` were
    /// changed to name something else.
    ///
    /// The cap in particular is arithmetic, not taste, and the arithmetic is
    /// about the ordinary throttle rather than the worst case. Four things come
    /// out of one metric budget, in order: the gap, the first request, the
    /// honoured wait, and the retried request. A throttle usually comes back
    /// quickly, since refusing a request is cheap, so what has to fit is
    ///
    ///     gap + cap + request budget < metric budget    (2 + 20 + 30 = 52 < 60)
    ///
    /// and a larger cap eats that margin: raising it makes a cancelled retry
    /// more likely, not less.
    ///
    /// The worst case fits under NO cap, which is why there is no assertion
    /// about it here. A first request that runs its full 30s before the
    /// throttle arrives costs 2 + 30 + 30 = 62 > 60 with a cap of zero, so a
    /// gap plus two full-timeout requests is already over budget on its own.
    /// The budget cancels that retry and the metric reports `indeterminate`,
    /// which is correct: we never got an answer. A retry guaranteed to fit
    /// would need a metric budget above 82s, which is a decision nobody has
    /// taken.
    #[test]
    fn the_default_politeness_is_a_two_second_gap_and_a_twenty_second_cap() {
        let args = Args::parse_from(["prober", "--at", "2026-01-01T00:00:00Z"]);
        assert_eq!(args.min_gap_ms, 2000, "a plain run must pause between requests to one host");
        assert_eq!(args.retry_after_cap_s, 20, "a longer cap than this leaves no room to retry at all");
        let budget = Budget::default();
        assert!(
            Duration::from_millis(args.min_gap_ms)
                + Duration::from_secs(args.retry_after_cap_s)
                + budget.request
                < budget.metric,
            "gap + cap + request budget must stay under the metric budget, or even a throttle \
             that came back at once cannot be retried inside it"
        );
    }

    /// The gap is not merely a taste setting: it is spent from the same metric
    /// budget as the request it spaces. A gap that leaves no room for a request
    /// makes a healthy endpoint report `indeterminate` almost everywhere, and
    /// nothing in a sweep warns about it, so it has to be refused up front.
    #[test]
    fn a_gap_that_cannot_fit_inside_the_metric_budget_is_refused() {
        let budget = Budget::default(); // request 30s, metric 60s
        assert!(validate_min_gap(Duration::from_millis(2000), budget).is_ok(),
                "the shipped default has to be accepted");
        assert!(validate_min_gap(Duration::ZERO, budget).is_ok());
        assert!(validate_min_gap(Duration::from_millis(29_999), budget).is_ok(),
                "just inside the room the request budget leaves");
        // 30s of room, so 30s of gap leaves nothing at all for the request.
        assert!(validate_min_gap(Duration::from_millis(30_000), budget).is_err(),
                "a gap equal to the room left is not room for a request");
        assert!(validate_min_gap(Duration::from_millis(70_000), budget).is_err(),
                "the reviewer's 'let us be extra polite' value loses half the metrics");
        // Arbitrary operator input must not reach `tokio::time::sleep` and
        // panic on an `Instant` overflow.
        assert!(validate_min_gap(Duration::from_millis(u64::MAX), budget).is_err());
    }

    /// The message has to name the relationship, not just the refusal: an
    /// operator who set a long gap on purpose needs to know which number to
    /// move.
    #[test]
    fn the_refusal_says_which_two_numbers_have_to_fit_together() {
        let err = validate_min_gap(Duration::from_millis(70_000), Budget::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("--min-gap-ms"), "names the flag: {err}");
        assert!(err.contains("70000"), "names the value it refused: {err}");
        assert!(err.contains("request budget") && err.contains("metric budget"),
                "names both budgets in the relationship: {err}");
        assert!(err.contains("30000ms"), "names the ceiling to stay under: {err}");
    }

    #[test]
    fn the_default_cost_ceiling_is_cheap() {
        let args = Args::parse_from(["prober", "--at", "2026-01-01T00:00:00Z"]);
        assert_eq!(
            args.max_cost,
            Cost::Cheap,
            "a plain run must not include expensive metrics"
        );
    }
}
