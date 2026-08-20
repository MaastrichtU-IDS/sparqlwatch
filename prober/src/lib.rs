pub mod emit;
pub mod verdict;
pub mod budget;
pub mod client;
pub mod observe;
pub mod metrics;
pub mod resolve;

use crate::budget::Budget;
use crate::client::Client;
use crate::emit::MeasurementRow;
use crate::metrics::{MetricDef, ProbeKind};
use crate::resolve::{resolve, Declared};
use crate::verdict::Verdict;

/// Probe every metric against every endpoint. Endpoints are processed
/// independently so one slow host cannot delay another's results.
///
/// A metric of kind `AskData` or `SelectIris` (the two kinds that read a
/// variable's bindings back out of the result) with no `var` set is a
/// definition error, not something to paper over with a guessed variable
/// name: guessing wrong silently turns a real capability into a false
/// `Absent`. Such a metric is skipped and recorded as `Indeterminate`.
pub async fn run_sweep(
    endpoints: &[String],
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
) -> Vec<MeasurementRow> {
    let mut rows = Vec::new();
    for ep in endpoints {
        for def in defs {
            let q = def.query.clone().unwrap_or_default();
            let needs_var = matches!(def.kind, ProbeKind::AskData | ProbeKind::SelectIris);
            if needs_var && def.var.is_none() {
                rows.push(MeasurementRow {
                    endpoint: ep.clone(),
                    metric_id: def.id.clone(),
                    verdict: Verdict::Indeterminate,
                    level: None,
                    elapsed_ms: 0,
                });
                continue;
            }
            let var = def.var.clone();
            let fut = async {
                match def.kind {
                    ProbeKind::AskData => client.ask_literal(ep, &q, var.as_deref().unwrap()).await,
                    ProbeKind::SelectIris => client.select_iris(ep, &q, var.as_deref().unwrap()).await,
                    _ => client.ask(ep, &q).await,
                }
            };
            let observed = budget.with_metric_budget(fut).await;
            let verdict = resolve(def, Declared { claimed: false }, observed.as_ref().map_err(|e| *e));
            let elapsed = observed.as_ref().map(|o| o.elapsed_ms).unwrap_or(0);
            rows.push(MeasurementRow {
                endpoint: ep.clone(),
                metric_id: def.id.clone(),
                verdict,
                level: None,
                elapsed_ms: elapsed,
            });
        }
    }
    rows
}
