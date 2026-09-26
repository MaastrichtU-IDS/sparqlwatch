pub mod emit;
pub mod verdict;
pub mod budget;
pub mod client;
pub mod declare;
pub mod dormancy;
pub mod media;
pub mod observe;
pub mod politeness;
pub mod profile;
pub mod metrics;
pub mod registry;
pub mod resolve;
pub mod seed;
pub mod state_file;
pub mod write;

use crate::budget::{Budget, Expired};
use crate::client::Client;
use crate::declare::{parse_declarations_for, Declarations};
use crate::emit::{
    ContentProfile, ContentSample, DeclarationsRead, EndpointFacts, MeasurementRow,
    NotMeasured, NotMeasuredReason, ProfileProperty,
    RunId,
};
use crate::metrics::{MetricDef, ProbeKind};
use crate::profile::{profile_classes, ProfileOutcome, Sampling};
use crate::politeness::host_key;
use crate::resolve::{is_a_readable_result, resolve, resolve_fetch, Declared};
use crate::verdict::Verdict;
use crate::write::RunWriter;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Everything one sweep produced, flat and ready to emit. For an endpoint the
/// sweep measured: one entry per (endpoint, metric) in `rows`, one entry in
/// `declarations_read`, and one per (endpoint, declined metric) in
/// `not_measured`. See `run_sweep` for what each of them means and how the
/// budgets shape them.
///
/// For an endpoint the sweep FAILED on, all three read differently, and
/// `assemble_endpoint` is where that is decided: no row and no `declarations_read`
/// entry, because nothing was observed, and `not_measured` carries every
/// metric that would have run (reason `prober-failed`) as well as the declined
/// ones (reason `cost-ceiling`). `failed_endpoints` counts those endpoints, so
/// a consumer that needs "one row per (endpoint, metric)" has to read it
/// rather than assume it.
pub struct Sweep {
    pub rows: Vec<MeasurementRow>,
    pub declarations_read: Vec<DeclarationsRead>,
    /// Two disjoint families of fact, not one: the metrics `main.rs` declined
    /// at the cost ceiling, and the metrics that would have run on an endpoint
    /// the sweep failed on. `NotMeasuredReason` is what tells them apart, and
    /// one (endpoint, metric) pair is only ever in one of them.
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
/// characterise. One endpoint per host in flight removes that term for the host
/// an endpoint NAMES, which is what the registry gives us: the grouping key
/// here is the same `host_key` that `politeness::acquire` gates on, so two
/// endpoints in different groups can never contend for one guard.
///
/// One case is left open, and this arrangement cannot close it.
/// `client::gated_hop` takes the gate for the host each HOP touches, and a
/// `Location` can point anywhere, so an endpoint that redirects into another
/// host being swept at the same time does wait on that host's guard, for up to
/// the same hold. Grouping cannot prevent it: the redirect target is unknown
/// until the endpoint is probed, and following it ungated is the thing
/// `gated_hop` exists to prevent. `README.md`'s known limitations price what
/// that wait costs.
///
/// It also settles dispatch order without a heuristic: grouping by host is what
/// spreads the work, so no round-robin is needed, and "concurrency only buys
/// parallelism across different hosts" is a property of the code rather than an
/// intention.
///
/// Output order is INPUT order, never completion order: each endpoint keeps its
/// input index as a slot and `collect_sweep` walks the slots afterwards. A
/// sweep's returned output therefore does not depend on which host answered
/// first. The FILE is a different matter and deliberately so: it is written in
/// completion order, one chunk per endpoint as that endpoint finishes, which is
/// the whole reason a crash costs only the endpoints not yet written.
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
/// Alongside the rows, every endpoint this sweep OBSERVED gets exactly one
/// `DeclarationsRead` fact: whether its queryless description fetch produced a
/// parseable graph of at least one triple. `Declared::claimed` cannot answer
/// this on its own -- `false` there means either "declares nothing" or "we
/// could not read it", and the two are distinguished only by this fact.
/// `EndpointSweep::declarations_read` starts `false` and is set inside
/// `probe_endpoint` as soon as the fetch's `Declarations` are known, so an
/// endpoint whose budget expires before that point (never fetched at all)
/// still gets a fact, honestly `false`, rather than none.
///
/// An endpoint whose task never returned gets NO such fact, which is the one
/// exception and is deliberate: a budget expiry means we fetched or tried to
/// fetch, so `false` is an honest report of the parse, whereas a failed task
/// means we do not know whether the description was readable, and `false`
/// would be an assertion about a fetch that may never have been made.
/// `assemble_endpoint` carries that decision and the unit test pins it.
///
/// `declined` is the other half of the definition list: metrics the caller
/// decided not to run. This function does not filter and does not know what a
/// ceiling is -- `main.rs` calls `metrics::within_cost` and passes both halves
/// -- it simply records one `NotMeasured` per (endpoint, declined metric) so
/// the gap is published as a fact rather than left as a missing row. Keeping
/// the policy in one place and the mechanism in another is deliberate: a
/// second reason for declining a metric changes `main.rs` and the reason enum,
/// not this loop.
// EIGHT ARGUMENTS, one past clippy's heuristic, and the alternative was worse.
// Bundling `budget`, `concurrency` and `memory` into a settings struct would
// touch 57 call sites -- almost all of them tests spelling `Budget::default()`
// and a concurrency of one -- to satisfy a threshold rather than to make
// anything clearer. Every parameter here is a distinct thing the sweep needs
// and each is named at every call. If a ninth ever arrives, that is the signal
// to do the bundle properly rather than to raise the allowance again.
#[allow(clippy::too_many_arguments)]
pub async fn run_sweep<W: std::io::Write>(
    endpoints: &[String],
    defs: &[MetricDef],
    declined: &[(MetricDef, NotMeasuredReason)],
    client: &Arc<Client>,
    budget: Budget,
    concurrency: NonZeroUsize,
    memory: &crate::profile::ContentMemory,
    writer: &mut RunWriter<W>,
) -> anyhow::Result<Sweep> {
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
    let shared_memory = Arc::new(memory.clone());
    // Finished endpoints on their way to the writer. BOUNDED, and safe to bound
    // because the loop that drains it does no probing: a `send` that has to wait
    // holds its group's permit, which delays that host's next endpoint and can
    // deadlock nothing while the receiver is live. What the bound buys is
    // backpressure and nothing else: see `ARRIVALS_IN_FLIGHT` for what it does
    // NOT buy, which is a bound on the sweep's memory.
    let (arrived, mut arrivals) =
        tokio::sync::mpsc::channel::<(usize, EndpointSweep)>(ARRIVALS_IN_FLIGHT);
    let mut tasks: JoinSet<()> = JoinSet::new();
    // Which endpoints each task is probing, with their slots. `JoinSet::join_next`'s
    // error arm carries only a `JoinError`, whose only identifying information is
    // `JoinError::id()`, so without this map a task that panicked could not even
    // be logged with the endpoints it was probing. It is for the log alone: the
    // slots it covered are filled by `assemble_endpoint` from their own emptiness,
    // so a missing entry here costs a name in one log line and nothing in the
    // published graph.
    //
    // The slots are carried alongside the names because a group can panic having
    // already delivered some of its endpoints, and those endpoints' real chunks
    // are on disk. Logging the whole group would name endpoints that were
    // measured under a message saying they were not, so the log filters on the
    // slots that are still empty.
    let mut covering: HashMap<tokio::task::Id, Vec<(usize, String)>> = HashMap::new();
    for group in groups {
        let named: Vec<(usize, String)> = group.clone();
        // Everything the task touches is owned by it: `probe_one_endpoint`
        // borrows the endpoint, the definitions and the client, and a spawned
        // future must be `'static`.
        let permits = Arc::clone(&permits);
        let defs = Arc::clone(&shared_defs);
        let client = Arc::clone(client);
        // Shared rather than cloned per group: it is one map for the whole
        // sweep and every group reads it without writing.
        let memory = Arc::clone(&shared_memory);
        let arrived = arrived.clone();
        let handle = tasks.spawn(async move {
            // Held for the WHOLE group, not per endpoint: a permit is the
            // right to talk to this host at all, and the loop below takes it
            // once so a host's endpoints are probed as one stretch instead of
            // requeuing behind whatever else is waiting between them.
            //
            // That is a fairness choice, not an invariant, and it is worth
            // being clear which: per-host serialisation comes from
            // `politeness::acquire`, not from this permit, and the group is one
            // sequential task whichever way round it goes, so acquiring inside
            // the loop instead would still put exactly one of this host's
            // endpoints in flight. No group can be queued behind this host's
            // guard either, which is what the grouping above is for.
            //
            // The permit is held across a `Retry-After` stand-down too, because
            // the wait happens inside `politeness::acquire` under it: a host
            // that asked for an hour keeps its permit until the METRIC budget
            // cancels the wait, then does it again for the next endpoint in the
            // group. The metric budget is what cancels it and not the endpoint
            // budget, because every acquire happens inside one:
            // `probe_endpoint` wraps the description fetch and each per-metric
            // future in `budget.with_metric_budget`, so a stood-down host's
            // wait ends after 60s rather than 600s. Eight of those is the
            // most an endpoint can spend against the shipped `metrics.toml`:
            // the description fetch plus the seven metrics that issue a probe
            // of their own, `service-description` being `FetchWellKnown`,
            // which reads that same fetch's outcome rather than making a
            // request. So 480s against a 600s endpoint budget, and 420s at the
            // default cheap ceiling, where `classes` is declined. The endpoint
            // budget only becomes the
            // canceller for a definition file where the metric budget times the
            // number of metrics exceeds it. The cost per group is the one
            // `README.md` already states, a shared host costing the sum of its
            // endpoints however high `--concurrency` is set. Releasing the
            // permit while a host is stood down would need a task per endpoint,
            // which is the arrangement whose guard waits do not fit a metric
            // budget.
            let _permit = permits
                .acquire()
                .await
                .expect("the sweep owns this semaphore and never closes it");
            for (slot, ep) in group {
                let swept = probe_one_endpoint(&ep, &defs, &client, budget, &memory).await;
                // A send error means the receiver is gone, which means the sweep
                // is over: it either stopped on a write failure or was cancelled.
                // So this task returns quietly rather than reaching for
                // `.expect()` the way the semaphore above does. Turning a write
                // failure into a panic per group would re-enter the panic path
                // and publish `prober-failed` facts about endpoints that were
                // measured, which is a confident wrong answer.
                if arrived.send((slot, swept)).await.is_err() {
                    return;
                }
            }
        });
        covering.insert(handle.id(), named);
    }
    // The sweep's own sender, dropped BEFORE the loop below. Every group holds a
    // clone, so `recv` returns `None` when the last of them finishes; holding
    // this one across the loop would mean it never returns and the sweep hangs
    // after the last group. That is the most common way to write this shape
    // wrong, and it presents as a hang rather than as a failure.
    drop(arrived);

    // The run this file describes, cloned once so the loops below can borrow it
    // alongside the writer it belongs to.
    let run = RunId(writer.run().0.clone());
    let mut slots: Vec<Option<EndpointFactLists>> = endpoints.iter().map(|_| None).collect();
    while let Some((slot, swept)) = arrivals.recv().await {
        let facts = assemble_endpoint(&endpoints[slot], Some(swept), defs, declined);
        // On arrival, which is what makes a chunk the unit of loss: a sweep
        // killed here has published every endpoint that finished before this one
        // and nothing about the ones that have not. The slot keeps the same
        // facts, so the returned `Sweep` is still in input order.
        writer.write_endpoint(facts.chunk(&run))?;
        slots[slot] = Some(facts);
    }
    // Every task has finished by now: that is what closed the channel. So this
    // waits for nothing and the only thing left to learn from it is which groups
    // panicked.
    //
    // A panic costs the endpoints of that group the task had not yet sent, which
    // is at most the whole group and at least the one it panicked on: the ones it
    // already sent are on disk and in their slots. So the log names the group's
    // endpoints whose slot is still empty, which is exactly the set whose facts
    // the loop below publishes as `prober-failed`.
    while let Some(joined) = tasks.join_next().await {
        if let Err(failure) = joined {
            let lost = covering
                .get(&failure.id())
                .map(|group| endpoints_without_facts(group, &slots))
                .unwrap_or_default();
            tracing::error!(
                endpoints = ?lost, error = %failure,
                "the task probing these endpoints failed; every metric on them is published as \
                 not measured, reason prober-failed"
            );
        }
    }
    // Then, and only then, the endpoints nothing arrived for. After every real
    // chunk because they are the run's failures and a reader meets them in the
    // order they were learned, and before the footer `main.rs` writes because a
    // footer certifies a run whose endpoints are all in the file: an endpoint
    // that contributed nothing at all would leave the site serving the previous
    // run's verdicts as current.
    let mut per_endpoint = Vec::with_capacity(endpoints.len());
    for (slot, ep) in endpoints.iter().enumerate() {
        let facts = match slots[slot].take() {
            Some(arrived) => arrived,
            None => {
                let failed = assemble_endpoint(ep, None, defs, declined);
                writer.write_endpoint(failed.chunk(&run))?;
                failed
            }
        };
        per_endpoint.push(facts);
    }
    Ok(collect_sweep(per_endpoint))
}

/// The endpoints of one group that have no facts in `slots`.
///
/// The set a panicked group really lost, which is what the log above names and
/// what the loop below it publishes as `prober-failed`. The filter is the whole
/// function: a group's task sends each endpoint as it finishes, so a panic
/// costs only the endpoints it had not sent yet, and naming the whole group
/// would name endpoints whose chunks are already on disk under a message saying
/// they were published as not measured.
///
/// Generic over the slot's contents so a test can build slots without building
/// facts: the question is only which of them are empty.
fn endpoints_without_facts<'a, T>(
    group: &'a [(usize, String)],
    slots: &[Option<T>],
) -> Vec<&'a str> {
    group
        .iter()
        .filter(|(slot, _)| slots[*slot].is_none())
        .map(|(_, endpoint)| endpoint.as_str())
        .collect()
}

/// How many finished endpoints may be waiting to be written.
///
/// Small on purpose, and what it buys is backpressure: a host that answers fast
/// cannot run arbitrarily far ahead of the writer, so the queue of
/// finished-but-unwritten endpoints stays at 16 instead of growing toward one
/// entry per finished endpoint when the writer is the slower of the two.
///
/// It does NOT bound the sweep's memory, and an earlier version of this comment
/// claimed it did. It cannot: the returned `Sweep` holds every endpoint's facts
/// by design, so a 548-endpoint sweep retains all 548 at its peak whatever this
/// number is. Task 3's review measured a 30-endpoint sweep at this capacity as
/// `peak_queued=15, retained_at_end=30`. Keeping `Sweep` whole is the deliberate
/// trade: 39 call sites and roughly 220 assertions read its input order, and
/// what writing incrementally buys here is crash tolerance, not a smaller heap.
const ARRIVALS_IN_FLIGHT: usize = 16;

/// One endpoint's four fact families, owned, as they will be published.
///
/// The unit the writer takes and the unit the returned `Sweep` is built from,
/// which is what lets the two differ in order without differing in content: a
/// chunk is written when its endpoint arrives, and the `Sweep` is built from
/// the slots afterwards, in input order.
struct EndpointFactLists {
    endpoint: String,
    rows: Vec<MeasurementRow>,
    declarations_read: Vec<DeclarationsRead>,
    not_measured: Vec<NotMeasured>,
    content_samples: Vec<ContentSample>,
    content_profiles: Vec<ContentProfile>,
    /// Whether the sweep failed on this endpoint, so `Sweep::failed_endpoints`
    /// counts the same endpoints whose facts say `prober-failed`.
    failed: bool,
}

impl EndpointFactLists {
    /// This endpoint's chunk, borrowing rather than copying: `EndpointFacts` is
    /// a view over the four lists and the writer serializes it immediately.
    fn chunk<'a>(&'a self, run: &'a RunId) -> EndpointFacts<'a> {
        EndpointFacts {
            run,
            endpoint: &self.endpoint,
            rows: &self.rows,
            declarations_read: &self.declarations_read,
            not_measured: &self.not_measured,
            content_samples: &self.content_samples,
            content_profiles: &self.content_profiles,
        }
    }
}

/// Build one endpoint's facts from its slot: the per-endpoint builder the
/// writer is fed from.
///
/// Per endpoint rather than per sweep because the chunk is the unit of writing,
/// so this is called once as each endpoint arrives rather than once over all of
/// them at the end. It does three things and the third is the one to read
/// twice: it appends one `NotMeasured { CostCeiling }` per declined metric for
/// EVERY endpoint, measured or failed. Under the default `--max-cost cheap`
/// that fact is the only thing a run says about `classes`, and both
/// `README.md` and `web/app.py` hang their honesty on it.
fn assemble_endpoint(
    ep: &str,
    slot: Option<EndpointSweep>,
    defs: &[MetricDef],
    declined: &[(MetricDef, NotMeasuredReason)],
) -> EndpointFactLists {
    let mut facts = EndpointFactLists {
        endpoint: ep.to_string(),
        rows: Vec::new(),
        declarations_read: Vec::new(),
        not_measured: Vec::new(),
        content_samples: Vec::new(),
            content_profiles: Vec::new(),
        failed: slot.is_none(),
    };
    match slot {
        Some(swept) => {
            facts
                .declarations_read
                .push(DeclarationsRead {
                    endpoint: ep.to_string(),
                    read: swept.declarations_read,
                    classes: swept.declared_classes,
                    properties: swept.declared_properties,
                });
            facts.rows = swept.rows;
            facts.content_samples = swept.content_samples;
            facts.content_profiles = swept.content_profiles;
            // Extended and not assigned: the cost-ceiling loop below appends to
            // this same list, and either order of the two is fine as long as
            // neither overwrites the other.
            facts.not_measured.extend(swept.not_measured);
            // THE UNREACHED CLASSES ARE NOT PUBLISHED AS NotMeasured, and the
            // spec said they would be. Doing it properly is not possible with
            // this vocabulary and the half-right version would be worse:
            //
            //   * `NotMeasured` is keyed on (endpoint, METRIC), so it cannot
            //     name a class. One fact per class is not a shape it has.
            //   * A single fact for the metric would be WRONG whenever some
            //     classes were profiled and some were not, which is the normal
            //     partial case: it would say nothing was measured while the
            //     graph holds profiles beside it.
            //   * Neither existing reason fits. `CostCeiling` is about a
            //     declined metric and `ProberFailed` means the prober panicked.
            //
            // A reader derives the set instead, exactly: the metric's own
            // ContentSample lists every class the enumeration saw, and the
            // ContentProfile facts name the ones profiled. The difference is the
            // unreached set, and it needs no new fact to be honest.
            //
            // The enumeration's own failure is no longer part of that
            // subtraction: it publishes a NotMeasured fact carrying
            // `EnumerationFailed`, because a pass that produced no
            // ContentSample would otherwise leave nothing in the graph to
            // distinguish it from a metric nobody declared.
            //
            // So this list is read for nothing, and is kept because it names
            // per-class refusals the subtraction already covers. Dropping the
            // field is a separate change to `EndpointSweep`.
            let _ = swept.profile_unreached;
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
            for def in defs {
                facts.not_measured.push(NotMeasured {
                    endpoint: ep.to_string(),
                    metric_id: def.id.clone(),
                    reason: NotMeasuredReason::ProberFailed,
                });
            }
        }
    }
    // One fact per (endpoint, declined metric), recorded whatever the budget
    // did: every reason here was decided before any probing started -- the
    // cost ceiling and the sweep's cadence both -- so an endpoint whose budget
    // expired still owes the reader an account of the metrics it was never
    // going to run. The reason travels WITH each metric rather than being
    // assumed, because an hourly sweep declines for two different reasons at
    // once and telling an operator "too expensive" about a metric that is
    // merely tonight's question would be a wrong answer.
    for (def, reason) in declined {
        facts.not_measured.push(NotMeasured {
            endpoint: ep.to_string(),
            metric_id: def.id.clone(),
            reason: *reason,
        });
    }
    facts
}

/// Flatten the per-endpoint lists into the four the caller reads, in the order
/// they are given.
///
/// `run_sweep` gives them in INPUT order, from the slots, which is the property
/// 39 call sites and about 220 assertions read. The file, written on arrival, is
/// in completion order. Those are two different properties and both hold: the
/// writer sees an endpoint once, when it finishes, and this sees all of them
/// once, afterwards.
fn collect_sweep(per_endpoint: Vec<EndpointFactLists>) -> Sweep {
    let mut sweep = Sweep {
        rows: Vec::new(),
        declarations_read: Vec::new(),
        not_measured: Vec::new(),
        content_samples: Vec::new(),
        failed_endpoints: 0,
    };
    for facts in per_endpoint {
        if facts.failed {
            sweep.failed_endpoints += 1;
        }
        sweep.rows.extend(facts.rows);
        sweep.declarations_read.extend(facts.declarations_read);
        sweep.not_measured.extend(facts.not_measured);
        sweep.content_samples.extend(facts.content_samples);
    }
    sweep
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
    /// The classes and properties the description named. Empty until the fetch
    /// parses, like `declarations_read` starting `false`, so an endpoint whose
    /// budget expired before the fetch finished publishes an honest nothing
    /// rather than an unset value.
    declared_classes: Vec<String>,
    declared_properties: Vec<String>,
    /// Whether a class profile pass asked this endpoint what classes it holds
    /// AND got a readable answer, whatever that answer was.
    ///
    /// Recorded explicitly rather than inferred from the ContentSample, which
    /// was the first attempt and is wrong: the pass publishes no sample when it
    /// finds nothing, on purpose, so that a size of 0 cannot be misread as a
    /// finding. Inferring from the sample therefore reported "we never looked"
    /// for an endpoint that looked and found none, which is the exact
    /// distinction `Observation::derived` exists to keep.
    profile_pass_enumerated: bool,
    /// This endpoint's share of `Sweep::content_samples`, under the same rules.
    content_samples: Vec<ContentSample>,
    /// This endpoint's class profiles, under the same rules: whatever the pass
    /// finished before the endpoint budget expired.
    content_profiles: Vec<ContentProfile>,
    /// Facts about metrics this endpoint's own probing decided not to measure,
    /// as opposed to the ones the cost ceiling declined before probing began.
    /// Only the class profile pass writes here, when its enumeration did not
    /// answer.
    ///
    /// It lives on the accumulator and not on the fact lists for the reason
    /// every field here does: the pass runs inside the endpoint budget, so a
    /// fact it recorded before an expiry has to survive the cancellation that
    /// drops the future.
    not_measured: Vec<NotMeasured>,
    /// Per profile metric, the classes the pass did not profile.
    ///
    /// Published as `NotMeasured` rather than omitted, because a reader who sees
    /// no profile for a class cannot otherwise tell "this class carries no
    /// properties" from "we never asked". An empty class list means the
    /// enumeration itself never answered, so nothing is known about any class.
    profile_unreached: Vec<(String, Vec<String>)>,
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
    memory: &crate::profile::ContentMemory,
) -> EndpointSweep {
    let mut acc = EndpointSweep::default();
    let outcome = budget
        .with_endpoint_budget(probe_endpoint(ep, defs, client, budget, memory, &mut acc))
        .await;
    if outcome.is_err() {
        // WHICH METRICS ARE MISSING, ASKED BY NAME. This was
        // `defs.iter().skip(acc.rows.len())` until 2026-09-18, which treats the
        // row count as a position in `defs` -- true only if every definition
        // pushes exactly one row, and two do not: `probe_endpoint` `continue`s
        // without a row for `ClassProfile` and `VocabularyDescribed`, whose
        // pass publishes samples and profiles instead of a verdict.
        //
        // So on an exhaustive sweep, eleven definitions produced nine rows, the
        // skip landed two short, and the expiry re-marked the last two metrics
        // -- graph-count and class-count -- that had ALREADY been measured. Two
        // rows for one pair, disagreeing, and `emit`'s duplicate guard then
        // correctly refused to publish either: a measurement that succeeded was
        // destroyed by a timeout that happened after it. Seen on five of
        // seventy-four endpoints in the 2026-09-18 profile pass, all of them
        // slow enough to expire during the profile phase, which runs after the
        // metric loop has finished every metric it has.
        //
        // Reading the ids off the rows cannot drift that way: it asks the
        // accumulator what it actually holds instead of reconstructing it from
        // a count.
        let measured: std::collections::HashSet<&str> =
            acc.rows.iter().map(|r| r.metric_id.as_str()).collect();
        // A metric that never produces a row does not get one HERE either. An
        // `Indeterminate` row for a profile metric would put it in the matrix
        // as a column of verdicts it does not have, which is exactly what
        // `dispatched_per_metric` is consulted for in the loop itself; the
        // profile pass reports its own unfinished work through
        // `acc.not_measured` and `acc.profile_unreached`.
        let unreached: Vec<&MetricDef> = defs
            .iter()
            .filter(|d| d.kind.dispatched_per_metric() && !measured.contains(d.id.as_str()))
            .collect();
        tracing::warn!(
            endpoint = %ep,
            reached = measured.len(),
            // Counted over the definitions that CAN produce a row, so that
            // "9 of 11" cannot describe a sweep where all nine were measured
            // and the two that were not are metrics which never yield one.
            of = defs.iter().filter(|d| d.kind.dispatched_per_metric()).count(),
            unreached = unreached.len(),
            "endpoint budget expired; unmeasured metrics are indeterminate"
        );
        // The budget expiring tells us nothing about the metrics we never
        // got to, and we did not measure their elapsed time either. Route
        // the verdict through `resolve` rather than writing one here:
        // judgement belongs in one place, and `resolve` already maps an
        // expired budget to `Indeterminate`.
        for def in unreached {
            acc.rows.push(MeasurementRow {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                verdict: resolve(def, Declared { claimed: false, value: None }, Err(Expired)),
                level: None, declared_count: None, observed_count: None,
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
    memory: &crate::profile::ContentMemory,
    acc: &mut EndpointSweep,
) {
    // THE REACHABILITY GATE, and it runs before anything else sends a byte.
    //
    // One request decides whether the other nine are worth sending. If nothing
    // answers it, nothing will answer them either: there is no state of the
    // world where DNS does not resolve and a CORS preflight still lands.
    // Before this, a dead endpoint cost the full battery every sweep --
    // bio2rdf.org, whose zone stopped resolving on 2026-09-13, was costing
    // ~68 seconds a sweep to be told nine times that it was not there.
    //
    // WHAT GATES AND WHAT DOES NOT. `status.is_some()` means an HTTP response
    // came back, whatever it said: an HTML console, a 500, a 404 all get the
    // whole battery, because CORS headers and a service description are plain
    // HTTP facts that can be true of a host whose query engine is refusing
    // work. Three of the nine endpoints the DBpedia KG catalog declares are
    // exactly that shape.
    //
    // Two things gate: nothing answered, and nothing answered IN TIME. The
    // second is a policy call rather than an observation about the network --
    // an endpoint taking 15 seconds over `LIMIT 1` is reachable -- and it is
    // made because it is still true that it will not answer nine harder
    // questions inside their budgets. The decline is named `LivenessFailed`
    // and not `Unreachable` for exactly that reason: the published fact has to
    // be true of the slow case too.
    //
    // The observation is kept rather than thrown away: it IS the availability
    // measurement, so a reachable endpoint pays for this request once and the
    // loop below reads the result instead of asking again.
    let live_def = defs.iter().find(|d| d.kind == ProbeKind::Liveness);
    let mut prefetched_liveness = match live_def {
        Some(def) => {
            let q = def.query.clone().unwrap_or_default();
            Some(budget.with_metric_budget(client.ask(ep, &q)).await)
        }
        // No liveness metric in this run's definition set -- a cost ceiling can
        // decline it, and a hand-built set may omit it. Nothing to gate on, so
        // nothing is gated: probe as before rather than inventing a reason to
        // skip an endpoint nobody asked us to test for reachability.
        None => None,
    };
    let reachable = match &prefetched_liveness {
        Some(Ok(o)) => o.status.is_some(),
        // OUR BUDGET EXPIRING IS NOT THE ENDPOINT'S SILENCE. An earlier draft
        // gated here too, on the reasoning that a host too slow for `LIMIT 1`
        // will not answer nine harder questions. That is probably true and it
        // is still the wrong place to act on it, for two reasons the tests
        // found. It would publish a decline about a server that is merely
        // under load, which is the one population a monitor must be most
        // careful about; and it would silently take away the partial-endpoint
        // property that `a_partial_endpoint_keeps_the_verdicts_it_already_earned`
        // exists to hold, where a metric that answered before a stall keeps
        // the verdict it earned.
        //
        // The measurement that settled it, taken 2026-09-14 over the ten
        // endpoints the DBpedia KG catalog declares: transport failures came
        // back in 11-24 ms and every live endpoint answered within 2.25 s.
        // Nothing sat in between. A short bound was solving a case the data
        // does not show, while risking the case it does.
        Some(Err(_)) => true,
        None => true,
    };

    if !reachable {
        // Availability still gets its row: we asked, and "nothing answered" is
        // what we found out. `Declared` is empty because no description was
        // read -- we did not fetch one -- and liveness declares nothing in the
        // service-description vocabulary anyway.
        if let (Some(def), Some(observed)) = (live_def, prefetched_liveness.as_ref()) {
            acc.rows.push(MeasurementRow {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                verdict: resolve(
                    def,
                    Declared { claimed: false, value: None },
                    observed.as_ref().map_err(|e| *e),
                ),
                level: None,
                declared_count: None,
                observed_count: None,
                // An expired bound measured nothing, so it reports no elapsed
                // time rather than a zero one -- the same rule every other row
                // follows.
                elapsed_ms: observed.as_ref().ok().map(|o| o.elapsed_ms),
            });
        }
        // Everything else is declined, with the reason naming the endpoint
        // rather than us or our budget. Not `Indeterminate` rows: an
        // indeterminate verdict asserts a measurement happened and was
        // inconclusive, and no measurement happened here.
        for def in defs {
            if live_def.is_some_and(|l| l.id == def.id) {
                continue;
            }
            acc.not_measured.push(NotMeasured {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                reason: NotMeasuredReason::LivenessFailed,
            });
        }
        return;
    }

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
    // Cloned out of the parsed declarations here, where they exist, because
    // `Declarations` is dropped at the end of this function and the fact is
    // assembled by the caller.
    acc.declared_classes = declarations.partitioned_classes.iter().cloned().collect();
    acc.declared_properties = declarations.partitioned_properties.iter().cloned().collect();
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
                // Not a counting metric: it grades a description's
                // informativeness, which is a level rather than a number.
                declared_count: None,
                observed_count: None,
                elapsed_ms: fetch_elapsed,
            });
            continue;
        }
        // A profile metric produces NO MEASUREMENT ROW, which is why it is
        // skipped here rather than dispatched. Its pass runs after this loop,
        // over classes it discovers itself, and publishes ContentSample and
        // ContentProfile facts instead of a verdict. Ruling 2 and Ruling 4 in
        // docs/superpowers/specs/2026-08-29-content-profiles-design.md.
        //
        // `continue` and not a row: a row would put this metric in the matrix as
        // a column of verdicts it does not have, which is the column Ruling 4
        // exists to remove.
        if !def.kind.dispatched_per_metric() {
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
                level: None, declared_count: None, observed_count: None,
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
                // The one kind with a fallback today. See MetricDef::fallback_query:
                // the default/named-graph UNION these content queries need is
                // rejected outright by stores with no named-graph support, and
                // before the fallback that read as `indeterminate` rather than as
                // the answer the simpler query gives.
                ProbeKind::AskData => client
                    .ask_literal_with_fallback(
                        ep,
                        &q,
                        def.fallback_query.as_deref(),
                        var.as_deref().expect(VAR_REQUIRED),
                    )
                    .await,
                ProbeKind::SelectIris => client.select_iris(ep, &q, var.as_deref().expect(VAR_REQUIRED)).await,
                // One aggregate, read as a literal. `ask_literal` already
                // extracts literal bindings and a COUNT comes back as one, so
                // this needs no new client path. It does need the metric's own
                // `var`: a caller passing the wrong name gets a silent empty
                // result, which reads as "counted nothing".
                // Asks which SHAPE of dataset this is before counting it.
                // See MetricDef::overlap_probe: a union default graph makes
                // the default/named UNION count everything twice, and this
                // project published `declared-but-wrong` against an honest
                // 335-million-triple declaration because of it.
                ProbeKind::Counted => client
                    .counted_by_shape(
                        ep,
                        &q,
                        def.overlap_probe.as_deref(),
                        def.overlap_query.as_deref(),
                        var.as_deref().expect(VAR_REQUIRED),
                    )
                    .await,
                // The two probes that announce an `Origin`, so the others
                // cannot be perturbed by a server that filters on it.
                ProbeKind::Cors => client.cors(ep, &q).await,
                // Takes no query: a preflight carries none, and this probe
                // asks whether a browser would be allowed to send one at all.
                ProbeKind::CorsPreflight => client.preflight(ep).await,
                ProbeKind::Liveness => client.ask(ep, &q).await,
                ProbeKind::AskFilter => client.ask(ep, &q).await,
                ProbeKind::FetchWellKnown => unreachable!("FetchWellKnown is handled once per endpoint before the per-metric dispatch"),
                ProbeKind::ClassProfile => unreachable!("ClassProfile is handled after the per-metric dispatch, and pushes no measurement row"),
                ProbeKind::VocabularyDescribed => unreachable!("VocabularyDescribed sends no request; it is derived after the profile pass"),
            }
        };
        // The gate above already asked the liveness question, under its own
        // shorter bound, and its answer is what let us get this far. Taking it
        // here rather than awaiting `fut` is what keeps the gate free for a
        // reachable endpoint: without this, every endpoint would pay for the
        // availability request twice, and an operator's log would show two
        // identical queries a few hundred milliseconds apart.
        //
        // `take()` so it is consumed once. A definition set with two Liveness
        // metrics -- which nothing forbids -- gets one prefetched answer and
        // then probes the second normally, rather than silently publishing one
        // observation under two metric ids.
        let observed = match (def.kind, prefetched_liveness.take()) {
            (ProbeKind::Liveness, Some(o)) => o,
            _ => budget.with_metric_budget(fut).await,
        };
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
        // The two numbers a counting metric compared, published beside its
        // verdict. Without them `verified` says a declaration was right and
        // never what it said, and a consumer asking how big an endpoint is
        // gets a grade rather than a number.
        //
        // Read from the same places `resolve` read them, so the published pair
        // is the pair that was actually compared rather than a second reading
        // that could differ. None for every other kind.
        let (declared_count, observed_count) = if def.kind == ProbeKind::Counted {
            let counted = observed
                .as_ref()
                .ok()
                .filter(|o| is_a_readable_result(o))
                .and_then(|o| o.bindings.first())
                .and_then(|b| b.trim().parse::<u64>().ok());
            (declared.value, counted)
        } else {
            (None, None)
        };
        acc.rows.push(MeasurementRow {
            endpoint: ep.to_string(),
            metric_id: def.id.clone(),
            verdict,
            level: None,
            declared_count,
            observed_count,
            elapsed_ms: elapsed,
        });
    }

    // ---------------------------------------------------------------------
    // The class profile pass, after every metric row and never through one.
    //
    // Last on purpose. It is the most expensive thing this function does, and
    // the endpoint budget cancels whatever is running when it expires, so
    // putting it here means an expiry costs the profiles rather than the
    // verdicts. A reader loses the answer to "what is in this endpoint" and
    // keeps the answer to "does it work", which is the right way round.
    // ---------------------------------------------------------------------
    // THE SECOND GATE. The first asked whether anything is there; this asks
    // whether what is there has moved since the last pass. It reads counts the
    // loop above already measured, so it costs no request of its own and only
    // the pass it skips is saved -- up to 200 queries against one endpoint.
    //
    // Read from `acc.rows` rather than re-queried, for the reason every other
    // derived fact on this page is: a second reading could differ from the one
    // that was published, and then the gate would be deciding on a number
    // nobody can see.
    let observed_count = |id: &str| -> Option<u64> {
        acc.rows.iter().find(|r| r.metric_id == id).and_then(|r| r.observed_count)
    };
    let reprofile = memory.worth_reprofiling(
        ep,
        observed_count(crate::metrics::TRIPLE_COUNT_METRIC),
        observed_count(crate::metrics::CLASS_COUNT_METRIC),
    );

    for def in defs.iter().filter(|d| d.kind == ProbeKind::ClassProfile) {
        if !reprofile {
            // Published rather than silently omitted. What the store holds
            // about this endpoint's vocabulary is the previous pass's, and a
            // reader who cannot tell "we looked and it is the same" from "we
            // never looked" has been told the wrong thing by an absence.
            acc.not_measured.push(NotMeasured {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                reason: NotMeasuredReason::Unchanged,
            });
            continue;
        }
        let enumeration = def.query.clone().unwrap_or_default();
        let var = def.var.clone().unwrap_or_else(|| "c".to_string());
        let observed = budget
            .with_metric_budget(client.select_iris(ep, &enumeration, &var))
            .await;
        // The gate is `is_a_readable_result` and not `Ok`, because `Ok` here
        // means only that the metric budget did not expire: an endpoint that
        // answered 500 arrives as `Ok` carrying an error body. The same
        // predicate gates the `SelectIris` arm of `resolve()`, which reads this
        // identical evidence shape, so the two decide alike by construction.
        let classes = match &observed {
            Ok(o) if is_a_readable_result(o) => o.bindings.clone(),
            // The enumeration did not answer: the budget expired, or the
            // endpoint refused, or the body was not a result set. Nothing is
            // known about this endpoint's classes, so nothing is published
            // about them; an empty class list would read as "this endpoint has
            // no classes".
            //
            // This fact is the ONLY thing the pass leaves behind here. The
            // metric publishes no measurement row, so without it a reader
            // cannot tell a pass that tried and failed from a metric nobody
            // declared, and every profile fact below is absent either way.
            _ => {
                acc.not_measured.push(NotMeasured {
                    endpoint: ep.to_string(),
                    metric_id: def.id.clone(),
                    reason: NotMeasuredReason::EnumerationFailed,
                });
                acc.profile_unreached.push((def.id.clone(), Vec::new()));
                continue;
            }
        };
        // The enumeration answered, whatever it answered. Set before the
        // empty check below, because "answered with none" is an answer.
        acc.profile_pass_enumerated = true;
        if classes.is_empty() {
            // A readable result that bound nothing, so the endpoint really has
            // no typed subjects and there is nothing to profile. NOT an
            // enumeration failure: it answered, and the answer was "none".
            //
            // Publishing nothing leaves a reader unable to tell this from a
            // pass that never ran, and the honest fact would be a sample with
            // zero values. The prober skips those on purpose so that a size of
            // 0 cannot be misread as a finding, while web/endpoint_content.py
            // documents `sampled` with an empty list as exactly this case and
            // has a fixture for it. The two tiers disagree, and settling that
            // changes the meaning of a published fact, so it is not settled
            // here.
            continue;
        }

        // The classes themselves are published as a ContentSample, exactly as
        // the `classes` metric published them before Ruling 4 retired its
        // verdict. Same fact, same shape, now produced by the pass that uses it.
        if let Some(limit) = def.sample_limit {
            acc.content_samples.push(ContentSample {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                values: classes.clone(),
                truncated: classes.len() >= limit,
            });
        }

        let sampling = def
            .sample_prefix
            .clone()
            .filter(|p| !p.is_empty())
            .map_or(Sampling::Exact, Sampling::HashPrefix);
        let mut outcome = ProfileOutcome::default();
        profile_classes(ep, &classes, sampling, client, budget, &mut outcome).await;
        // On BOTH paths, because a cancelled pass cannot name its own tail and a
        // completed one still has refusals to name. `name_unreached` is
        // idempotent so calling it here cannot double-name anything.
        outcome.name_unreached(&classes);

        for profile in outcome.profiles {
            acc.content_profiles.push(ContentProfile {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                class: profile.class,
                sampling: profile.sampling.slug().to_string(),
                sampling_prefix: profile.sampling.prefix().map(str::to_string),
                properties: profile
                    .rows
                    .into_iter()
                    .map(|r| ProfileProperty {
                        property: r.property,
                        subjects: r.subjects,
                        datatypes: r.datatypes,
                        any_datatype: r.any_datatype,
                    })
                    .collect(),
            });
        }
        if !outcome.unreached.is_empty() {
            acc.profile_unreached
                .push((def.id.clone(), outcome.unreached));
        }
    }

    // ---------------------------------------------------------------------
    // The content verdict, derived. No request is sent for it.
    // ---------------------------------------------------------------------
    //
    // Whether the endpoint DESCRIBES the vocabulary it uses, from two things
    // already in hand: the classes a `void:classPartition` named, and the
    // classes the profile pass found. The comparison is `resolve`'s, as every
    // other verdict's is, so this assembles the evidence and decides nothing.
    //
    // LAST, after the pass, because it grades the pass's results. It is also
    // after the endpoint budget may have expired, and that is the honest
    // outcome: a pass that never ran leaves no classes, the synthesised
    // observation says so, and `resolve` reads `indeterminate` rather than
    // claiming the endpoint describes nothing.
    for def in defs.iter().filter(|d| d.kind == ProbeKind::VocabularyDescribed) {
        // THE PASS THIS METRIC GRADES DID NOT RUN, so there is nothing to
        // grade and this publishes no verdict.
        //
        // Without this it published `indeterminate`, which is the verdict for
        // "we could not determine it" -- and the reason we could not is that we
        // deliberately chose not to re-derive it. That is the false negative
        // this project argues against everywhere else: `declared-but-wrong` is
        // withheld a few lines up in resolve.rs precisely because a capped,
        // sampled pass cannot support the harsh reading, and this was making
        // the harsh reading about ourselves instead.
        //
        // Measured on dev 2026-09-24: 54 of 74 endpoints read `indeterminate`
        // for this metric, every one of them carrying `class-profiles` declined
        // `unchanged` from the same sweep. None of them had failed at anything.
        //
        // A decline instead, with the SAME reason the pass gave, so the two
        // facts agree about why. The previous real verdict then survives in the
        // derived graph under the per-metric rule and is shown with its own
        // date, which is exactly what that rule is for.
        if !reprofile {
            acc.not_measured.push(NotMeasured {
                endpoint: ep.to_string(),
                metric_id: def.id.clone(),
                reason: NotMeasuredReason::Unchanged,
            });
            continue;
        }
        // The observation is synthesised rather than fetched. `bindings` is
        // what the pass FOUND; the status is 200 only when a pass actually
        // produced a class list, which is what tells "found nothing" apart
        // from "never looked".
        let found: Vec<String> =
            acc.content_profiles.iter().map(|p| p.class.clone()).collect();
        let looked = acc.profile_pass_enumerated;
        let observation = crate::observe::Observation::derived(found, looked);
        // `claimed` here is whether the description named ANY class, which is a
        // different question from the capability sets `Declared::from` reads,
        // so it is stated rather than looked up.
        let declared = Declared { claimed: !declarations.partitioned_classes.is_empty(), value: None };
        acc.rows.push(MeasurementRow {
            endpoint: ep.to_string(),
            metric_id: def.id.clone(),
            verdict: resolve(def, declared, Ok(&observation)),
            level: None,
            // Not a counting metric: it compares vocabularies, not numbers.
            declared_count: None,
            observed_count: None,
            // No request, so no time to report. A zero would read as the
            // fastest measurement in the dataset.
            elapsed_ms: None,
        });
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Cost, ProbeKind};

    /// What the panic log may name, and what it may not.
    ///
    /// A group's task sends each endpoint as it finishes, so a panic costs only
    /// the endpoints it had not sent yet. Naming the whole group instead would
    /// name endpoints whose chunks are already on disk, under a message saying
    /// every metric on them was published as not measured, which is a confident
    /// wrong answer in the log an operator reads after a failed sweep. This is
    /// the behaviour commit ec0f369 added and nothing held it.
    #[test]
    fn a_panicked_group_loses_only_the_endpoints_it_had_not_sent() {
        let group = vec![
            (0, "https://a.example/sparql".to_string()),
            (1, "https://b.example/sparql".to_string()),
            (2, "https://c.example/sparql".to_string()),
        ];
        // The task sent a and c before it panicked, so their facts are in their
        // slots and only b is lost.
        let slots = vec![Some(()), None, Some(())];

        assert_eq!(
            endpoints_without_facts(&group, &slots),
            vec!["https://b.example/sparql"],
            "an endpoint whose chunk is already written must not be named as lost"
        );
    }

    /// The other end of the same rule: a task that panicked before sending
    /// anything really did lose its whole group, and every one of them has to
    /// be named, because the loop after the log publishes exactly this set as
    /// `prober-failed`.
    #[test]
    fn a_group_that_sent_nothing_loses_all_of_it() {
        let group = vec![
            (0, "https://a.example/sparql".to_string()),
            (1, "https://b.example/sparql".to_string()),
        ];
        let slots: Vec<Option<()>> = vec![None, None];

        assert_eq!(
            endpoints_without_facts(&group, &slots),
            vec!["https://a.example/sparql", "https://b.example/sparql"]
        );
    }

    fn def(id: &str) -> MetricDef {
        MetricDef {
            id: id.into(),
            label: id.into(),
            dimension: "content".into(),
            kind: ProbeKind::Liveness,
            query: Some("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1".into()),
            fallback_query: None,
            overlap_probe: None,
            overlap_query: None,
            expect: None,
            var: None,
            declared_by: None,
            graded: false,
            cost: Cost::Cheap,
            cadence: Default::default(),
            sample_limit: None,
            sample_prefix: None, tolerance: None,
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
                    level: None, declared_count: None, observed_count: None,
                    elapsed_ms: Some(7),
                })
                .collect(),
            declarations_read: true,
            declared_classes: Vec::new(),
            declared_properties: Vec::new(),
            profile_pass_enumerated: false,
            content_samples: vec![ContentSample {
                endpoint: ep.to_string(),
                metric_id: defs[0].id.clone(),
                values: vec!["http://example.org/C".to_string()],
                truncated: false,
            }],
            // Empty on purpose: this helper stands for one endpoint's FINISHED
            // work under the shapes that existed before the profile pass, and
            // the tests using it are about the four lists a consumer reads. A
            // profile here would change what they assert without changing what
            // they are for.
            content_profiles: Vec::new(),
            profile_unreached: Vec::new(),
            not_measured: Vec::new(),
        }
    }

    /// A unit test on `assemble_endpoint` over already-normalised slots, and not
    /// on `run_sweep`, because `JoinError` has no public constructor: a fold
    /// typed over one cannot be tested at all, which is why the translation from
    /// a join failure to an empty slot happens at the `join_next` site and the
    /// judgement about an empty slot happens here.
    ///
    /// It goes through `collect_sweep` as well, because the contract under test
    /// is about the four lists a consumer reads, and the builder now produces
    /// one endpoint's share of them at a time. The three calls in input order
    /// are what `run_sweep` does with its slots after the drain loop.
    #[test]
    fn a_failed_group_still_carries_facts_for_every_endpoint_in_it() {
        let a = "https://a.example/sparql";
        let b = "https://b.example/sparql";
        let c = "https://c.example/sparql";
        let endpoints = vec![a.to_string(), b.to_string(), c.to_string()];
        let defs = vec![def("availability"), def("has-classes")];
        let declined = vec![(def("classes"), NotMeasuredReason::CostCeiling)];
        let slots = vec![Some(swept(a, &defs)), None, Some(swept(c, &defs))];

        let sweep = collect_sweep(
            endpoints
                .iter()
                .zip(slots)
                .map(|(ep, slot)| assemble_endpoint(ep, slot, &defs, &declined))
                .collect(),
        );

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
