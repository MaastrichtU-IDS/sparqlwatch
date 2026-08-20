pub mod emit;
pub mod verdict;
pub mod budget;
pub mod client;
pub mod declare;
pub mod observe;
pub mod metrics;
pub mod resolve;

use crate::budget::{Budget, Expired};
use crate::client::Client;
use crate::declare::{parse_declarations, Declarations};
use crate::emit::MeasurementRow;
use crate::metrics::{MetricDef, ProbeKind};
use crate::observe::BodyKind;
use crate::resolve::{resolve, resolve_fetch, Declared};
use crate::verdict::Verdict;

/// Probe every metric against every endpoint. Endpoints are processed
/// independently so one slow host cannot delay another's results.
///
/// Three nested budgets bound the work: per request (in the HTTP client), per
/// metric, and per endpoint. When the endpoint budget expires mid-loop the
/// metrics not reached still get rows, recorded as `Indeterminate`, so the
/// one-row-per-(endpoint, metric) invariant holds whatever the timing.
///
/// A metric whose kind has no implemented probe is skipped without issuing any
/// request, and also recorded as `Indeterminate`, so the gap stays visible in
/// the published output rather than being papered over.
///
/// Every verdict here comes from `resolve()` or from a budget expiry, which is
/// the one thing this layer knows and the resolver cannot: judgement about an
/// *observation* never happens outside `resolve()`.
pub async fn run_sweep(
    endpoints: &[String],
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
) -> Vec<MeasurementRow> {
    let mut rows = Vec::new();
    for ep in endpoints {
        let mut ep_rows: Vec<MeasurementRow> = Vec::new();
        let outcome = budget
            .with_endpoint_budget(probe_endpoint(ep, defs, client, budget, &mut ep_rows))
            .await;
        if outcome.is_err() {
            tracing::warn!(endpoint = %ep, reached = ep_rows.len(), of = defs.len(),
                           "endpoint budget expired; remaining metrics are indeterminate");
            // The budget expiring tells us nothing about the metrics we never
            // got to, and we did not measure their elapsed time either. Route
            // the verdict through `resolve` rather than writing one here:
            // judgement belongs in one place, and `resolve` already maps an
            // expired budget to `Indeterminate`.
            for def in defs.iter().skip(ep_rows.len()) {
                ep_rows.push(MeasurementRow {
                    endpoint: ep.clone(),
                    metric_id: def.id.clone(),
                    verdict: resolve(def, Declared { claimed: false }, Err(Expired)),
                    level: None,
                    elapsed_ms: None,
                });
            }
        }
        rows.extend(ep_rows);
    }
    rows
}

const VAR_REQUIRED: &str = "a bindings-reading probe kind requires `var`; load_metrics enforces it";

/// One endpoint's metrics, in definition order, appending as it goes so a
/// caller that cancels this future can still see how far it got.
async fn probe_endpoint(
    ep: &str,
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
    rows: &mut Vec<MeasurementRow>,
) {
    // One queryless fetch per endpoint, not one per metric: six metrics must
    // not mean six identical GETs landing in an operator's log. Its outcome
    // feeds two things below: the `Declarations` every metric's `Declared`
    // is built from, and the `FetchWellKnown` row itself.
    let fetch_outcome = budget.with_metric_budget(client.fetch_rdf(ep)).await;
    let declarations = match &fetch_outcome {
        // Only a body that actually parsed as RDF has anything to declare;
        // anything else (a transport failure, an HTML console, a body that
        // didn't parse) yields the same empty `Declarations` as never having
        // fetched at all.
        Ok(o) if o.body_kind == BodyKind::Rdf => parse_declarations(o.body.as_deref().unwrap_or(""), None),
        _ => Declarations::empty(),
    };
    let (fetch_verdict, fetch_level) = resolve_fetch(&declarations, fetch_outcome.as_ref().map_err(|e| *e));
    // Same principle as every other row: an expired or failed fetch measured
    // nothing, so it reports no elapsed time rather than a zero one.
    let fetch_elapsed = fetch_outcome.as_ref().ok().map(|o| o.elapsed_ms);

    for def in defs {
        // The description was already fetched once above; this row reports
        // that outcome rather than issuing a second, redundant fetch.
        if def.kind == ProbeKind::FetchWellKnown {
            rows.push(MeasurementRow {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                verdict: fetch_verdict,
                level: fetch_level,
                elapsed_ms: fetch_elapsed,
            });
            continue;
        }
        // A kind with no implemented probe is skipped before any request is
        // built: the generic query path would send `?query=` to a real
        // operator and learn nothing. No current kind takes this path (even
        // `FetchWellKnown` is handled above), but the mechanism stays for
        // whichever future kind arrives without one.
        if !def.kind.has_probe() {
            rows.push(MeasurementRow {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                verdict: Verdict::Indeterminate,
                level: None,
                elapsed_ms: None,
            });
            continue;
        }
        let q = def.query.clone().unwrap_or_default();
        // `load_metrics` rejects a bindings-reading kind with no `var`, so
        // this cannot be None for a definition that came from a file. A
        // hand-built one that violates it is a programming error and fails
        // loudly here rather than measuring the wrong variable.
        let var = def.var.clone();
        let fut = async {
            match def.kind {
                ProbeKind::AskData => client.ask_literal(ep, &q, var.as_deref().expect(VAR_REQUIRED)).await,
                ProbeKind::SelectIris => client.select_iris(ep, &q, var.as_deref().expect(VAR_REQUIRED)).await,
                // The only probe that announces an `Origin`, so the others
                // cannot be perturbed by a server that filters on it.
                ProbeKind::Cors => client.cors(ep, &q).await,
                _ => client.ask(ep, &q).await,
            }
        };
        let observed = budget.with_metric_budget(fut).await;
        let declared = Declared::from(&declarations, def);
        let verdict = resolve(def, declared, observed.as_ref().map_err(|e| *e));
        // An expired metric budget measured nothing, so it reports no elapsed
        // time rather than a zero one.
        let elapsed = observed.as_ref().ok().map(|o| o.elapsed_ms);
        rows.push(MeasurementRow {
            endpoint: ep.to_string(),
            metric_id: def.id.clone(),
            verdict,
            level: None,
            elapsed_ms: elapsed,
        });
    }
}
