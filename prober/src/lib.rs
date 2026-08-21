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
use crate::declare::{parse_declarations_for, Declarations};
use crate::emit::{DeclarationsRead, MeasurementRow};
use crate::metrics::{MetricDef, ProbeKind};
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
///
/// Alongside the rows, every endpoint gets exactly one `DeclarationsRead`
/// fact: whether its queryless description fetch produced a parseable graph
/// of at least one triple. `Declared::claimed` cannot answer this on its
/// own -- `false` there means either "declares nothing" or "we could not
/// read it", and the two are distinguished only by this fact. `read` starts
/// `false` and is set inside `probe_endpoint` as soon as the fetch's
/// `Declarations` are known, so an endpoint whose budget expires before that
/// point (never fetched at all) still gets a fact, honestly `false`, rather
/// than none.
pub async fn run_sweep(
    endpoints: &[String],
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
) -> (Vec<MeasurementRow>, Vec<DeclarationsRead>) {
    let mut rows = Vec::new();
    let mut declarations_read = Vec::new();
    for ep in endpoints {
        let mut ep_rows: Vec<MeasurementRow> = Vec::new();
        let mut read = false;
        let outcome = budget
            .with_endpoint_budget(probe_endpoint(ep, defs, client, budget, &mut ep_rows, &mut read))
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
        declarations_read.push(DeclarationsRead { endpoint: ep.clone(), read });
        rows.extend(ep_rows);
    }
    (rows, declarations_read)
}

const VAR_REQUIRED: &str = "a bindings-reading probe kind requires `var`; load_metrics enforces it";

/// One endpoint's metrics, in definition order, appending as it goes so a
/// caller that cancels this future can still see how far it got.
///
/// `declarations_read` is set the same way, as a side effect on a borrowed
/// `bool`, for the same reason: `run_sweep` wraps this whole function in the
/// endpoint budget, and a cancelled future returns nothing, so a fact that
/// depended on this call's return value would simply be lost whenever the
/// budget expired before the fetch finished. Starting `false` and setting it
/// true only once a graph is actually in hand keeps the fact honest under
/// cancellation too.
async fn probe_endpoint(
    ep: &str,
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
    rows: &mut Vec<MeasurementRow>,
    declarations_read: &mut bool,
) {
    // One queryless fetch per endpoint, not one per metric: six metrics must
    // not mean six identical GETs landing in an operator's log. Its outcome
    // feeds two things below: the `Declarations` every metric's `Declared`
    // is built from, and the `FetchWellKnown` row itself.
    let fetch_outcome = budget.with_metric_budget(client.fetch_rdf(ep)).await;
    let declarations = match &fetch_outcome {
        // `parse_declarations` never panics and yields partial (often empty)
        // results on anything that isn't real RDF, so the body is passed
        // unconditionally rather than re-checking `body_kind` here too: an
        // HTML console parses to ~0 triples either way, and `resolve_fetch`
        // below is what actually decides `Html`/failure means `Indeterminate`.
        // Keeping that decision in one place means the two can't disagree.
        // `content_type` is threaded through so the real serialization
        // (RDF/XML, JSON-LD, ...) is parsed as itself rather than assumed to
        // be Turtle. The endpoint URLs are threaded through because a
        // document may describe several services and a capability claim may
        // only be read from the one we probed.
        Ok(o) => {
            // Both URLs name the same service: the one the registry gave us
            // and, if the fetch was redirected, the one it landed on. A
            // description that states only its post-redirect `sd:endpoint`
            // would otherwise look like somebody else's document.
            let mut urls: Vec<&str> = vec![ep];
            if let Some(landed) = o.final_url.as_deref() {
                if landed != ep {
                    urls.push(landed);
                }
            }
            parse_declarations_for(o.body.as_deref().unwrap_or(""), o.content_type.as_deref(), &urls)
        }
        Err(Expired) => Declarations::empty(),
    };
    // The honest definition: we got a graph and read at least one triple out
    // of it. This is about the parse, not the HTTP status or the fetch's own
    // verdict: a 200 with an empty body reads `false` here, and a body that
    // declares something and then hits a syntax error mid-parse (`declare.rs`
    // keeps what parsed before the error) reads `true` even though
    // `resolve_fetch` below grades that same fetch `Indeterminate`.
    *declarations_read = declarations.triples > 0;
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
                // A level means something only for a metric defined as
                // `graded`: that flag, not the probe kind, is what
                // `metrics.toml` uses to say "this one carries a grade", and
                // metrics are data a config edit can change. Keying off
                // `graded` here means a future non-graded `FetchWellKnown`
                // metric (or a graded metric of some other kind, should one
                // ever exist) gets exactly the row shape its definition asks
                // for, not one implied by its probe kind.
                level: if def.graded { fetch_level } else { None },
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
            // No catch-all arm in this match, ever. A new probe kind must fail to
            // compile until it is dispatched. Exhaustiveness checking ensures no
            // routing bugs hide behind plausible-looking fallbacks.
            match def.kind {
                ProbeKind::AskData => client.ask_literal(ep, &q, var.as_deref().expect(VAR_REQUIRED)).await,
                ProbeKind::SelectIris => client.select_iris(ep, &q, var.as_deref().expect(VAR_REQUIRED)).await,
                // The only probe that announces an `Origin`, so the others
                // cannot be perturbed by a server that filters on it.
                ProbeKind::Cors => client.cors(ep, &q).await,
                ProbeKind::Liveness => client.ask(ep, &q).await,
                ProbeKind::AskFilter => client.ask(ep, &q).await,
                ProbeKind::FetchWellKnown => unreachable!("FetchWellKnown is handled once per endpoint before the per-metric dispatch"),
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
