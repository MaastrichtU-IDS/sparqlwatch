use clap::Parser;
use sparqlwatch_prober::{
    budget::Budget,
    client::Client,
    dormancy::{
        self, plan_sweep, Outcome, Thresholds, DEFAULT_CADENCE_DAYS, DEFAULT_COST_MS,
        DEFAULT_GRACE_DAYS, DEFAULT_STRIKES, MIN_COST_MS,
    },
    emit::{DormancyFact, MeasurementRow, NotMeasured, NotMeasuredReason, RunFooter, RunHeader, RunId},
    metrics::{definitions_revision, load_metrics, within_cost, Cost},
    politeness::{Politeness, DEFAULT_MIN_GAP, DEFAULT_RETRY_AFTER_CAP},
    registry::{load_endpoints, read_exclusions},
    run_sweep,
    state_file::{check_lock, merge_state, read_state, DEFAULT_STATE},
    verdict::Verdict,
    write::{partial_path, RunWriter},
    Sweep,
};
use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sparqlwatch-prober")]
struct Args {
    #[arg(long, default_value = "endpoints.toml")]
    endpoints: String,
    /// The exclusion list: the hosts somebody asked this project not to probe.
    /// Read from disk at every run, so an entry takes effect at the next sweep
    /// rather than at the next build. The sweep does not start if this file
    /// cannot be read, because the alternative to stopping is probing hosts
    /// that asked not to be probed.
    #[arg(long, default_value = sparqlwatch_prober::registry::DEFAULT_EXCLUSIONS)]
    exclusions: String,
    #[arg(long, default_value = "metrics.toml")]
    metrics: String,
    #[arg(long, default_value = "run.nq")]
    out: String,
    /// The run instant, spelled exactly `YYYY-MM-DDTHH:MM:SSZ`: UTC, with no
    /// offset, no fractional second and no leap second. Passed in rather than
    /// read from the clock so runs are reproducible.
    ///
    /// That form is narrower than ISO-8601 and it is enforced, so `--help` and
    /// the parser agree. A retry of a failed sweep reuses its `--at` by design,
    /// and the dormancy policy recognises that retry by comparing instants as
    /// STRINGS, so two spellings of one instant (`...00Z` beside `...00.000Z`,
    /// or `12:00:00Z` beside `14:00:00+02:00`) would give the retry a second run
    /// IRI and no replay match: it would strike an endpoint twice for one sweep
    /// and publish a run graph that disagrees with the first about what it
    /// skipped. See `validate_instant`, which refuses anything else before a
    /// request is sent.
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
    /// How many HOSTS to probe at once. Endpoints are grouped by host and each
    /// group is probed sequentially, so this bounds how many groups run
    /// together, never how many requests one host receives at once: that stays
    /// at one, gated by `--min-gap-ms`.
    ///
    /// Four by default, for politeness rather than throughput: four hosts in
    /// flight with a 2s per-host gap is roughly two requests per second in
    /// aggregate, which is a defensible load for a service that probes
    /// strangers uninvited.
    ///
    /// `NonZeroUsize`, so zero is refused here by the parser rather than
    /// repaired downstream: `Semaphore::new(0)` does not fail, it hangs, and a
    /// sweep that probes nothing and reports nothing is the worst failure this
    /// crate has.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY)]
    concurrency: NonZeroUsize,
    /// The dormancy state: which endpoints this sweep may decline to ask, and
    /// the file it writes this run's strikes and promotions back to.
    ///
    /// Read before any probing and it FAILS CLOSED. A state file that cannot be
    /// read is not an empty state: read as empty, a sweep would re-admit all 57
    /// relegated endpoints at roughly 210 s each, which is over three hours of
    /// probing nobody asked for, and would forget every hold placed by hand.
    ///
    /// `prober/state/` is git-ignored, because the file is machine-written and
    /// every sweep rewrites it, so a fresh checkout and a container image both
    /// need `dormancy init --state <path>` once. That command is the ONLY thing
    /// that creates the file: a sweep that created its own could not tell "first
    /// ever run" from "the volume holding the state did not get mounted", and
    /// the second of those is the case that quietly re-admits everything.
    #[arg(long, default_value = DEFAULT_STATE)]
    state: String,
    /// Above this, in summed per-metric `elapsed_ms` for one endpoint in one
    /// run, a sweep that produced no positive verdict for that endpoint is a
    /// strike against it.
    ///
    /// 60 s, and BOTH halves of that rule are required. The 2026-08-24 sweep of
    /// 543 candidates cost 3.46 serial hours; 57 endpoints were 3.12 of them,
    /// 90% of the total, and not one produced a positive verdict, while 339
    /// equally silent endpoints cost 0.05 h between them. So the policy is
    /// cost-weighted rather than failure-weighted: relegating on failure would
    /// relegate 459 endpoints to save 0.34 h. And three endpoints that DO answer
    /// cost 61 s, 67 s and 89 s, so a cost-only rule would relegate working
    /// endpoints at whatever threshold.
    ///
    /// Refused at startup below `dormancy::MIN_COST_MS`, which is one
    /// `Budget::request`: under that, an endpoint that merely ran out its
    /// request budget once would be a strike. The mistake the floor exists for
    /// is `--dormant-cost-ms 60`, seconds typed where the flag takes
    /// milliseconds, which relegates roughly 460 of the 543 over two sweeps.
    #[arg(long, default_value_t = DEFAULT_COST_MS)]
    dormant_cost_ms: u64,
    /// How many consecutive expensive silent sweeps relegate an endpoint.
    ///
    /// Two, not one. The stability data would justify one (98% of the 57 were
    /// over 60 s again when re-probed 39 hours later), and two costs one extra
    /// probe per endpoint; it is bought for exactly the `data.datahub.kr` case,
    /// which measured 210.0 s in one sweep and 5.7 s in the next.
    ///
    /// `NonZeroU32`, so the parser refuses zero rather than anything downstream
    /// repairing it: zero strikes would relegate an endpoint on the strength of
    /// no sweep at all.
    #[arg(long, default_value_t = DEFAULT_DORMANT_STRIKES)]
    dormant_strikes: NonZeroU32,
    /// How often a relegated endpoint is probed anyway, in whole days.
    ///
    /// Whole days rather than an instant comparison, deliberately: a sweep that
    /// starts fifteen minutes earlier than last week's is not a day early, and
    /// compared as instants the cadence would drift a little later every week
    /// for as long as sweeps kept starting early. This is the second of the two
    /// time granularities `dormancy.rs` documents and they are not unified; the
    /// first is replay detection, which compares `--at` strings exactly.
    ///
    /// It is a cadence per endpoint, not per sweep: the due set is divided into
    /// a slice sized by the gap since the last sweep, so daily sweeps take a
    /// seventh of the dormant set each and a weekly sweep takes all of it.
    #[arg(long, default_value_t = DEFAULT_DORMANT_CADENCE)]
    dormant_every_days: NonZeroU64,
}

/// The two counted dormancy defaults as values of their own `NonZero` types.
///
/// Named constants for the reason `DEFAULT_CONCURRENCY` records: `default_value_t`
/// needs a value of the field's type, and `NonZeroU32::new(2).unwrap()` inside
/// the attribute would put a `.unwrap()` in a flag declaration. The unwraps here
/// are on literal consts and are unreachable unless somebody edits one to zero,
/// which would not compile past this line. The tests still assert on the PARSED
/// args rather than on these, because asserting on a constant would pass even if
/// `default_value_t` were changed to name something else.
const DEFAULT_DORMANT_STRIKES: NonZeroU32 = NonZeroU32::new(DEFAULT_STRIKES).unwrap();
const DEFAULT_DORMANT_CADENCE: NonZeroU64 = NonZeroU64::new(DEFAULT_CADENCE_DAYS).unwrap();

/// The four numbers the admission policy is calibrated against, gathered from
/// the flags that carry them.
///
/// One place, so `plan_sweep` and `update` cannot be handed two different
/// policies inside one run: the plan decides what is probed and the update
/// decides what that means, and a sweep whose two halves disagreed would skip an
/// endpoint on one cadence and record it against another.
/// **`grace_days` comes from the policy and not from a flag, because a sweep
/// never reads it.** `Thresholds::grace_days` has exactly one reader,
/// `dormancy::wake`, which computes a hold's expiry once and stores it in the
/// state file as an instant; nothing in this binary calls `wake`. So the
/// `--dormant-grace-days` this field used to carry could be typed, parsed and
/// threaded through `plan_sweep` and `update` without changing anything at all,
/// which is worse than not offering it: an operator who set it would believe a
/// grace period had been shortened. The flag that DOES decide a grace is
/// `dormancy wake --grace-days`, on the binary that owns the decision. The value
/// is still filled in here rather than left out, because `Thresholds` is one
/// value the whole policy shares and a second constructor for it is how the two
/// halves of a sweep come to disagree.
fn thresholds_from(args: &Args) -> Thresholds {
    Thresholds {
        cost_ms: args.dormant_cost_ms,
        strikes: args.dormant_strikes,
        cadence_days: args.dormant_every_days,
        // Unwrap on a literal const, like the two above it: unreachable unless
        // somebody edits `DEFAULT_GRACE_DAYS` to zero, which is not a grace.
        grace_days: NonZeroU64::new(DEFAULT_GRACE_DAYS).unwrap(),
    }
}

/// Refuse a cost threshold under one request budget, before anything is probed.
///
/// The `NonZero` types cover the other three fields in the parser, so this
/// function has exactly one thing to check. It is a whole startup refusal
/// because the failure it prevents is silent and expensive: at
/// `--dormant-cost-ms 60`, every endpoint that spent more than 60 milliseconds
/// in a sweep earns a strike, which on the 2026-08-24 data is roughly 460 of 543
/// endpoints relegated over two sweeps. Nothing would report that as an error.
/// The run graphs would simply say, honestly and permanently, that this project
/// declined to ask almost every endpoint it monitors.
///
/// The floor is one `Budget::request` because below it an endpoint that merely
/// ran out its request budget once would be a strike, and running out a request
/// budget once is what a healthy-but-slow endpoint does.
fn validate_thresholds(thresholds: &Thresholds) -> anyhow::Result<()> {
    if thresholds.cost_ms < MIN_COST_MS {
        anyhow::bail!(
            "--dormant-cost-ms {} is under the floor of {MIN_COST_MS}, which is one request \
             budget. Below it an endpoint that merely ran out its request budget once would \
             earn a strike, and two such sweeps relegate it: at 60, meaning seconds typed \
             where this flag takes milliseconds, that is roughly 460 of 543 endpoints the \
             sweep would stop asking. Use a value at or above {MIN_COST_MS}.",
            thresholds.cost_ms,
        );
    }
    Ok(())
}

/// What this sweep measured, per endpoint it probed, for the dormancy write-back.
///
/// **One `Outcome` per PROBED endpoint, including one the prober failed on**,
/// whose cost then sums to zero. That is deliberate and it is the opposite of
/// the obvious reading. With no `Outcome` the endpoint gets no `last_probed`, so
/// a re-run of the same `--at` would not recognise it as part of that sweep:
/// `plan_sweep` rule 3 would skip it as `Automatic`, and the one endpoint most
/// worth retrying is the one that never gets retried. A zero cost lands in
/// `update`'s rule G, so a prober failure still earns no strike, which is the
/// property that actually matters.
///
/// An endpoint the sweep DECLINED to probe is absent, because an outcome is a
/// measurement and no measurement was taken; `update`'s rule D then carries its
/// entry forward untouched.
///
/// Collected through a map keyed on the url, so a duplicate in `probed` cannot
/// produce two outcomes for one endpoint. `update` refuses TWO OUTCOMES FOR ONE
/// URL outright (rule I: one of the two would decide a strike and the other
/// would be lost), and refusing at the END of a 90 minute sweep would cost the
/// whole run's state write. A row naming an endpoint that is not in `probed` is
/// dropped for the same reason: `run_sweep` cannot produce one, and `update`
/// would refuse an outcome whose url is outside the endpoint list it was handed.
///
/// The result comes back in url order rather than in probe order, which is a
/// consequence of the map and not a promise: `update` iterates its own endpoint
/// list and looks each url up, so nothing reads this order. It is deterministic
/// only so a test can assert on the whole vector.
///
/// **`elapsed_ms: None` contributes zero.** It is three different things in
/// `lib.rs` and only one of them spent any time: an expired ENDPOINT budget
/// fills in the metrics it never reached, a probe kind with no implemented probe
/// reports it without sending anything, and an expired METRIC budget reports it
/// because the observation was dropped. The case that DID spend time, a request
/// that ran out its own 30 s budget, comes back as a failed `Observation`
/// carrying its elapsed time, which is why the 57 endpoints behind this policy
/// measure 210,010 ms rather than nothing.
fn outcomes_from(
    rows: &[MeasurementRow],
    not_measured: &[NotMeasured],
    probed: &[String],
) -> Vec<Outcome> {
    let mut by_url: BTreeMap<&str, Outcome> = probed
        .iter()
        .map(|url| {
            (url.as_str(), Outcome { url: url.clone(), cost_ms: 0, positive: false, liveness_failed: false })
        })
        .collect();
    // The gate's verdict, read from the declines rather than re-derived from
    // the rows: `LivenessFailed` is the only reason meaning "it did not
    // answer", and reading it here keeps that definition in `probe_endpoint`.
    for fact in not_measured {
        if fact.reason == NotMeasuredReason::LivenessFailed {
            if let Some(outcome) = by_url.get_mut(fact.endpoint.as_str()) {
                outcome.liveness_failed = true;
            }
        }
    }
    for row in rows {
        let Some(outcome) = by_url.get_mut(row.endpoint.as_str()) else { continue };
        outcome.cost_ms = outcome.cost_ms.saturating_add(row.elapsed_ms.unwrap_or(0));
        // The six-verdict vocabulary is closed and dormancy is not a member of
        // it. These two are the ones that mean "we confirmed something", which
        // is what a promotion has to rest on; the other four do not.
        outcome.positive |=
            matches!(row.verdict, Verdict::Verified | Verdict::UndeclaredButVerified);
    }
    by_url.into_values().collect()
}

/// Four hosts at once. A named constant because `default_value_t` needs a
/// value of the field's own type, and `NonZeroUsize::new(4).unwrap()` inside
/// the attribute would put a `.unwrap()` in the flag declaration. The test
/// below still asserts on the PARSED args rather than on this constant, for
/// the reason the cost ceiling's test records: asserting on the constant would
/// pass even if `default_value_t` were changed to name something else.
const DEFAULT_CONCURRENCY: NonZeroUsize = NonZeroUsize::new(4).unwrap();

/// `--at` must be a UTC instant spelled exactly `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Three consumers, and the narrowest of them decides the grammar.
///
/// 1. It is interpolated into two IRIs and into the partial file's name, so a
///    value like `banana` would produce a nonsense graph name and a space would
///    break the IRI outright.
/// 2. It is published as an `xsd:dateTime`, so it has to be one.
/// 3. **`dormancy` recognises a re-run by comparing instants as STRINGS**, and
///    that is what rules out every other ISO-8601 spelling. A retry reuses its
///    `--at` by design; `plan_sweep` rules 2 and 3 and `update` rule B both ask
///    whether `last_probed == now` as text. Two spellings of one instant
///    (`...00Z` beside `...00.000Z`, or `12:00:00Z` beside `14:00:00+02:00`)
///    would give the retry a second run IRI and no replay match, so it would
///    strike an endpoint twice for one sweep and publish a run graph that
///    disagrees with the first about what it skipped. A leap second goes with
///    them: `23:59:60` is a valid `xsd:dateTime` and `dormancy`'s day arithmetic,
///    which exists so this crate needs no date dependency, has no room for it.
///
/// This is a shape check and not the authority. `dormancy::parse_instant` is,
/// and it re-checks the same string plus whether the date exists at all
/// (`2026-02-30` passes here and is refused there). That is safe because
/// `plan_sweep` runs before the run file is opened and before any request is
/// sent, so a date that only it refuses still stops the sweep at t=0. What this
/// function buys is the refusal happening before the state file is even read,
/// beside `--min-gap-ms`, where the message is about the flag rather than about
/// a field of a state file.
fn validate_instant(at: &str) -> anyhow::Result<()> {
    let bad = || {
        anyhow::anyhow!(
            "--at must be a UTC instant of exactly the form YYYY-MM-DDTHH:MM:SSZ, such as \
             2026-08-20T12:00:00Z, got {at:?}. No offset, no fractional second and no leap \
             second: this form is narrower than ISO-8601 on purpose, because a re-run of one \
             instant is detected by comparing these strings, so one instant may have only one \
             spelling"
        )
    };
    // ASCII first, so the fixed byte positions below are also character
    // positions and no index can land inside a multi-byte character.
    if !at.is_ascii() || at.len() != 20 {
        return Err(bad());
    }
    let bytes = at.as_bytes();
    for (index, expected) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'Z')]
    {
        if bytes[index] != expected {
            return Err(bad());
        }
    }
    let num = |at: usize, width: usize, lo: u32, hi: u32| -> anyhow::Result<u32> {
        let field = &bytes[at..at + width];
        if !field.iter().all(u8::is_ascii_digit) {
            return Err(bad());
        }
        let value = field.iter().fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0'));
        if (lo..=hi).contains(&value) {
            Ok(value)
        } else {
            Err(bad())
        }
    };
    num(0, 4, 0, 9999)?;
    num(5, 2, 1, 12)?;
    num(8, 2, 1, 31)?;
    num(11, 2, 0, 23)?;
    num(14, 2, 0, 59)?;
    // 59 and not 60. `xsd:dateTime` permits a leap second; the day arithmetic
    // in `dormancy` does not, and that is the consumer that decides.
    num(17, 2, 0, 59)?;
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
///
/// This stays the only budget relation a sweep needs BECAUSE one host is
/// probed by one task at a time (see `--concurrency`, which counts hosts). The
/// per-host guard in `politeness::acquire` is held until the request returns,
/// so a second endpoint of the same host would wait for the guard, not just
/// for the gap: gap plus the whole first request, and on a throttled host gap
/// plus request plus the honoured `Retry-After` plus the retry, which is the
/// 2 + 30 + 20 + 30 = 82 seconds priced out on `--retry-after-cap-s` above,
/// inside a 60-second metric budget. Relaxing per-host serialisation would
/// therefore need this check rewritten around that HOLD term rather than
/// around the gap.
///
/// Until then the term is absent for the host an endpoint NAMES: no endpoint
/// waits on another endpoint's guard for the host it was pointed at, because
/// `run_sweep` groups on the same `host_key` the gate acquires. It is NOT
/// absent for a redirect: `client::gated_hop` gates each hop on that hop's own
/// host, so an endpoint redirected into another host of the same sweep can
/// wait one hold there, and that wait is inside this budget. See the known
/// limitation in `README.md`; it is unclosable at dispatch time, since the
/// redirect target is only learned by following it.
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
    // Both flag validations first, before a file is read or a socket is opened,
    // for the same reason: each of them turns a typo into a sweep that publishes
    // confident nonsense, and neither is detectable afterwards.
    let thresholds = thresholds_from(&args);
    validate_thresholds(&thresholds)?;
    let budget = Budget::default();
    validate_min_gap(Duration::from_millis(args.min_gap_ms), budget)?;
    // Deduplicated by the loader: one URL listed twice would otherwise be
    // probed twice and publish two `declarationsRead` facts about one endpoint
    // IRI in one run graph, which can and do disagree.
    // Read before the endpoint list, so a sweep that cannot tell which hosts
    // asked to be left alone stops before it has looked at what to probe. An
    // unreadable or malformed list is an error and not an empty one: see
    // `registry::read_exclusions`.
    let excluded = read_exclusions(Path::new(&args.exclusions))?;
    let endpoints = load_endpoints(&std::fs::read_to_string(&args.endpoints)?, &excluded)?;

    // The state, then the lock, then the plan, and all three before the run file
    // is opened and before any request is sent: a state problem or a lock left
    // behind by a killed sweep costs a rerun here and costs 90 minutes of
    // probing plus every strike measured in it anywhere later.
    //
    // `read_state` BEFORE `check_lock`, and the order is not arbitrary. On a
    // fresh deployment `state/` does not exist, and `check_lock` reaches it
    // first would report that it cannot create the lock file. `read_state`'s
    // message for a missing state file is the one that deployment needs: it
    // names the path, says a relative path is resolved against the working
    // directory, and names `dormancy init` as the only thing that may create it.
    let state = read_state(Path::new(&args.state))?;
    // Not a reservation, deliberately. Between this check and the merge at the
    // end of the sweep another writer may take the lock, and `merge_state`
    // refuses rather than overwriting; holding the lock across the sweep instead
    // would lock out every operator command for 90 minutes, which is the thing
    // the lock protects. What this buys is that a lock left behind by a killed
    // sweep is found now rather than after the probing.
    check_lock(Path::new(&args.state))?;
    let plan = plan_sweep(&state, &endpoints, &args.at, &thresholds)?;
    tracing::info!(
        probing = plan.probe.len(),
        dormant = plan.skipped.len(),
        listed = endpoints.len(),
        "dormancy applied"
    );
    // Published on the header, so the run graph says what this sweep declined to
    // ask instead of leaving 57 endpoints unmentioned. Silence about an endpoint
    // would be read as a claim that nothing was found there.
    let dormant: Vec<DormancyFact> = plan
        .skipped
        .iter()
        .map(|skipped| DormancyFact {
            endpoint: skipped.url.clone(),
            dormant_since: skipped.dormant_since.clone(),
            reason: skipped.reason,
        })
        .collect();

    let defs = load_metrics(&std::fs::read_to_string(&args.metrics)?)?;
    // The real settings, from flags a reader can see. `Politeness::unlimited()`
    // exists for tests and must never appear here.
    let politeness = Politeness::with_retry_after_cap(
        Duration::from_millis(args.min_gap_ms),
        Duration::from_secs(args.retry_after_cap_s),
    );
    // An `Arc` because `run_sweep` spawns one task per host group and a spawned
    // future must be `'static`: every group holds its own handle on the one
    // client, which is also what keeps the per-host gate global to the sweep.
    let client = std::sync::Arc::new(Client::new(budget, politeness)?);

    // A pure function of the definitions, so the published revision is
    // reproducible from the same metrics.toml. It identifies the definitions,
    // all of them, not the subset this
    // run chose to probe: the ceiling is published separately, on the
    // activity, so two runs of one file at different ceilings stay comparable.
    let revision = definitions_revision(&defs);
    // The policy lives here, in one place. `run_sweep` receives both halves as
    // data and never learns what a ceiling is.
    let (run, declined) = within_cost(&defs, args.max_cost);
    let run_id = RunId(args.at.clone());
    // The file is opened and its header written BEFORE any probing, so a bad
    // `--out` is reported now rather than after a ten-minute sweep, and a sweep
    // killed before its first endpoint finished still leaves a loadable header.
    // It is a sibling of `--out`, not `--out` itself: see `write::RunWriter`.
    let partial = partial_path(Path::new(&args.out), &args.at);
    let mut writer = RunWriter::create(
        Path::new(&args.out),
        &args.at,
        RunHeader {
            run: &run_id,
            generated_at: &args.at,
            metric_revision: &revision,
            max_cost: args.max_cost,
            concurrency: args.concurrency,
            dormant: &dormant,
        },
    )?;
    tracing::info!(partial = %partial.display(), "writing this run as its endpoints finish");
    // Each endpoint's chunk reaches the file as that endpoint finishes, inside
    // this call. What comes back is the same four fact lists as before, in
    // input order, built from the slots after the last chunk was written.
    //
    // `&plan.probe`, and this is the line the whole dormancy stage exists for.
    // Not `&endpoints`: every other instruction in this stage can be satisfied
    // with the full list here, publishing a perfectly correct dormancy section
    // while still spending 90 minutes probing all 543. `plan.probe` is what the
    // policy admitted; `endpoints` is the full list, and it goes to `update`
    // below, which has to see every entry so a hold can lapse on a date rather
    // than on being swept.
    let Sweep { rows, declarations_read, not_measured, content_samples, failed_endpoints } =
        run_sweep(&plan.probe, &run, &declined, &client, budget, args.concurrency, &mut writer)
            .await?;
    // The footer, and then the rename onto `--out`. Last, because it publishes
    // `failedEndpoints`, which summarises the chunks, and because the rename is
    // what promises `--out` is a run that finished.
    writer.finish(RunFooter { run: &run_id, failed_endpoints })?;
    // `plan.probe.len()` and not `endpoints.len()`, here and in the bail below:
    // a run that probed 486 of 543 must not report its counts against 543, or a
    // sweep with one failure says "1 of 543 endpoints were not probed" when 486
    // were attempted.
    tracing::info!(endpoints = plan.probe.len(), dormant = dormant.len(),
                   measurements = rows.len(),
                   declarations_read = declarations_read.len(),
                   not_measured = not_measured.len(), content_samples = content_samples.len(),
                   max_cost = args.max_cost.slug(), concurrency = args.concurrency,
                   failed_endpoints, revision = %revision, out = %args.out, "sweep complete");

    // The dormancy write-back, after the footer and BEFORE the failure bail.
    // Before, because otherwise no sweep with a single prober failure would ever
    // record state, and the endpoints most likely to produce one are exactly the
    // expensive ones this policy is about.
    //
    // A closure and not a `State`: `merge_state` takes the lock, re-reads the
    // file inside it, and applies this to what is on disk at that moment. The
    // race is concrete. This sweep read state 90 minutes ago; an operator may
    // have run `dormancy sleep` in the middle of it, and writing back the state
    // this process read at the start would destroy that hold silently. So the
    // closure closes over the sweep's FACTS and never over `state`.
    //
    // What a failure here costs, honestly: a dormant endpoint that answered on
    // its cadence day and then lost its write is published as promoted and
    // skipped for another week, so the state disagrees with what was published,
    // failing toward probing LESS in the case the spec calls immediate. The
    // recovery is a re-run of the same `--at`, which `plan_sweep` rule 2 and
    // `update` rule B make exact rather than approximate.
    let outcomes = outcomes_from(&rows, &not_measured, &plan.probe);
    merge_state(Path::new(&args.state), |on_disk| {
        dormancy::update(on_disk, &endpoints, &outcomes, &args.at, &thresholds)
    })?;

    // Non-zero AFTER the file is written and after the state is recorded, never
    // instead of either. The run is worth keeping: every endpoint the sweep
    // failed on carries its own `prober-failed` facts, and the activity carries
    // the count, so the graph says what happened. The exit status is for the
    // scheduler, which has no other way to learn that this run was incomplete.
    if failed_endpoints > 0 {
        anyhow::bail!(
            "{failed_endpoints} of the {} endpoints this sweep probed were not probed because \
             the prober failed on them; {} was still written and records each of them as \
             prober-failed",
            plan.probe.len(),
            args.out,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        outcomes_from, thresholds_from, validate_instant, validate_min_gap, validate_thresholds,
        Args,
    };
    use clap::Parser;
    use sparqlwatch_prober::budget::Budget;
    use sparqlwatch_prober::dormancy::{Outcome, Thresholds, MIN_COST_MS};
    use sparqlwatch_prober::emit::MeasurementRow;
    use sparqlwatch_prober::metrics::Cost;
    use sparqlwatch_prober::verdict::Verdict;
    use std::time::Duration;

    #[test]
    fn a_well_formed_instant_is_accepted() {
        for ok in ["2026-08-20T12:00:00Z", "0001-01-01T00:00:00Z", "2026-12-31T23:59:59Z"] {
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

    /// The grammar narrowed to exactly one spelling per instant, and the reason
    /// is `dormancy`'s replay rule rather than anything about IRIs.
    ///
    /// A re-run of a sweep reuses its `--at`, and `dormancy::plan_sweep` and
    /// `dormancy::update` recognise that re-run by comparing `last_probed`
    /// against `--at` as STRINGS. Two spellings of one instant
    /// (`...00Z` and `...00.000Z`, or `...12:00:00Z` and `...14:00:00+02:00`)
    /// would give the re-run a second run IRI and no replay match, so it would
    /// strike an endpoint twice for one sweep and publish a run graph that
    /// disagrees with the first about what it skipped. A leap second goes with
    /// them: `23:59:60` is a valid `xsd:dateTime` and `dormancy::parse_instant`
    /// has no room for it in the day arithmetic it does with no date
    /// dependency.
    ///
    /// This is a documented flag narrowing, so `prober/README.md`'s `--at` row
    /// moves with it.
    #[test]
    fn the_utc_grammar_refuses_an_offset_a_fraction_and_a_leap_second() {
        for bad in [
            "2026-08-20T12:00:00+02:00", // an offset: two spellings of one instant
            "2026-08-20T12:00:00-05:00",
            "2026-08-20T12:00:00+00:00", // even the one that means Z
            "2026-08-20T12:00:00.123Z",  // a fraction: two spellings again
            "2026-08-20T12:00:00.0Z",
            "2026-12-31T23:59:60Z", // a leap second the day arithmetic has no room for
        ] {
            assert!(
                validate_instant(bad).is_err(),
                "{bad:?} must be refused: the replay rule compares instants as strings"
            );
        }
        assert!(
            validate_instant("2026-08-20T12:00:00Z").is_ok(),
            "the one accepted spelling has to stay accepted"
        );
    }

    /// The refusal names the one form it takes, because an operator handed a
    /// perfectly valid ISO-8601 instant back has no other way to learn which
    /// part of it was the problem.
    #[test]
    fn the_instant_refusal_names_the_one_form_it_accepts() {
        let err = validate_instant("2026-08-20T12:00:00+02:00").unwrap_err().to_string();
        assert!(err.contains("YYYY-MM-DDTHH:MM:SSZ"), "names the form: {err}");
    }

    /// `--dormant-cost-ms 60` is the mistake this floor exists for: seconds
    /// typed where the flag takes milliseconds. Every endpoint that used more
    /// than 60 ms in a sweep would be a strike, which on the 2026-08-24 data is
    /// roughly 460 of 543 endpoints relegated over two sweeps. The floor is one
    /// `Budget::request`, because below that an endpoint that merely ran out its
    /// request budget once would be a strike, and running out a request budget
    /// once is what a healthy-but-slow endpoint does.
    #[test]
    fn a_cost_threshold_below_one_request_budget_is_refused_at_startup() {
        let at = |cost_ms| Thresholds { cost_ms, ..Thresholds::default() };
        assert!(validate_thresholds(&at(MIN_COST_MS)).is_ok(), "the floor itself is allowed");
        assert!(validate_thresholds(&Thresholds::default()).is_ok(), "the shipped default is");
        assert!(validate_thresholds(&at(MIN_COST_MS - 1)).is_err(), "one under it is not");
        assert!(validate_thresholds(&at(60)).is_err(), "seconds typed where ms were meant");
        assert!(validate_thresholds(&at(0)).is_err());
        let err = validate_thresholds(&at(60)).unwrap_err().to_string();
        assert!(err.contains("--dormant-cost-ms"), "names the flag: {err}");
        assert!(err.contains("60"), "names the value it refused: {err}");
        assert!(err.contains("30000"), "names the floor to stay at or above: {err}");
    }

    /// The four dormancy defaults, pinned on the PARSED args rather than on the
    /// `DEFAULT_*` constants, for the reason the cost ceiling's test records:
    /// asserting on the constant would still pass if `default_value_t` were
    /// changed to name something else.
    ///
    /// Four and not five: there is no `--dormant-grace-days`, because a sweep
    /// never reads a grace. The equality against `Thresholds::default()` below
    /// is what still pins the grace this binary hands the policy, and
    /// `thresholds_from` says why it is a constant here.
    #[test]
    fn the_dormancy_defaults_are_the_calibrated_ones() {
        let args = Args::parse_from(["prober", "--at", "2026-01-01T00:00:00Z"]);
        assert_eq!(args.state, sparqlwatch_prober::state_file::DEFAULT_STATE);
        assert_eq!(args.dormant_cost_ms, 60_000);
        assert_eq!(args.dormant_strikes.get(), 2);
        assert_eq!(args.dormant_every_days.get(), 7);
        assert!(
            Args::try_parse_from([
                "prober", "--at", "2026-01-01T00:00:00Z", "--dormant-grace-days", "14"
            ])
            .is_err(),
            "a flag a sweep cannot read must not be offered: the grace is set by \
             `dormancy wake --grace-days`"
        );
        assert_eq!(
            thresholds_from(&args),
            Thresholds::default(),
            "a plain run must use the policy's own calibrated numbers"
        );
    }

    /// A zero in either counted flag is refused by the parser rather than
    /// repaired downstream, the same argument `--concurrency` records. Zero
    /// strikes would relegate an endpoint on its first silent sweep, and a zero
    /// cadence divides by nothing.
    #[test]
    fn a_zero_in_a_counted_dormancy_flag_is_refused_by_the_parser() {
        for (flag, value) in [
            ("--dormant-strikes", "0"),
            ("--dormant-every-days", "0"),
        ] {
            assert!(
                Args::try_parse_from(["prober", "--at", "2026-01-01T00:00:00Z", flag, value])
                    .is_err(),
                "{flag} 0 must not parse"
            );
        }
    }

    fn measured(endpoint: &str, metric_id: &str, verdict: Verdict, elapsed_ms: Option<u64>)
        -> MeasurementRow
    {
        MeasurementRow {
            endpoint: endpoint.into(),
            metric_id: metric_id.into(),
            verdict,
            level: None,
            declared_count: None,
            observed_count: None,
            elapsed_ms,
        }
    }

    /// `elapsed_ms: None` is not a zero and is not a gap in the sum: it is
    /// three different things in `lib.rs`, and only one of them spent any time.
    /// An expired ENDPOINT budget fills the metrics it never reached with
    /// `None`; a probe kind with no implemented probe reports `None` without
    /// sending anything; and an expired METRIC budget reports `None` because the
    /// observation was dropped. The one that DID spend time, a request that ran
    /// out its own 30 s budget, comes back as a failed `Observation` carrying
    /// its elapsed time, which is why the 57 endpoints behind this policy
    /// measure 210,010 ms rather than nothing. So `None` contributes zero here
    /// and the expensive case still reaches the threshold.
    #[test]
    fn a_none_elapsed_contributes_nothing_to_cost() {
        let ep = "https://a.example/sparql";
        let rows = vec![
            measured(ep, "availability", Verdict::Absent, Some(30_001)),
            measured(ep, "cors", Verdict::Indeterminate, None),
            measured(ep, "classes", Verdict::Absent, Some(30_002)),
        ];
        let outcomes = outcomes_from(&rows, &[], &[ep.to_string()]);
        assert_eq!(
            outcomes,
            vec![Outcome { url: ep.into(), cost_ms: 60_003, positive: false, liveness_failed: false }],
            "the two measured metrics sum and the unmeasured one adds nothing"
        );
    }

    /// A positive verdict is `Verified` or `UndeclaredButVerified` and nothing
    /// else. The six-verdict vocabulary is closed and dormancy is not a member
    /// of it, so this maps the two that mean "we confirmed something" and
    /// leaves the other four alone.
    #[test]
    fn one_positive_row_makes_the_whole_endpoints_outcome_positive() {
        let ep = "https://a.example/sparql";
        for positive in [Verdict::Verified, Verdict::UndeclaredButVerified] {
            let rows = vec![
                measured(ep, "availability", Verdict::Absent, Some(90_000)),
                measured(ep, "cors", positive, Some(1)),
            ];
            let outcomes = outcomes_from(&rows, &[], &[ep.to_string()]);
            assert!(outcomes[0].positive, "{positive:?} is a positive verdict");
        }
        for other in [
            Verdict::Absent,
            Verdict::DeclaredOnly,
            Verdict::Indeterminate,
            Verdict::DeclaredButWrong,
        ] {
            let rows = vec![measured(ep, "cors", other, Some(1))];
            assert!(
                !outcomes_from(&rows, &[], &[ep.to_string()])[0].positive,
                "{other:?} is not a positive verdict"
            );
        }
    }

    /// An endpoint the sweep PROBED and got nothing at all from still gets an
    /// `Outcome`, at zero cost.
    ///
    /// This is deliberate and it is the opposite of the obvious reading. With no
    /// `Outcome` the endpoint gets no `last_probed`, so a re-run of the same
    /// `--at` would not recognise it as part of that sweep and would skip it as
    /// `Automatic`: the one endpoint most worth retrying is the one that never
    /// gets retried. Zero cost lands in `dormancy::update`'s rule G, so a
    /// prober failure still earns no strike, which is the property that matters.
    #[test]
    fn an_endpoint_the_prober_failed_on_gets_a_zero_cost_outcome() {
        let failed = "https://gone.example/sparql";
        let ok = "https://a.example/sparql";
        let rows = vec![measured(ok, "availability", Verdict::Absent, Some(7))];
        let outcomes = outcomes_from(&rows, &[], &[failed.to_string(), ok.to_string()]);
        assert_eq!(
            outcomes,
            vec![
                Outcome { url: ok.into(), cost_ms: 7, positive: false, liveness_failed: false },
                Outcome { url: failed.into(), cost_ms: 0, positive: false, liveness_failed: false },
            ],
            "every probed endpoint gets exactly one outcome, in url order"
        );
    }

    /// An endpoint the sweep DECLINED to probe gets no outcome, because an
    /// outcome is a measurement and no measurement was taken. `update`'s rule D
    /// then carries its entry forward unchanged, which is what leaves a dormant
    /// endpoint's `last_probed` pointing at the sweep that last actually asked
    /// it something.
    #[test]
    fn an_endpoint_that_was_not_probed_gets_no_outcome() {
        let skipped = "https://slow.example/sparql";
        let probed = "https://a.example/sparql";
        let rows = vec![measured(probed, "availability", Verdict::Absent, Some(7))];
        let outcomes = outcomes_from(&rows, &[], &[probed.to_string()]);
        assert_eq!(outcomes.len(), 1, "one probed endpoint, one outcome");
        assert!(
            outcomes.iter().all(|o| o.url != skipped),
            "an endpoint the sweep never asked has no outcome: {outcomes:?}"
        );
    }

    /// Two outcomes for one url is a hard error inside `dormancy::update`
    /// (rule I), because one of them would decide a strike and the other would
    /// be lost. Collecting through a map here means a duplicate in the probe
    /// list cannot produce that shape at all, rather than producing it and
    /// stopping the write-back at the end of a 90 minute sweep.
    #[test]
    fn a_duplicate_in_the_probe_list_cannot_yield_two_outcomes() {
        let ep = "https://a.example/sparql";
        let rows = vec![measured(ep, "availability", Verdict::Absent, Some(5))];
        let outcomes = outcomes_from(&rows, &[], &[ep.to_string(), ep.to_string()]);
        assert_eq!(outcomes.len(), 1, "one url, one outcome: {outcomes:?}");
        assert_eq!(outcomes[0].cost_ms, 5, "and its cost is counted once");
    }

    /// A row for an endpoint the sweep was not asked to probe is dropped rather
    /// than turned into an outcome. `run_sweep` cannot produce one, and
    /// `dormancy::update` would refuse an outcome whose url is not in the
    /// endpoint list; dropping keeps a bug in the sweep from costing the whole
    /// run's state write.
    #[test]
    fn a_row_for_an_unprobed_endpoint_does_not_invent_an_outcome() {
        let rows = vec![measured("https://stray.example/sparql", "cors", Verdict::Verified, Some(9))];
        assert_eq!(outcomes_from(&rows, &[], &[]), Vec::new());
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

    /// Four hosts at once, and the reason is politeness rather than
    /// throughput: four in flight with a 2s per-host gap is roughly two
    /// requests per second in aggregate, which is a defensible load for a
    /// service that probes strangers uninvited. Asserted on the PARSED args,
    /// not on `DEFAULT_CONCURRENCY`, for the same reason as the ceiling below.
    #[test]
    fn the_default_concurrency_is_four_hosts() {
        let args = Args::parse_from(["prober", "--at", "2026-01-01T00:00:00Z"]);
        assert_eq!(args.concurrency.get(), 4, "a plain run talks to four hosts at once");
    }

    /// `Semaphore::new(0)` does not fail, it hangs: every group would wait
    /// forever for a permit that cannot exist, and a sweep that probes nothing
    /// and reports nothing is the worst failure this crate has. `NonZeroUsize`
    /// makes it unrepresentable, so the refusal happens in the parser and
    /// nothing downstream has to repair a caller's zero.
    #[test]
    fn a_concurrency_of_zero_is_refused_before_anything_is_probed() {
        assert!(
            Args::try_parse_from([
                "prober",
                "--at",
                "2026-01-01T00:00:00Z",
                "--concurrency",
                "0"
            ])
            .is_err(),
            "zero must not parse: a semaphore with no permits hangs rather than failing"
        );
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
