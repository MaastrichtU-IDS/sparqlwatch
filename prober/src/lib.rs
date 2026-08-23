pub mod emit;
pub mod verdict;
pub mod budget;
pub mod client;
pub mod declare;
pub mod media;
pub mod observe;
pub mod politeness;
pub mod metrics;
pub mod registry;
pub mod resolve;

use crate::budget::{Budget, Expired};
use crate::client::Client;
use crate::declare::{parse_declarations_for, Declarations};
use crate::emit::{ContentSample, DeclarationsRead, MeasurementRow, NotMeasured, NotMeasuredReason};
use crate::metrics::{MetricDef, ProbeKind};
use crate::politeness::host_key;
use crate::resolve::{resolve, resolve_fetch, Declared};
use crate::verdict::Verdict;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Everything one sweep produced, flat and ready to emit: one entry per
/// (endpoint, metric) in `rows`, one per endpoint in `declarations_read`, one
/// per (endpoint, declined metric) in `not_measured`. See `run_sweep` for what
/// each of them means and how the budgets shape them.
pub struct Sweep {
    pub rows: Vec<MeasurementRow>,
    pub declarations_read: Vec<DeclarationsRead>,
    pub not_measured: Vec<NotMeasured>,
    /// What the enumerating metrics saw, one entry per (endpoint, metric that
    /// declared a `sample_limit`, reached a positive verdict, and bound at
    /// least one value). A metric that declared no `sample_limit` contributes
    /// nothing here; neither does one whose probe bound nothing, because an
    /// empty list is not a sample and the measurement row already says
    /// `absent`; and neither does one the resolver would not confirm, because
    /// a sample is an assertion and an unconfirmed observation supports none.
    pub content_samples: Vec<ContentSample>,
    /// How many endpoints the sweep produced no observation for at all,
    /// because the task probing their host never returned. Each of them
    /// carries a `NotMeasured` fact per metric instead, so the count is a
    /// summary of facts already in the lists above rather than the only
    /// record of them, and `main.rs` exits non-zero on it after writing the
    /// output.
    pub failed_endpoints: usize,
}

/// Probe every metric against every endpoint.
///
/// The unit of concurrency is a HOST, not an endpoint: endpoints are grouped
/// by `politeness::host_key`, each group is one sequential task, and
/// `concurrency` bounds how many groups run at once. So `concurrency` means
/// "how many hosts we talk to at once", and one host is never asked two things
/// at once whatever it is set to.
///
/// That is what keeps the budget arithmetic to the one relation `main.rs`
/// validates. `politeness::acquire`'s guard is held until the request returns,
/// so a competing endpoint on the same host would wait for the guard rather
/// than for the gap: gap plus the whole first request, and on a throttled host
/// gap plus request plus the honoured `Retry-After` plus the retry, which is 82
/// seconds inside a 60-second metric budget. A cancelled metric budget does not
/// warn, so the symptom would be healthy-but-slow endpoints quietly reported
/// `indeterminate`, which is exactly the population this project exists to
/// characterise. One endpoint per host in flight removes that term by
/// construction.
///
/// It also settles dispatch order without a heuristic: grouping by host is what
/// spreads the work, so no round-robin is needed, and "concurrency only buys
/// parallelism across different hosts" is a property of the code rather than an
/// intention.
///
/// Output order is INPUT order, never completion order: each endpoint keeps its
/// input index as a slot and `assemble` walks the slots afterwards. A sweep's
/// output therefore does not depend on which host answered first.
///
/// Three nested budgets bound the work: per request (in the HTTP client), per
/// metric, and per endpoint. When the endpoint budget expires mid-loop the
/// metrics not reached still get rows, recorded as `Indeterminate`, so the
/// one-row-per-(endpoint, metric) invariant holds whatever the timing. That
/// fill, and the accumulator whose ownership makes the partial results survive
/// the expiry at all, both live in `probe_one_endpoint`.
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
/// read it", and the two are distinguished only by this fact.
/// `EndpointSweep::declarations_read` starts `false` and is set inside
/// `probe_endpoint` as soon as the fetch's `Declarations` are known, so an
/// endpoint whose budget expires before that point (never fetched at all)
/// still gets a fact, honestly `false`, rather than none.
///
/// `declined` is the other half of the definition list: metrics the caller
/// decided not to run. This function does not filter and does not know what a
/// ceiling is -- `main.rs` calls `metrics::within_cost` and passes both halves
/// -- it simply records one `NotMeasured` per (endpoint, declined metric) so
/// the gap is published as a fact rather than left as a missing row. Keeping
/// the policy in one place and the mechanism in another is deliberate: a
/// second reason for declining a metric changes `main.rs` and the reason enum,
/// not this loop.
pub async fn run_sweep(
    endpoints: &[String],
    defs: &[MetricDef],
    declined: &[MetricDef],
    client: &Arc<Client>,
    budget: Budget,
    concurrency: NonZeroUsize,
) -> Sweep {
    // Endpoints grouped by host, each keeping its input index as its slot, in
    // first-seen host order. First-seen rather than sorted so the endpoints a
    // registry lists first are also the ones dispatched first when there are
    // more hosts than permits, and deterministic either way, which a `HashMap`
    // iteration order would not be.
    let mut groups: Vec<Vec<(usize, String)>> = Vec::new();
    let mut group_of: HashMap<String, usize> = HashMap::new();
    for (slot, ep) in endpoints.iter().enumerate() {
        let key = host_key(ep);
        match group_of.get(&key) {
            Some(existing) => groups[*existing].push((slot, ep.clone())),
            None => {
                group_of.insert(key, groups.len());
                groups.push(vec![(slot, ep.clone())]);
            }
        }
    }
    let permits = Arc::new(Semaphore::new(concurrency.get()));
    // One shared copy of the definitions for the whole sweep. A `to_vec()` per
    // endpoint would be 548 copies of the definition list at stage 1d.
    let shared_defs = Arc::new(defs.to_vec());
    let mut tasks: JoinSet<Vec<(usize, EndpointSweep)>> = JoinSet::new();
    // Which endpoints each task is probing. `JoinSet::join_next`'s error arm
    // carries only a `JoinError`, whose only identifying information is
    // `JoinError::id()`, so without this map a task that panicked could not
    // even be logged with the endpoints it was probing. It is for the log
    // alone: the slots it covered are filled by `assemble` from their own
    // emptiness, so a missing entry here costs a name in one log line and
    // nothing in the published graph.
    let mut covering: HashMap<tokio::task::Id, Vec<String>> = HashMap::new();
    for group in groups {
        let named: Vec<String> = group.iter().map(|(_, ep)| ep.clone()).collect();
        // Everything the task touches is owned by it: `probe_one_endpoint`
        // borrows the endpoint, the definitions and the client, and a spawned
        // future must be `'static`.
        let permits = Arc::clone(&permits);
        let defs = Arc::clone(&shared_defs);
        let client = Arc::clone(client);
        let handle = tasks.spawn(async move {
            // Held for the WHOLE group, not per endpoint: a permit is the
            // right to talk to this host at all, and releasing it between two
            // of its endpoints would let another group's endpoint start while
            // this host still has work queued behind the same guard.
            let _permit = permits
                .acquire()
                .await
                .expect("the sweep owns this semaphore and never closes it");
            let mut done = Vec::with_capacity(group.len());
            for (slot, ep) in group {
                done.push((slot, probe_one_endpoint(&ep, &defs, &client, budget).await));
            }
            done
        });
        covering.insert(handle.id(), named);
    }
    let mut slots: Vec<Option<EndpointSweep>> = endpoints.iter().map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(done) => {
                for (slot, swept) in done {
                    slots[slot] = Some(swept);
                }
            }
            // A panic in one group costs that whole group, since its results
            // are built inside the task, so the log names every endpoint it
            // covered rather than one. The slots stay empty and `assemble`
            // publishes a `prober-failed` fact for each of them: an endpoint
            // that contributed nothing at all would leave the site serving the
            // previous run's verdicts as current.
            Err(failure) => {
                let named = covering.get(&failure.id()).cloned().unwrap_or_default();
                tracing::error!(
                    endpoints = ?named, error = %failure,
                    "the task probing these endpoints failed; every metric on them is \
                     published as not measured, reason prober-failed"
                );
            }
        }
    }
    assemble(slots, endpoints, defs, declined)
}

/// Turn one slot per endpoint into the four flat fact lists, walking
/// `endpoints` in input order so the output does not depend on which task
/// finished first.
fn assemble(
    slots: Vec<Option<EndpointSweep>>,
    endpoints: &[String],
    defs: &[MetricDef],
    declined: &[MetricDef],
) -> Sweep {
    let mut rows = Vec::new();
    let mut declarations_read = Vec::new();
    let mut not_measured = Vec::new();
    let mut content_samples = Vec::new();
    let mut failed_endpoints = 0;
    for (ep, slot) in endpoints.iter().zip(slots) {
        match slot {
            Some(swept) => {
                declarations_read
                    .push(DeclarationsRead { endpoint: ep.clone(), read: swept.declarations_read });
                rows.extend(swept.rows);
                content_samples.extend(swept.content_samples);
            }
            // Nothing was observed, so there is nothing to grade: no rows, and
            // no `declarationsRead` either, since whether the description was
            // readable is precisely what this run did not find out. One
            // `NotMeasured` per metric that would have run instead.
            //
            // `ProberFailed` rather than `Indeterminate` rows: an
            // `Indeterminate` verdict asserts that a measurement happened and
            // was inconclusive, which is what an expired budget produces, so a
            // reader could not tell a crashed sweep from a slow endpoint. The
            // declined metrics keep their `CostCeiling` facts below, so no
            // (endpoint, metric) pair gets two `NotMeasured` facts and
            // `emit`'s duplicate-subject guard is not triggered.
            None => {
                failed_endpoints += 1;
                for def in defs {
                    not_measured.push(NotMeasured {
                        endpoint: ep.clone(),
                        metric_id: def.id.clone(),
                        reason: NotMeasuredReason::ProberFailed,
                    });
                }
            }
        }
        // One fact per (endpoint, declined metric), recorded whatever the
        // budget did: the reason is the ceiling, which was decided before any
        // probing started, so an endpoint whose budget expired still owes the
        // reader an account of the metrics it was never going to run.
        for def in declined {
            not_measured.push(NotMeasured {
                endpoint: ep.clone(),
                metric_id: def.id.clone(),
                reason: NotMeasuredReason::CostCeiling,
            });
        }
    }
    Sweep { rows, declarations_read, not_measured, content_samples, failed_endpoints }
}

/// One endpoint's results, accumulated in place as `probe_endpoint` earns
/// them.
///
/// The value is owned by `probe_one_endpoint`, which is the caller of the
/// endpoint budget's `timeout`, and never by the future that timeout wraps.
/// That ownership is the entire reason this type exists. `tokio::time::timeout`
/// DROPS the future it wraps on expiry, and a dropped future returns nothing,
/// so anything `probe_endpoint` returned instead of writing here would be lost
/// for every endpoint whose budget expires -- the ordinary case for a slow or
/// black-holed host, not an edge case. Lost, specifically: every row already
/// measured, after which the expiry fill would see no rows at all and write
/// `Indeterminate` over real verdicts and their `elapsedMs`; every sample
/// already collected; and a `declarations_read` we had just earned the right
/// to publish as `true`.
///
/// Belonging to the unit of work rather than to the sweep loop is also what
/// lets one endpoint be probed inside a task of its own: the borrow is created
/// and consumed inside `probe_one_endpoint`, so the future borrows nothing the
/// scheduler owns.
#[derive(Default)]
struct EndpointSweep {
    /// The metrics reached so far, in definition order. Its length is
    /// therefore also how far the loop got, which is what the expiry fill in
    /// `probe_one_endpoint` resumes from.
    rows: Vec<MeasurementRow>,
    /// Whether the queryless description fetch produced a parseable graph of
    /// at least one triple. Starts `false`, so an endpoint whose budget expired
    /// before the fetch finished publishes an honest `false` rather than
    /// nothing.
    declarations_read: bool,
    /// This endpoint's share of `Sweep::content_samples`, under the same rules.
    content_samples: Vec<ContentSample>,
}

/// Probe one endpoint, under the endpoint budget: the unit of work a sweep
/// schedules.
///
/// The accumulator is created here, outside the `timeout`, and lent to
/// `probe_endpoint`, so a budget that expires mid-loop still yields everything
/// measured before it did. See `EndpointSweep` for why that is not a style
/// choice.
async fn probe_one_endpoint(
    ep: &str,
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
) -> EndpointSweep {
    let mut acc = EndpointSweep::default();
    let outcome = budget
        .with_endpoint_budget(probe_endpoint(ep, defs, client, budget, &mut acc))
        .await;
    if outcome.is_err() {
        tracing::warn!(endpoint = %ep, reached = acc.rows.len(), of = defs.len(),
                       "endpoint budget expired; remaining metrics are indeterminate");
        // The budget expiring tells us nothing about the metrics we never
        // got to, and we did not measure their elapsed time either. Route
        // the verdict through `resolve` rather than writing one here:
        // judgement belongs in one place, and `resolve` already maps an
        // expired budget to `Indeterminate`.
        for def in defs.iter().skip(acc.rows.len()) {
            acc.rows.push(MeasurementRow {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                verdict: resolve(def, Declared { claimed: false }, Err(Expired)),
                level: None,
                elapsed_ms: None,
            });
        }
    }
    acc
}

const VAR_REQUIRED: &str = "a bindings-reading probe kind requires `var`; load_metrics enforces it";

/// One endpoint's metrics, in definition order, appending to `acc` as it goes
/// so a caller that cancels this future can still see how far it got.
///
/// `acc.declarations_read` is set the same way, as a side effect on the
/// borrowed accumulator, for the same reason: `probe_one_endpoint` wraps this
/// whole function in the endpoint budget, and a cancelled future returns
/// nothing, so a fact that depended on this call's return value would simply be
/// lost whenever the budget expired before the fetch finished. Starting `false`
/// and setting it true only once a graph is actually in hand keeps the fact
/// honest under cancellation too.
async fn probe_endpoint(
    ep: &str,
    defs: &[MetricDef],
    client: &Client,
    budget: Budget,
    acc: &mut EndpointSweep,
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
    acc.declarations_read = declarations.triples > 0;
    let (fetch_verdict, fetch_level) = resolve_fetch(&declarations, fetch_outcome.as_ref().map_err(|e| *e));
    // Same principle as every other row: an expired or failed fetch measured
    // nothing, so it reports no elapsed time rather than a zero one.
    let fetch_elapsed = fetch_outcome.as_ref().ok().map(|o| o.elapsed_ms);

    for def in defs {
        // The description was already fetched once above; this row reports
        // that outcome rather than issuing a second, redundant fetch.
        if def.kind == ProbeKind::FetchWellKnown {
            acc.rows.push(MeasurementRow {
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
            acc.rows.push(MeasurementRow {
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
                // The two probes that announce an `Origin`, so the others
                // cannot be perturbed by a server that filters on it.
                ProbeKind::Cors => client.cors(ep, &q).await,
                // Takes no query: a preflight carries none, and this probe
                // asks whether a browser would be allowed to send one at all.
                ProbeKind::CorsPreflight => client.preflight(ep).await,
                ProbeKind::Liveness => client.ask(ep, &q).await,
                ProbeKind::AskFilter => client.ask(ep, &q).await,
                ProbeKind::FetchWellKnown => unreachable!("FetchWellKnown is handled once per endpoint before the per-metric dispatch"),
            }
        };
        let observed = budget.with_metric_budget(fut).await;
        let declared = Declared::from(&declarations, def);
        let verdict = resolve(def, declared, observed.as_ref().map_err(|e| *e));
        // The bindings are kept only for a metric that asked to enumerate. Up
        // to here they were used to reach a verdict and then dropped, so an
        // endpoint could be reported as having classes without ever saying
        // which, for a task whose whole purpose is knowing what is in an
        // endpoint before writing a query.
        if let Some(limit) = def.sample_limit {
            // Gated on the VERDICT, not on the response status. A sample is an
            // assertion about the endpoint's data, so it may only be published
            // where the resolver confirmed the capability: `Verified` or
            // `UndeclaredButVerified`. Everything else, `Indeterminate` above
            // all, means we do not know what this endpoint holds, and a graph
            // that says "we could not determine whether this endpoint has
            // classes" must not also say "here are its 59 classes, complete".
            //
            // The rejected alternative was to re-check the status here, the way
            // `resolve`'s `SelectIris` arm checks `answered_ok`. That would be
            // the third copy of the same rule (`resolve_fetch` and the
            // `Liveness` positive case were both fixed by adding one), so the
            // rules could drift apart in silence and a future change to them
            // would have to find every copy. Riding on the verdict means the
            // sample and the measurement cannot disagree by construction, and a
            // change to the status rules carries the sample with it.
            let confirmed = matches!(verdict, Verdict::Verified | Verdict::UndeclaredButVerified);
            if let Ok(o) = &observed {
                // The empty check stays alongside the verdict gate rather than
                // relying on it: no current kind reaches a positive verdict
                // with nothing bound, and if one ever does, a sample of size
                // zero is still not a sample.
                if confirmed && !o.bindings.is_empty() {
                    acc.content_samples.push(ContentSample {
                        endpoint: ep.to_string(),
                        metric_id: def.id.clone(),
                        values: o.bindings.clone(),
                        // `>=`, not `==`, deliberately. An endpoint that
                        // ignores `LIMIT` and returns more than the cap has
                        // still handed us a sample we cannot call complete;
                        // `==` would call exactly that case complete, which is
                        // the one failure this fact exists to prevent.
                        truncated: o.bindings.len() >= limit,
                    });
                }
            }
        }
        // An expired metric budget measured nothing, so it reports no elapsed
        // time rather than a zero one.
        let elapsed = observed.as_ref().ok().map(|o| o.elapsed_ms);
        acc.rows.push(MeasurementRow {
            endpoint: ep.to_string(),
            metric_id: def.id.clone(),
            verdict,
            level: None,
            elapsed_ms: elapsed,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Cost, ProbeKind};

    fn def(id: &str) -> MetricDef {
        MetricDef {
            id: id.into(),
            label: id.into(),
            dimension: "content".into(),
            kind: ProbeKind::Liveness,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            sample_limit: None,
        }
    }

    /// One endpoint's finished work: a row per metric, a description read, and
    /// one sample. What a slot holds when its group came back.
    fn swept(ep: &str, defs: &[MetricDef]) -> EndpointSweep {
        EndpointSweep {
            rows: defs
                .iter()
                .map(|d| MeasurementRow {
                    endpoint: ep.to_string(),
                    metric_id: d.id.clone(),
                    verdict: Verdict::Verified,
                    level: None,
                    elapsed_ms: Some(7),
                })
                .collect(),
            declarations_read: true,
            content_samples: vec![ContentSample {
                endpoint: ep.to_string(),
                metric_id: defs[0].id.clone(),
                values: vec!["http://example.org/C".to_string()],
                truncated: false,
            }],
        }
    }

    /// A unit test on `assemble` over already-normalised slots, and not on
    /// `run_sweep`, because `JoinError` has no public constructor: a fold typed
    /// over one cannot be tested at all, which is why the translation from a
    /// join failure to an empty slot happens at the `join_next` site and the
    /// judgement about an empty slot happens here.
    #[test]
    fn a_failed_group_still_carries_facts_for_every_endpoint_in_it() {
        let a = "https://a.example/sparql";
        let b = "https://b.example/sparql";
        let c = "https://c.example/sparql";
        let endpoints = vec![a.to_string(), b.to_string(), c.to_string()];
        let defs = vec![def("availability"), def("has-classes")];
        let declined = vec![def("classes")];
        let slots = vec![Some(swept(a, &defs)), None, Some(swept(c, &defs))];

        let sweep = assemble(slots, &endpoints, &defs, &declined);

        assert_eq!(sweep.failed_endpoints, 1, "one slot came back empty");
        // The survivors keep their place, and the failure between them does not
        // shift anything.
        assert_eq!(
            sweep.rows.iter().map(|r| r.endpoint.as_str()).collect::<Vec<_>>(),
            vec![a, a, c, c],
            "the survivors' rows stay in input order"
        );
        assert_eq!(
            sweep.declarations_read.iter().map(|d| d.endpoint.as_str()).collect::<Vec<_>>(),
            vec![a, c],
            "the failed endpoint publishes no declarationsRead: whether its description \
             was readable is exactly what this run did not find out"
        );
        assert_eq!(
            sweep.content_samples.iter().map(|s| s.endpoint.as_str()).collect::<Vec<_>>(),
            vec![a, c]
        );

        // The failed endpoint's own facts: one `NotMeasured` per metric that
        // would have run, saying the prober failed, plus its `CostCeiling`
        // facts unchanged. Not `Indeterminate` rows: nothing was observed.
        assert_eq!(
            sweep
                .not_measured
                .iter()
                .filter(|n| n.endpoint == b)
                .map(|n| (n.metric_id.as_str(), n.reason))
                .collect::<Vec<_>>(),
            vec![
                ("availability", NotMeasuredReason::ProberFailed),
                ("has-classes", NotMeasuredReason::ProberFailed),
                ("classes", NotMeasuredReason::CostCeiling),
            ]
        );
        assert!(
            !sweep.rows.iter().any(|r| r.endpoint == b),
            "a failed endpoint gets no measurement row: an Indeterminate one would assert \
             that a measurement happened and was inconclusive"
        );

        // No (endpoint, metric) pair carries two `NotMeasured` facts, which is
        // what would make `emit`'s duplicate-subject guard publish neither.
        let mut pairs: Vec<(&str, &str)> = sweep
            .not_measured
            .iter()
            .map(|n| (n.endpoint.as_str(), n.metric_id.as_str()))
            .collect();
        let before = pairs.len();
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(pairs.len(), before, "a pair got two NotMeasured facts");

        // The property the whole arrangement exists for: every endpoint the
        // sweep covered contributes at least one fact. An empty slot would let
        // `web/queries/endpoint_measurements.rq` keep serving last night's
        // verdicts as current, with nothing anywhere saying this run failed.
        for ep in &endpoints {
            assert!(
                sweep.rows.iter().any(|r| &r.endpoint == ep)
                    || sweep.not_measured.iter().any(|n| &n.endpoint == ep),
                "{ep} contributed nothing to the run graph"
            );
        }
    }
}
