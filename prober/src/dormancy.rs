//! The admission policy: which endpoints a sweep probes, and which it declines.
//!
//! This module is the whole decision and nothing else. It touches no disk, no
//! clock and no network: every instant it reasons about arrives as a string
//! from the caller's `--at`, and the state it reads and writes is a value.
//! `state_file.rs` does the I/O, `main.rs` does the wiring. That split is what
//! makes the policy testable at all, because the interesting cases are a sweep
//! re-run at the same instant, a sweep a week late and an operator command
//! issued in the middle of a ninety minute sweep, none of which a test can
//! stage against a real clock.
//!
//! **Why any of this exists.** The 2026-08-24 sweep of all 543 candidates cost
//! 3.46 serial hours. 57 endpoints were 3.12 of them, 90% of the total, and
//! not one produced a positive verdict; 48 of the 57 spent between 210,010 and
//! 210,021 ms, which is what seven cheap metrics each running out a 30 s
//! request budget sums to. They accept the connection and hold it open until
//! each request is cancelled. Re-probed alone 39 hours later, 56 of 57 were
//! over 60 s again and none answered. Meanwhile 339 equally silent endpoints
//! cost 0.05 h between them.
//!
//! So the policy is **cost-weighted, not failure-weighted**: relegating on
//! failure would relegate 459 endpoints to save 0.34 h, and relegating on cost
//! saves 3.12 h by touching 57. The 339 free ones stay in every sweep, so an
//! endpoint coming back to life is noticed on the day it happens. And **both
//! conditions are required**: three endpoints that do answer cost 61 s, 67 s
//! and 89 s, so a cost-only rule would relegate working endpoints whatever the
//! threshold.
//!
//! **`dormant`, not `unresponsive`.** What is known is that seven probes, each
//! cancelled at 30 s, went unanswered. `dormant` describes where the endpoint
//! sits in our rotation, which is a fact about us; `unresponsive` would
//! overclaim in exactly the way the six-verdict vocabulary exists to prevent.
//! Dormancy is therefore **not a verdict** and never becomes one.
//!
//! **Two time granularities, deliberately, and they are not unified.**
//!
//! - **Replay detection compares exact instant strings.** A re-run reuses its
//!   `--at`, and two sweeps in one day are two sweeps: a 09:00 expensive sweep
//!   followed by a 21:00 positive one must land the promotion, which a day
//!   keyed comparison would discard as a replay.
//! - **The cadence compares whole days.** A sweep fifteen minutes early is not
//!   a day early, and an instant comparison would let the seven day cadence
//!   drift a little later every week.
//!
//! Because the first is a string comparison, the accepted instant form is
//! exactly `YYYY-MM-DDTHH:MM:SSZ` and nothing else: two spellings of one
//! instant (`...00Z` and `...00.000Z`) would give a re-run a second run IRI and
//! no replay match, which defeats the mechanism. See `parse_instant`, which
//! also owns the day arithmetic so that this crate needs no date dependency.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::num::{NonZeroU32, NonZeroU64};

/// Above this, in summed per-metric `elapsed_ms` for one endpoint in one run,
/// a silent sweep is a strike. 60 s sits clear of the three answering
/// endpoints at 61 to 89 s by construction, and keeping the 4 silent endpoints
/// in the 30 to 60 s band in every sweep costs 0.04 h.
pub const DEFAULT_COST_MS: u64 = 60_000;
/// The floor a flag may not go under: one `Budget::request`. Below it, an
/// endpoint that merely used its request budget once would be a strike.
pub const MIN_COST_MS: u64 = 30_000;
/// Two consecutive expensive silent sweeps, not one. The stability data would
/// justify one (98%), but two costs one extra probe per endpoint and it is
/// exactly the `data.datahub.kr` case: 210.0 s in one sweep, 5.7 s in the next.
pub const DEFAULT_STRIKES: u32 = 2;
pub const DEFAULT_CADENCE_DAYS: u64 = 7;
/// A hand wake grants immunity from automatic relegation for seven days.
pub const DEFAULT_GRACE_DAYS: u64 = 7;
/// The state file's shape. A file declaring anything else stops the sweep
/// rather than being guessed at, because a misread strike count silently
/// relegates or silently un-relegates an endpoint.
pub const STATE_VERSION: u32 = 1;

/// The four numbers the policy is calibrated against, all of them flags.
///
/// Declared explicitly rather than derived: the fields are `NonZero`, so the
/// `DEFAULT_*` consts need unwrapping, and every test in this module calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds {
    pub cost_ms: u64,
    pub strikes: NonZeroU32,
    pub cadence_days: NonZeroU64,
    pub grace_days: NonZeroU64,
}

impl Default for Thresholds {
    fn default() -> Thresholds {
        Thresholds {
            cost_ms: DEFAULT_COST_MS,
            // Unwrap on a literal const: unreachable unless somebody edits the
            // const to zero, which would not compile past this line.
            strikes: NonZeroU32::new(DEFAULT_STRIKES).unwrap(),
            cadence_days: NonZeroU64::new(DEFAULT_CADENCE_DAYS).unwrap(),
            grace_days: NonZeroU64::new(DEFAULT_GRACE_DAYS).unwrap(),
        }
    }
}

/// An operator's decision about one endpoint, which outranks the machine in
/// both directions.
///
/// On disk this is a `[endpoint.hold]` table with `state = "awake"` or
/// `state = "dormant"`, so the file says which kind of hold it is in a word an
/// operator reading it recognises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Hold {
    /// `until` is `None` for a pinned wake. When present it is a full
    /// `YYYY-MM-DDTHH:MM:SSZ` instant, because the day helper parses it.
    Awake { reason: String, until: Option<String> },
    /// No expiry field at all: a hold the policy ignores must not be
    /// representable, and plan rule 1 never reads one. A sleep is permanent
    /// until a human wakes it, because an operator may sleep an endpoint for
    /// reasons no verdict expresses.
    Dormant { reason: String },
}

/// What the policy knows about one endpoint.
///
/// Every `Option` field is genuinely absent for a real endpoint at some point:
/// a url in the registry that no sweep has reached yet has no `last_probed`,
/// and an endpoint nobody has ever held has no `hold`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EndpointState {
    pub url: String,
    /// Consecutive expensive silent sweeps as of `last_probed`.
    #[serde(default)]
    pub strikes: u32,
    /// Strikes as of the instant before `last_probed`, so a replay recomputes
    /// instead of incrementing and a replay with a BETTER outcome still
    /// promotes. Without it, re-running one `--at` twice would relegate an
    /// endpoint on the strength of a single sweep.
    #[serde(default)]
    pub strikes_before_last: u32,
    #[serde(default)]
    pub last_probed: Option<String>,
    #[serde(default)]
    pub last_cost_ms: Option<u64>,
    /// Set means relegated, and the instant is published in run graphs, so it
    /// is never rewritten once set.
    #[serde(default)]
    pub dormant_since: Option<String>,
    #[serde(default)]
    pub hold: Option<Hold>,
    /// The reason of an `Awake` hold whose grace has run out, kept so a page
    /// can say why this endpoint was in the sweep last week and is not now.
    /// Declared after `hold` on purpose, which is safe: the vendored `toml`
    /// serializer buffers child tables by creation position and skips `None`.
    #[serde(default)]
    pub lapsed_hold: Option<String>,
}

/// The state file, whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The newest sweep this file has seen. The slice divides by the gap
    /// between it and `now`, so a weekly sweep probes a week's worth.
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub endpoint: Vec<EndpointState>,
}

/// What `render` puts above the data. It names the binary that writes the file
/// because the first question an operator finding it has is whether editing it
/// is allowed, and the second is what the fields mean.
const HEADER: &str = "\
# sparqlwatch dormancy state. Written by every sweep and by the `dormancy`
# operator binary; it is not a configuration file. Each [[endpoint]] records
# what prober/src/dormancy.rs knows about one endpoint: how many consecutive
# expensive silent sweeps it has produced, when it was last probed, when it
# was relegated, and any hold an operator placed on it.
#
# Hand editing is supported and checked. Every instant is exactly
# YYYY-MM-DDTHH:MM:SSZ, and a file this build cannot read stops the sweep
# rather than being treated as an empty one: a state file read as empty would
# quietly re-admit every relegated endpoint and forget every operator hold.
";

impl State {
    pub fn empty() -> State {
        State { version: STATE_VERSION, updated_at: None, endpoint: Vec::new() }
    }

    /// Parse a state file, validating the version and every instant-shaped
    /// field, naming the field, the value and the endpoint it belongs to.
    ///
    /// **Fails closed**, like `registry::parse_exclusions` and for a related
    /// reason: a state file that cannot be read is not an empty one, and
    /// treating it as empty would re-probe all 57 relegated endpoints and
    /// discard every hold somebody placed by hand.
    ///
    /// One combination is refused: `dormant_since` set with `last_probed`
    /// absent, **unless the entry carries a `Dormant` hold**. That exemption is
    /// not a nicety. `sleep` on a never probed endpoint produces exactly that
    /// combination, and the state read happens before any probing, so refusing
    /// it unconditionally would mean one `dormancy sleep` on a new url bricks
    /// every subsequent sweep and every subsequent `dormancy` command until a
    /// human hand-edits the file. It is safe because plan rule 1 fires first
    /// for a held entry, and rule 6, the only reader of `last_probed`, is never
    /// reached.
    pub fn parse(toml_text: &str) -> anyhow::Result<State> {
        let mut state: State = toml::from_str(toml_text).map_err(|error| {
            anyhow::anyhow!(
                "the dormancy state does not parse, so nothing here knows which endpoints \
                 were relegated or which an operator held. Every [[endpoint]] needs a url, \
                 and a [endpoint.hold] needs state = \"awake\" or state = \"dormant\" with \
                 a reason: {error}"
            )
        })?;
        if state.version != STATE_VERSION {
            anyhow::bail!(
                "the dormancy state declares version {}, and this build reads and writes \
                 version {STATE_VERSION} only. Guessing at an unknown layout would risk \
                 misreading a strike count, which silently relegates a working endpoint or \
                 silently re-admits 57 expensive ones: upgrade the binary, or move the file \
                 aside and let the next sweep write a fresh one",
                state.version
            );
        }
        if let Some(updated_at) = &state.updated_at {
            day_of(updated_at, "updated_at")?;
        }
        let mut seen: HashSet<String> = HashSet::with_capacity(state.endpoint.len());
        for entry in &state.endpoint {
            if entry.url.trim().is_empty() {
                anyhow::bail!(
                    "an [[endpoint]] in the dormancy state has an empty url, so nothing can \
                     say which endpoint it is about: give it the url as the sweep spells it, \
                     or delete the entry"
                );
            }
            if !seen.insert(entry.url.clone()) {
                anyhow::bail!(
                    "the dormancy state holds two [[endpoint]] entries for {}, so its strike \
                     count and its hold are ambiguous and a sweep would apply an outcome to \
                     one of them and leave the other stale: keep the entry that is right and \
                     delete the other",
                    entry.url
                );
            }
            if let Some(last_probed) = &entry.last_probed {
                day_of(last_probed, &format!("last_probed of {}", entry.url))?;
            }
            if let Some(dormant_since) = &entry.dormant_since {
                day_of(dormant_since, &format!("dormant_since of {}", entry.url))?;
            }
            if let Some(Hold::Awake { until: Some(until), .. }) = &entry.hold {
                day_of(until, &format!("hold.until of {}", entry.url))?;
            }
            if entry.dormant_since.is_some()
                && entry.last_probed.is_none()
                && !matches!(entry.hold, Some(Hold::Dormant { .. }))
            {
                anyhow::bail!(
                    "{} is marked dormant_since with no last_probed and no operator hold, \
                     and the cadence cannot say when to re-probe an endpoint it has no \
                     record of probing. Either give it the last_probed instant it was \
                     relegated on, or, if a person relegated it, give it a [endpoint.hold] \
                     with state = \"dormant\" and a reason",
                    entry.url
                );
            }
        }
        state.endpoint.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(state)
    }

    /// The file, sorted by url so two runs that know the same facts write the
    /// same bytes and a diff shows only what changed.
    pub fn render(&self) -> String {
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            #[serde(skip_serializing_if = "Option::is_none")]
            updated_at: Option<&'a String>,
            endpoint: Vec<&'a EndpointState>,
        }
        let mut endpoint: Vec<&EndpointState> = self.endpoint.iter().collect();
        endpoint.sort_by(|a, b| a.url.cmp(&b.url));
        let document =
            Document { version: self.version, updated_at: self.updated_at.as_ref(), endpoint };
        // Infallible for this shape: every value is a string, an integer or a
        // table, and no map has a non-string key. An error here would mean the
        // types above changed, which is a bug and not a runtime condition.
        let body = toml::to_string(&document)
            .expect("the dormancy state is strings, integers and tables, which toml can write");
        format!("{HEADER}{body}")
    }

    pub fn get(&self, url: &str) -> Option<&EndpointState> {
        self.endpoint.iter().find(|entry| entry.url == url)
    }
}

/// Why one endpoint was not probed. Two reasons, closed, because a page has to
/// say which and the machine's reason and a person's reason read differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Automatic,
    OperatorHold,
}

impl SkipReason {
    /// The wire slugs, exactly these two strings: run graphs carry them, the
    /// loader parses them and the pages render them. Precedent:
    /// `Verdict::slug` in `verdict.rs`.
    pub fn slug(&self) -> &'static str {
        match self {
            SkipReason::Automatic => "automatic",
            SkipReason::OperatorHold => "operator-hold",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub url: String,
    /// Carried as it stands in the state, so the run graph publishes the
    /// instant the endpoint was actually relegated rather than this run's.
    pub dormant_since: Option<String>,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub probe: Vec<String>,
    pub skipped: Vec<Skipped>,
}

/// What one endpoint cost this sweep and whether anything answered.
///
/// `positive` is `Verdict::Verified` or `Verdict::UndeclaredButVerified` and
/// nothing else; `cost_ms` is the sum of per-metric `elapsed_ms`, the quantity
/// the 60 s threshold was calibrated against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub url: String,
    pub cost_ms: u64,
    pub positive: bool,
}

/// What an entry's hold means for the sweep happening now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldEffect {
    /// No hold at all.
    Unheld,
    /// A person relegated this endpoint. Permanent until a person wakes it.
    Dormant,
    /// A person woke it, and the grace has not run out.
    LiveAwake,
    /// A person woke it and the grace has run out. The machine is back in
    /// charge; `update` rule A is what clears the record of it.
    LapsedAwake,
}

fn hold_effect(hold: Option<&Hold>, today: i64, url: &str) -> anyhow::Result<HoldEffect> {
    Ok(match hold {
        None => HoldEffect::Unheld,
        Some(Hold::Dormant { .. }) => HoldEffect::Dormant,
        Some(Hold::Awake { until: None, .. }) => HoldEffect::LiveAwake,
        Some(Hold::Awake { until: Some(until), .. }) => {
            // Whole days: a grace that expired at 09:00 has not expired for a
            // sweep at 21:00 the same day.
            if day_of(until, &format!("hold.until of {url}"))? >= today {
                HoldEffect::LiveAwake
            } else {
                HoldEffect::LapsedAwake
            }
        }
    })
}

/// Decide what this sweep probes and what it declines, and why.
///
/// The rules run in this order per endpoint, and the order is load bearing:
///
/// 1. A `Dormant` hold: SKIP, `OperatorHold`. **This precedes replay
///    detection**, because a re-run of an `--at` from before an operator's
///    `sleep` would otherwise probe an endpoint a person forbade, and a hold
///    overrides the machine in both directions.
/// 2. This `--at` has already run and this endpoint's `last_probed` is
///    `now`: PROBE.
/// 3. This `--at` has already run and it is not: SKIP, `Automatic`.
/// 4. A live `Awake` hold: PROBE.
/// 5. Not relegated: PROBE. This is the 339 cheap silent ones and every url
///    the state has never seen.
/// 6. Relegated: PROBE if the cadence is due and this endpoint is in the
///    oldest `slice` of the due set, else SKIP, `Automatic`.
///
/// **Rules 2 and 3 are one decision, and it is computed once for the whole
/// sweep**, not per endpoint: `this_at_has_already_run` is true when ANY
/// endpoint in `endpoints` was last probed at `now`. Per endpoint it
/// reproduces only half a replay. With 57 dormant, cadence 7 and a slice of 9,
/// the first run probes the 9 oldest and moves their `last_probed`; a re-run
/// would then probe those 9 by rule 2 AND admit the next 9 through rule 5, so
/// it probes 18 rather than 9, spends roughly 31 extra minutes, and writes a
/// run graph that disagrees with the first about what it skipped. Globally: if
/// this `--at` has already run, probe exactly what it probed and skip the rest.
/// That is sound because state is written only after the footer, so a crashed
/// run leaves no replay marker and its re-run is correctly a fresh sweep.
pub fn plan_sweep(
    state: &State,
    endpoints: &[String],
    now: &str,
    thresholds: &Thresholds,
) -> anyhow::Result<Plan> {
    let today = day_of(now, "--at")?;
    let this_at_has_already_run = endpoints
        .iter()
        .any(|url| state.get(url).and_then(|e| e.last_probed.as_deref()) == Some(now));

    // Rule 6 cannot decide any one endpoint without knowing the whole due set,
    // so the set is built first. `governed` counts only what rule 6 reaches:
    // listed, relegated, and under no effective hold. Counting held endpoints
    // would divide the slice among endpoints that are probed anyway, and 20
    // pinned endpoints beside 6 dormant ones would give the 6 a slice of 4.
    let mut governed: i64 = 0;
    let mut due: Vec<(&Option<String>, &str)> = Vec::new();
    if !this_at_has_already_run {
        for url in endpoints {
            let Some(entry) = state.get(url) else { continue };
            if entry.dormant_since.is_none() {
                continue;
            }
            match hold_effect(entry.hold.as_ref(), today, &entry.url)? {
                HoldEffect::Dormant | HoldEffect::LiveAwake => continue,
                HoldEffect::Unheld | HoldEffect::LapsedAwake => {}
            }
            governed += 1;
            if is_due(entry, today, thresholds)? {
                due.push((&entry.last_probed, &entry.url));
            }
        }
        // Oldest first, then url for a stable order. `None` sorts before every
        // `Some`, which is right: an endpoint the state has no probe record for
        // is the most overdue thing in the set. A due-but-unsliced endpoint
        // keeps its old `last_probed`, so it sorts ahead of everything probed
        // since and climbs into the next slice. It cannot starve.
        due.sort();
    }
    let slice = slice_size(state, today, governed, thresholds)?;
    let admitted: HashSet<&str> = due.iter().take(slice).map(|(_, url)| *url).collect();

    let mut probe = Vec::new();
    let mut skipped = Vec::new();
    for url in endpoints {
        let entry = state.get(url);
        let effect = match entry {
            Some(entry) => hold_effect(entry.hold.as_ref(), today, &entry.url)?,
            None => HoldEffect::Unheld,
        };
        let dormant_since = entry.and_then(|entry| entry.dormant_since.clone());
        let mut skip = |reason: SkipReason| {
            skipped.push(Skipped { url: url.clone(), dormant_since: dormant_since.clone(), reason });
        };
        // 1
        if effect == HoldEffect::Dormant {
            skip(SkipReason::OperatorHold);
            continue;
        }
        // 2 and 3
        if this_at_has_already_run {
            if entry.and_then(|entry| entry.last_probed.as_deref()) == Some(now) {
                probe.push(url.clone());
            } else {
                skip(SkipReason::Automatic);
            }
            continue;
        }
        // 4
        if effect == HoldEffect::LiveAwake {
            probe.push(url.clone());
            continue;
        }
        // 5
        let Some(entry) = entry else {
            probe.push(url.clone());
            continue;
        };
        if entry.dormant_since.is_none() {
            probe.push(url.clone());
            continue;
        }
        // 6
        if admitted.contains(entry.url.as_str()) {
            probe.push(url.clone());
        } else {
            skip(SkipReason::Automatic);
        }
    }
    Ok(Plan { probe, skipped })
}

/// Whether the cadence has come round for one relegated endpoint.
///
/// Whole days, so a sweep that starts fifteen minutes earlier than last
/// week's is not a day early: `last_probed` 2026-09-02T19:45:03Z is due at
/// 2026-09-09T19:30:00Z. Compared as instants the cadence would drift later
/// every week for as long as sweeps keep starting a few minutes early.
///
/// No `last_probed` at all counts as due. It is not reachable through `sleep`
/// (rule 1 fires first) but a hand-edited file can hold it, and the honest
/// reading of "relegated, never probed" is that a probe is overdue.
fn is_due(entry: &EndpointState, today: i64, thresholds: &Thresholds) -> anyhow::Result<bool> {
    let Some(last_probed) = &entry.last_probed else { return Ok(true) };
    let probed = day_of(last_probed, &format!("last_probed of {}", entry.url))?;
    Ok(today - probed >= thresholds.cadence_days.get() as i64)
}

/// How many of the due dormant endpoints this sweep takes.
///
/// ```text
/// gap   = max(1, day(now) - day(state.updated_at))   // 1 when updated_at is absent
/// slice = clamp(ceil(governed * gap / cadence_days), 1, governed)
/// ```
///
/// **The divisor comes from the observed gap, and that is the point.** A
/// constant `ceil(governed / cadence_days)` silently assumes one sweep per day.
/// Nothing here is on a schedule, and `/about` says so; a 1h26m sweep run
/// weekly would probe 9 of 57 per sweep, giving each dormant endpoint a probe
/// every 6.3 weeks against a seven day cadence. Dividing by the gap makes
/// daily sweeps take `ceil(57 * 1 / 7) = 9` and weekly sweeps take all 57, so a
/// week's worth is probed in the sweep that a week has passed before.
fn slice_size(
    state: &State,
    today: i64,
    governed: i64,
    thresholds: &Thresholds,
) -> anyhow::Result<usize> {
    if governed <= 0 {
        return Ok(0);
    }
    let gap = match &state.updated_at {
        Some(updated_at) => (today - day_of(updated_at, "updated_at")?).max(1),
        // No previous sweep to measure a gap against, so assume the smallest
        // one. Assuming a large gap here would put the whole dormant set into
        // the first sweep after the file is created.
        None => 1,
    };
    let cadence = thresholds.cadence_days.get() as i64;
    let ceiling = (governed.saturating_mul(gap).saturating_add(cadence - 1)) / cadence;
    Ok(ceiling.clamp(1, governed) as usize)
}

/// Apply one sweep's outcomes to the state and return the state to write.
///
/// Rule A runs for **every entry in the state**, including entries whose url is
/// not in `endpoints`, because a hold lapses on a date and not on being swept.
///
/// - **A. Clear a lapsed `Awake` hold.** `hold = None`, `lapsed_hold =
///   Some(reason)`, **and both strike counters go to zero**. Clearing only
///   `strikes` would leave a replay of the last grace-period sweep recomputing
///   from `strikes_before_last = 1` and re-relegating the endpoint at once.
///
/// Then, per endpoint in `endpoints`:
///
/// - **B. Replay.** `last_probed == now` exactly: recompute from
///   `strikes_before_last`, leaving `strikes_before_last` unchanged.
/// - **C. Out of order.** `last_probed` present and `now < last_probed`: carry
///   forward unchanged, and log. A sweep replaying an older `--at` must not
///   move state backwards.
/// - **D. Not in `outcomes`:** carry forward unchanged.
/// - **E. Positive:** promote immediately, on the sweep that produced it.
/// - **F. Not positive, cost above the threshold:** a strike, and relegate at
///   the threshold.
/// - **G. Not positive, cost at or under:** the 339. Strikes reset,
///   `dormant_since` untouched.
/// - **H.** A `Dormant` hold means no outcome should exist, so E to G do not
///   fire for one. If one arrives anyway an operator slept the endpoint during
///   the sweep, and a sleep is permanent until a human wakes it: applying a
///   positive outcome would promote an endpoint a person just relegated.
/// - **I.** Duplicate urls in `outcomes` are an error, not two strikes.
/// - **NO PRUNING.** The spec documents narrowed sweeps, which is how the 57
///   were re-probed, so `--endpoints just-the-57.toml` must not destroy the
///   other 486 entries. Pruning is the explicit `dormancy prune`.
///
/// **Why rule F asks whether the entry carries a live `Awake` hold.** The spec
/// says a wake "makes it immune from automatic relegation for seven days".
/// Letting F relegate during the grace and having A un-relegate afterwards
/// would mark a woken endpoint dormant on day two of its own immunity. Two
/// fixes were available and they differ: clearing `dormant_since` at the lapse
/// relegates and then un-relegates, while suppressing F keeps the endpoint out
/// of the dormant set throughout. **Suppressing is chosen**, because a dormant
/// endpoint counts toward the slice and toward the published dormant count, and
/// an immune endpoint should appear in neither. Strikes still accumulate
/// underneath, so at the lapse A resets them and relegation costs two fresh
/// post-grace sweeps.
pub fn update(
    state: &State,
    endpoints: &[String],
    outcomes: &[Outcome],
    now: &str,
    thresholds: &Thresholds,
) -> anyhow::Result<State> {
    let today = day_of(now, "--at")?;

    // I
    let mut by_url: HashMap<&str, &Outcome> = HashMap::with_capacity(outcomes.len());
    for outcome in outcomes {
        if by_url.insert(&outcome.url, outcome).is_some() {
            anyhow::bail!(
                "the sweep reported two outcomes for {} in one run, and this policy cannot \
                 say which one that run measured: one of them would decide a strike and the \
                 other would be lost. The endpoint list holds each url once, so two \
                 outcomes for one url is a bug in the sweep and not something to average",
                outcome.url
            );
        }
    }
    let listed: HashSet<&str> = endpoints.iter().map(|url| url.as_str()).collect();
    for outcome in outcomes {
        if !listed.contains(outcome.url.as_str()) {
            anyhow::bail!(
                "the sweep reported an outcome for {} but that url is not in the endpoint \
                 list handed to this write, so the outcome would be silently dropped and a \
                 strike lost. Hand `update` the same endpoint list the sweep planned against",
                outcome.url
            );
        }
    }

    let mut next = state.clone();

    // A, over every entry and not only the swept ones.
    for entry in &mut next.endpoint {
        let lapsed = match &entry.hold {
            Some(Hold::Awake { reason, until: Some(until) }) => {
                if day_of(until, &format!("hold.until of {}", entry.url))? < today {
                    Some(reason.clone())
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(reason) = lapsed {
            entry.hold = None;
            entry.lapsed_hold = Some(reason);
            entry.strikes = 0;
            entry.strikes_before_last = 0;
        }
    }

    for url in endpoints {
        let outcome = by_url.get(url.as_str()).copied();
        let index = match next.endpoint.iter().position(|entry| &entry.url == url) {
            Some(index) => index,
            None => {
                // D, for a url the state has never seen: there is nothing to
                // carry forward, and writing an all-zero entry for every
                // endpoint a sweep declined would put 543 rows in the file to
                // say nothing.
                if outcome.is_none() {
                    continue;
                }
                next.endpoint.push(EndpointState { url: url.clone(), ..Default::default() });
                next.endpoint.len() - 1
            }
        };
        let entry = &mut next.endpoint[index];
        let effect = hold_effect(entry.hold.as_ref(), today, &entry.url)?;

        // H
        if effect == HoldEffect::Dormant {
            if outcome.is_some() {
                tracing::warn!(
                    endpoint = %entry.url,
                    "an outcome arrived for an endpoint an operator has slept, which means \
                     the sleep was issued during this sweep. The hold stands and the outcome \
                     is not applied to the dormancy state"
                );
            }
            continue;
        }
        // D
        let Some(outcome) = outcome else { continue };
        let replay = entry.last_probed.as_deref() == Some(now);
        // C. Instant strings of this one fixed form compare lexicographically
        // in chronological order, which is the same granularity replay uses.
        if let Some(last_probed) = &entry.last_probed {
            if !replay && now < last_probed.as_str() {
                tracing::warn!(
                    endpoint = %entry.url,
                    last_probed = %last_probed,
                    at = %now,
                    "this sweep is older than the newest one this endpoint's state records, \
                     so its outcome is not applied: a late arrival must not move state \
                     backwards"
                );
                continue;
            }
        }
        // B. A replay recomputes from the strikes the replayed sweep inherited,
        // so running one --at twice cannot strike twice, and a replay carrying
        // a better outcome still promotes.
        let base = if replay {
            entry.strikes_before_last
        } else {
            entry.strikes_before_last = entry.strikes;
            entry.strikes
        };
        entry.last_probed = Some(now.to_string());
        entry.last_cost_ms = Some(outcome.cost_ms);
        if outcome.positive {
            // E
            entry.strikes = 0;
            entry.dormant_since = None;
        } else if outcome.cost_ms > thresholds.cost_ms {
            // F
            entry.strikes = base.saturating_add(1);
            if entry.strikes >= thresholds.strikes.get()
                && entry.dormant_since.is_none()
                && effect != HoldEffect::LiveAwake
            {
                entry.dormant_since = Some(now.to_string());
            }
        } else {
            // G
            entry.strikes = 0;
        }
    }

    // The newest sweep the file has seen, never an older one: the slice divides
    // by the gap to this instant, and letting a replayed older --at move it
    // backwards would make the next sweep's gap, and so its slice, wrong.
    if next.updated_at.as_deref().is_none_or(|updated_at| updated_at < now) {
        next.updated_at = Some(now.to_string());
    }
    next.endpoint.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(next)
}

/// Put an endpoint back in every sweep by hand, with a grace period during
/// which the machine may not relegate it again.
///
/// An unknown url gains an entry, because an operator may know something before
/// the machine does: an endpoint's admin can say it is fixed before any sweep
/// has seen it answer.
///
/// **The strikes and `dormant_since` are cleared here, not merely ignored.**
/// The spec grants immunity from relegation, and an endpoint that stays
/// relegated is not immune: it would keep counting toward the slice and toward
/// the published dormant count. `last_probed` and `last_cost_ms` are left
/// alone, because they are measurements and this is not a measurement.
pub fn wake(
    state: &State,
    url: &str,
    reason: &str,
    now: &str,
    permanent: bool,
    thresholds: &Thresholds,
) -> anyhow::Result<State> {
    day_of(now, "--at")?;
    let url = checked_url(url)?;
    let reason = checked_reason(reason, "wake")?;
    // `None` is a pin, and a pin never lapses.
    let until = match permanent {
        true => None,
        false => Some(instant_plus_days(now, "--at", thresholds.grace_days.get())?),
    };
    let mut next = state.clone();
    let entry = upsert(&mut next, &url);
    entry.hold = Some(Hold::Awake { reason, until });
    entry.strikes = 0;
    entry.strikes_before_last = 0;
    entry.dormant_since = None;
    entry.lapsed_hold = None;
    next.updated_at = Some(now.to_string());
    next.endpoint.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(next)
}

/// Take an endpoint out of every sweep by hand, permanently until a human wakes
/// it, because an operator may sleep an endpoint for reasons no verdict
/// expresses: a request from its admin, a cost nobody wants to pay, a machine
/// that is being migrated.
///
/// `dormant_since` is set only if it is not already set, preserving the
/// original relegation instant that a run graph has already published. The
/// strikes are left alone: a human's decision is not evidence about the
/// endpoint's behaviour, and if the hold is later lifted the machine should
/// resume from what it actually measured.
pub fn sleep(state: &State, url: &str, reason: &str, now: &str) -> anyhow::Result<State> {
    day_of(now, "--at")?;
    let url = checked_url(url)?;
    let reason = checked_reason(reason, "sleep")?;
    let mut next = state.clone();
    let entry = upsert(&mut next, &url);
    entry.hold = Some(Hold::Dormant { reason });
    if entry.dormant_since.is_none() {
        entry.dormant_since = Some(now.to_string());
    }
    next.updated_at = Some(now.to_string());
    next.endpoint.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(next)
}

/// The entries the registry no longer lists and nobody is holding, and the
/// state without them.
///
/// A held entry is kept whatever the registry says, in both directions: an
/// operator who slept an endpoint and then dropped it from the list has
/// recorded a decision, and losing it means the endpoint silently returns to
/// full sweeps if the url comes back. Nothing here writes anything; the caller
/// decides whether to.
pub fn prunable(state: &State, endpoints: &[String]) -> (Vec<String>, State) {
    let listed: HashSet<&str> = endpoints.iter().map(|url| url.as_str()).collect();
    let mut pruned = Vec::new();
    let mut kept =
        State { version: state.version, updated_at: state.updated_at.clone(), endpoint: Vec::new() };
    for entry in &state.endpoint {
        if entry.hold.is_none() && !listed.contains(entry.url.as_str()) {
            pruned.push(entry.url.clone());
        } else {
            kept.endpoint.push(entry.clone());
        }
    }
    pruned.sort();
    kept.endpoint.sort_by(|a, b| a.url.cmp(&b.url));
    (pruned, kept)
}

/// The entry for `url`, created if the state has none.
fn upsert<'a>(state: &'a mut State, url: &str) -> &'a mut EndpointState {
    match state.endpoint.iter().position(|entry| entry.url == url) {
        Some(index) => &mut state.endpoint[index],
        None => {
            state.endpoint.push(EndpointState { url: url.to_string(), ..Default::default() });
            state.endpoint.last_mut().expect("just pushed")
        }
    }
}

/// A url an entry can be written for, or an error saying what to pass.
///
/// Refused rather than accepted as an entry that matches no endpoint. That is
/// the failure this exists for: a hold written against a url with a stray
/// space never compares equal to anything the sweep probes, and a file on disk
/// then says an endpoint is held while every sweep probes it.
fn checked_url(url: &str) -> anyhow::Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "a dormancy hold needs the endpoint url it applies to, and an empty one applies \
             to nothing: pass the url exactly as endpoints.toml spells it"
        );
    }
    Ok(trimmed.to_string())
}

/// A hold's reason, which is not optional.
///
/// The reason is the whole value of a hand hold to the next person: a state
/// file saying an endpoint is asleep without saying why leaves them unable to
/// decide whether waking it is safe.
fn checked_reason(reason: &str, command: &str) -> anyhow::Result<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "a dormancy {command} needs a reason, and it is not decoration: it is what tells \
             the next operator, or you in six months, whether undoing this is safe. Say who \
             asked or what you saw, in a few words"
        );
    }
    Ok(trimmed.to_string())
}

/// A `YYYY-MM-DDTHH:MM:SSZ` instant, taken apart.
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Parse an instant of exactly `YYYY-MM-DDTHH:MM:SSZ`, and nothing else.
///
/// Deliberately narrower than `main.rs`'s `validate_instant`, which accepts
/// offsets, fractional seconds and the leap second `23:59:60`:
///
/// - **No offset.** `2026-08-24T12:00:00+02:00` and `2026-08-24T10:00:00Z` are
///   one instant with two spellings, and replay detection is a string
///   comparison, so accepting both would give a re-run a second run IRI and no
///   replay match.
/// - **No fractional second**, for the same reason.
/// - **No leap second.** A day-number helper cannot place second 60, and
///   pretending it is second 59 would make two instants compare unequal that
///   this module would then treat as the same day.
///
/// The whole point of doing it by hand is that this crate gains no date
/// dependency for arithmetic that is a dozen lines and fully testable.
fn parse_instant(instant: &str, field: &str) -> anyhow::Result<Civil> {
    let bad = || {
        anyhow::anyhow!(
            "{field} must be an instant of exactly the form YYYY-MM-DDTHH:MM:SSZ, in UTC, \
             with no offset, no fractional second and no leap second, and it must be a date \
             that exists. Got {instant:?}. This form is narrower than ISO-8601 on purpose: \
             a re-run of one instant is detected by comparing these strings, so one instant \
             may have only one spelling"
        )
    };
    let bytes = instant.as_bytes();
    if bytes.len() != 20 {
        return Err(bad());
    }
    for (index, expected) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'Z')] {
        if bytes[index] != expected {
            return Err(bad());
        }
    }
    // Every remaining position is a digit, checked before any is read, so no
    // byte here is part of a multi-byte character.
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !bytes[index].is_ascii_digit() {
            return Err(bad());
        }
    }
    let digit = |index: usize| (bytes[index] - b'0') as u32;
    let two = |index: usize| digit(index) * 10 + digit(index + 1);
    let year =
        (digit(0) * 1000 + digit(1) * 100 + digit(2) * 10 + digit(3)) as i64;
    let civil = Civil {
        year,
        month: two(5),
        day: two(8),
        hour: two(11),
        minute: two(14),
        second: two(17),
    };
    if !(1..=12).contains(&civil.month)
        || civil.hour > 23
        || civil.minute > 59
        || civil.second > 59
        || civil.day < 1
        || civil.day > days_in_month(civil.year, civil.month)
    {
        return Err(bad());
    }
    Ok(civil)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// The proleptic Gregorian day number of a civil date, days from 1970-01-01.
///
/// Hinnant's `days_from_civil`: shift the year to start in March so the leap
/// day lands at the end of it, then count 400 year eras, which is the cycle
/// the Gregorian leap rules repeat on. Only the difference of two of these is
/// ever used, so the epoch is arbitrary, but 1970 keeps the numbers small and
/// recognisable in a failing test.
fn days_from_civil(civil: &Civil) -> i64 {
    let year = if civil.month <= 2 { civil.year - 1 } else { civil.year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month =
        if civil.month > 2 { civil.month - 3 } else { civil.month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + civil.day as i64 - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date of a day number. The inverse of `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The day number of an instant, for the cadence and for nothing else. Replay
/// detection must never come through here: it compares strings.
fn day_of(instant: &str, field: &str) -> anyhow::Result<i64> {
    Ok(days_from_civil(&parse_instant(instant, field)?))
}

/// An instant `days` after `instant`, with its time of day preserved and in the
/// same one accepted spelling.
///
/// Preserving the time of day is what makes a grace period readable: an
/// operator who wakes an endpoint at 13:45 sees the hold expire at 13:45, and
/// the day comparison in `hold_effect` means the exact time of day never
/// decides anything anyway.
fn instant_plus_days(instant: &str, field: &str, days: u64) -> anyhow::Result<String> {
    let civil = parse_instant(instant, field)?;
    let (year, month, day) = civil_from_days(days_from_civil(&civil) + days as i64);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        civil.hour, civil.minute, civil.second
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn ep(u: &str) -> String {
        u.to_string()
    }

    /// The calibration's own number: 48 of the 57 spent 210,010 to 210,021 ms.
    fn expensive(u: &str) -> Outcome {
        Outcome { url: ep(u), cost_ms: 210_000, positive: false }
    }

    /// One of the 339 silent-and-free endpoints the policy keeps in every sweep.
    fn cheap(u: &str) -> Outcome {
        Outcome { url: ep(u), cost_ms: 4_000, positive: false }
    }

    fn answered(u: &str, cost_ms: u64) -> Outcome {
        Outcome { url: ep(u), cost_ms, positive: true }
    }

    fn entry(url: &str) -> EndpointState {
        EndpointState { url: ep(url), ..Default::default() }
    }

    /// `n` endpoints relegated together, every one last probed at `last_probed`.
    fn dormant_fleet(n: usize, last_probed: &str, updated_at: &str) -> (State, Vec<String>) {
        let mut state = State::empty();
        state.updated_at = Some(ep(updated_at));
        let mut endpoints = Vec::new();
        for i in 0..n {
            let url = format!("http://e{i:03}.example/sparql");
            state.endpoint.push(EndpointState {
                url: url.clone(),
                strikes: 2,
                strikes_before_last: 1,
                last_probed: Some(ep(last_probed)),
                last_cost_ms: Some(210_000),
                dormant_since: Some(ep(last_probed)),
                hold: None,
                lapsed_hold: None,
            });
            endpoints.push(url);
        }
        (state, endpoints)
    }

    // --- the policy's four load-bearing claims, from the spec's evidence ---

    #[test]
    fn an_unknown_endpoint_is_probed() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://new.example/sparql")];
        let plan = plan_sweep(&State::empty(), &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(plan.probe, endpoints);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn one_expensive_silent_sweep_is_not_enough_to_relegate() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://a.example/s")];
        let state = update(
            &State::empty(),
            &endpoints,
            &[expensive("http://a.example/s")],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        let e = state.get("http://a.example/s").unwrap();
        assert_eq!(e.strikes, 1);
        assert!(e.dormant_since.is_none(), "one strike is not two");
        assert_eq!(
            plan_sweep(&state, &endpoints, "2026-08-25T00:00:00Z", &t).unwrap().probe,
            endpoints
        );
    }

    #[test]
    fn two_expensive_silent_sweeps_relegate() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://a.example/s")];
        let mut state = State::empty();
        for day in ["2026-08-24T00:00:00Z", "2026-08-25T00:00:00Z"] {
            state =
                update(&state, &endpoints, &[expensive("http://a.example/s")], day, &t).unwrap();
        }
        let e = state.get("http://a.example/s").unwrap();
        assert_eq!(e.strikes, 2);
        assert_eq!(e.dormant_since.as_deref(), Some("2026-08-25T00:00:00Z"));
        let plan = plan_sweep(&state, &endpoints, "2026-08-26T00:00:00Z", &t).unwrap();
        assert!(plan.probe.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::Automatic);
        assert_eq!(plan.skipped[0].dormant_since.as_deref(), Some("2026-08-25T00:00:00Z"));
    }

    #[test]
    fn a_cheap_silent_endpoint_is_never_relegated() {
        // The 339. Watching them costs nothing worth naming, so they stay in
        // every sweep and a recovery is noticed the day it happens.
        let t = Thresholds::default();
        let endpoints = vec![ep("http://silent.example/s")];
        let mut state = State::empty();
        for day in 1..=10 {
            let now = format!("2026-08-{day:02}T00:00:00Z");
            state = update(&state, &endpoints, &[cheap("http://silent.example/s")], &now, &t)
                .unwrap();
        }
        // And the threshold itself is a strict "above", so a sweep landing
        // exactly on 60 s is still cheap.
        state = update(
            &state,
            &endpoints,
            &[Outcome { url: ep("http://silent.example/s"), cost_ms: 60_000, positive: false }],
            "2026-08-11T00:00:00Z",
            &t,
        )
        .unwrap();
        let e = state.get("http://silent.example/s").unwrap();
        assert_eq!(e.strikes, 0);
        assert!(e.dormant_since.is_none());
    }

    #[test]
    fn an_expensive_endpoint_that_answers_is_never_relegated() {
        // The three that cost 61 s, 67 s and 89 s and do answer. Both
        // conditions are required, so cost alone never relegates.
        let t = Thresholds::default();
        let endpoints = vec![ep("http://slow-but-alive.example/s")];
        let mut state = State::empty();
        for (day, cost) in [(1, 61_000), (2, 67_000), (3, 89_000)] {
            let now = format!("2026-08-{day:02}T00:00:00Z");
            state = update(
                &state,
                &endpoints,
                &[answered("http://slow-but-alive.example/s", cost)],
                &now,
                &t,
            )
            .unwrap();
        }
        let e = state.get("http://slow-but-alive.example/s").unwrap();
        assert_eq!(e.strikes, 0);
        assert!(e.dormant_since.is_none());
        assert_eq!(e.last_cost_ms, Some(89_000));
    }

    #[test]
    fn a_positive_verdict_promotes_a_dormant_endpoint_immediately() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://recovers.example/s")];
        let mut state = State::empty();
        for day in ["2026-08-24T00:00:00Z", "2026-08-25T00:00:00Z"] {
            state =
                update(&state, &endpoints, &[expensive("http://recovers.example/s")], day, &t)
                    .unwrap();
        }
        assert!(state.get("http://recovers.example/s").unwrap().dormant_since.is_some());
        let state = update(
            &state,
            &endpoints,
            &[answered("http://recovers.example/s", 5_700)],
            "2026-09-01T00:00:00Z",
            &t,
        )
        .unwrap();
        let e = state.get("http://recovers.example/s").unwrap();
        assert!(e.dormant_since.is_none(), "the promotion lands on the sweep that produced it");
        assert_eq!(e.strikes, 0);
        assert_eq!(e.strikes_before_last, 2);
    }

    #[test]
    fn an_endpoint_the_sweep_did_not_probe_keeps_its_state_exactly() {
        let t = Thresholds::default();
        let mut state = State::empty();
        state.updated_at = Some(ep("2026-08-20T00:00:00Z"));
        state.endpoint.push(EndpointState {
            url: ep("http://a.example/s"),
            strikes: 1,
            strikes_before_last: 0,
            last_probed: Some(ep("2026-08-20T00:00:00Z")),
            last_cost_ms: Some(210_000),
            dormant_since: None,
            hold: None,
            lapsed_hold: None,
        });
        let before = state.get("http://a.example/s").unwrap().clone();
        let after = update(
            &state,
            &[ep("http://a.example/s")],
            &[],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        assert_eq!(after.get("http://a.example/s").unwrap(), &before);
    }

    // --- the cadence and the slice ---

    #[test]
    fn a_dormant_endpoint_is_reprobed_after_the_cadence() {
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(1, "2026-08-24T00:00:00Z", "2026-08-24T00:00:00Z");
        for early in ["2026-08-25T00:00:00Z", "2026-08-30T00:00:00Z"] {
            let plan = plan_sweep(&state, &endpoints, early, &t).unwrap();
            assert!(plan.probe.is_empty(), "{early} is inside the cadence");
            assert_eq!(plan.skipped[0].reason, SkipReason::Automatic);
            assert_eq!(plan.skipped[0].dormant_since.as_deref(), Some("2026-08-24T00:00:00Z"));
        }
        let plan = plan_sweep(&state, &endpoints, "2026-08-31T00:00:00Z", &t).unwrap();
        assert_eq!(plan.probe, endpoints, "seven days is due");
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn the_dormant_set_is_sliced_so_one_sweep_never_carries_all_of_it() {
        // 57 relegated together, daily sweeps: ceil(57 * 1 / 7) = 9 probed, 48 skipped.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(57, "2026-08-10T00:00:00Z", "2026-08-23T00:00:00Z");
        let plan = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(plan.probe.len(), 9);
        assert_eq!(plan.skipped.len(), 48);
        assert!(plan.skipped.iter().all(|s| s.reason == SkipReason::Automatic));
    }

    #[test]
    fn a_weekly_sweep_probes_a_weeks_worth_not_a_days_worth() {
        // The same 57 with updated_at seven days back: all 57 are due and all 57
        // are probed. A constant 1/7 slice would give each one a probe every 6.3
        // weeks, against the spec's seven days.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(57, "2026-08-10T00:00:00Z", "2026-08-17T00:00:00Z");
        let plan = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(plan.probe.len(), 57);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn the_slice_counts_only_the_endpoints_the_cadence_governs() {
        // 6 automatic dormant plus 20 pinned-awake: ceil(6*1/7) = 1, not ceil(26*1/7) = 4.
        let t = Thresholds::default();
        let mut state = State::empty();
        state.updated_at = Some(ep("2026-08-23T00:00:00Z"));
        let mut endpoints = Vec::new();
        for i in 0..6 {
            let url = format!("http://dormant{i:02}.example/s");
            state.endpoint.push(EndpointState {
                url: url.clone(),
                strikes: 2,
                strikes_before_last: 1,
                last_probed: Some(ep("2026-08-10T00:00:00Z")),
                last_cost_ms: Some(210_000),
                dormant_since: Some(ep("2026-08-10T00:00:00Z")),
                hold: None,
                lapsed_hold: None,
            });
            endpoints.push(url);
        }
        for i in 0..20 {
            let url = format!("http://pinned{i:02}.example/s");
            state.endpoint.push(EndpointState {
                url: url.clone(),
                strikes: 0,
                strikes_before_last: 0,
                last_probed: Some(ep("2026-08-10T00:00:00Z")),
                last_cost_ms: Some(210_000),
                dormant_since: Some(ep("2026-08-10T00:00:00Z")),
                hold: Some(Hold::Awake { reason: ep("operator pinned"), until: None }),
                lapsed_hold: None,
            });
            endpoints.push(url);
        }
        let plan = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(plan.probe.len(), 21, "20 held awake plus one slice of one");
        assert_eq!(plan.skipped.len(), 5);
    }

    #[test]
    fn the_cadence_does_not_drift_when_a_sweep_starts_early() {
        // last_probed 2026-09-02T19:45:03Z, sweep at 2026-09-09T19:30:00Z: due.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(1, "2026-09-02T19:45:03Z", "2026-09-08T19:30:00Z");
        let plan = plan_sweep(&state, &endpoints, "2026-09-09T19:30:00Z", &t).unwrap();
        assert_eq!(plan.probe, endpoints, "fifteen minutes early is not a day early");
    }

    // --- replay, which is the data-loss guard ---

    #[test]
    fn a_rerun_of_one_at_probes_exactly_what_that_at_probed() {
        // 57 dormant, slice 9. First run at D probes 9. A re-run at D probes those
        // SAME 9 and nothing else. Probing 18 would spend 31 extra minutes and
        // write a run graph disagreeing with the first about what it skipped.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(57, "2026-08-10T00:00:00Z", "2026-08-23T00:00:00Z");
        let first = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(first.probe.len(), 9);
        let outcomes: Vec<Outcome> = first.probe.iter().map(|u| expensive(u)).collect();
        let written =
            update(&state, &endpoints, &outcomes, "2026-08-24T00:00:00Z", &t).unwrap();
        let rerun = plan_sweep(&written, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(rerun.probe, first.probe);
        assert_eq!(rerun.skipped.len(), 48);
    }

    #[test]
    fn a_replay_with_the_same_outcome_does_not_double_strike() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://a.example/s")];
        let once = update(
            &State::empty(),
            &endpoints,
            &[expensive("http://a.example/s")],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        let twice = update(
            &once,
            &endpoints,
            &[expensive("http://a.example/s")],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        assert_eq!(twice.get("http://a.example/s").unwrap().strikes, 1);
        assert!(twice.get("http://a.example/s").unwrap().dormant_since.is_none());
    }

    #[test]
    fn a_replay_with_a_better_outcome_promotes() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://a.example/s")];
        let mut state = State::empty();
        for day in ["2026-08-24T00:00:00Z", "2026-08-25T00:00:00Z"] {
            state =
                update(&state, &endpoints, &[expensive("http://a.example/s")], day, &t).unwrap();
        }
        assert_eq!(state.get("http://a.example/s").unwrap().strikes_before_last, 1);
        let replayed = update(
            &state,
            &endpoints,
            &[answered("http://a.example/s", 5_700)],
            "2026-08-25T00:00:00Z",
            &t,
        )
        .unwrap();
        let e = replayed.get("http://a.example/s").unwrap();
        assert_eq!(e.strikes, 0);
        assert!(e.dormant_since.is_none(), "a replay carrying a better outcome promotes");
        assert_eq!(e.strikes_before_last, 1, "a replay leaves strikes_before_last alone");
    }

    #[test]
    fn two_sweeps_in_one_day_are_two_sweeps() {
        // 09:00 expensive then 21:00 positive: the promotion must land. A day-keyed
        // replay test would discard it.
        let t = Thresholds::default();
        let endpoints = vec![ep("http://twice.example/s")];
        let mut state = State::empty();
        state = update(&state, &endpoints, &[expensive("http://twice.example/s")],
                       "2026-08-24T00:00:00Z", &t).unwrap();
        state = update(&state, &endpoints, &[expensive("http://twice.example/s")],
                       "2026-08-25T09:00:00Z", &t).unwrap();
        assert_eq!(state.get("http://twice.example/s").unwrap().strikes, 2);
        let evening = update(
            &state,
            &endpoints,
            &[answered("http://twice.example/s", 5_700)],
            "2026-08-25T21:00:00Z",
            &t,
        )
        .unwrap();
        let e = evening.get("http://twice.example/s").unwrap();
        assert!(e.dormant_since.is_none());
        assert_eq!(e.last_probed.as_deref(), Some("2026-08-25T21:00:00Z"));
        assert_eq!(
            e.strikes_before_last, 2,
            "the evening sweep is its own sweep, so it inherits the morning's two strikes \
             rather than recomputing from what the morning inherited"
        );
    }

    #[test]
    fn an_out_of_order_sweep_does_not_move_state_backwards() {
        let t = Thresholds::default();
        let mut state = State::empty();
        state.updated_at = Some(ep("2026-08-25T00:00:00Z"));
        state.endpoint.push(EndpointState {
            url: ep("http://a.example/s"),
            strikes: 1,
            strikes_before_last: 0,
            last_probed: Some(ep("2026-08-25T00:00:00Z")),
            last_cost_ms: Some(210_000),
            dormant_since: None,
            hold: None,
            lapsed_hold: None,
        });
        let before = state.get("http://a.example/s").unwrap().clone();
        let after = update(
            &state,
            &[ep("http://a.example/s")],
            &[answered("http://a.example/s", 1_000)],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        assert_eq!(after.get("http://a.example/s").unwrap(), &before);
        assert_eq!(
            after.updated_at.as_deref(),
            Some("2026-08-25T00:00:00Z"),
            "the file's own instant is the newest sweep it has seen"
        );
    }

    #[test]
    fn a_crashed_run_leaves_no_replay_marker_so_its_rerun_is_a_fresh_sweep() {
        // state is written after the footer, so a crash writes nothing.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(57, "2026-08-10T00:00:00Z", "2026-08-23T00:00:00Z");
        let crashed = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert!(state.endpoint.iter().all(|e| e.last_probed.as_deref()
            != Some("2026-08-24T00:00:00Z")));
        let rerun = plan_sweep(&state, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(rerun, crashed, "the same state at the same --at is the same plan");
        assert_eq!(rerun.probe.len(), 9);
    }

    // --- operator holds ---

    #[test]
    fn an_operator_sleep_is_never_probed_and_never_promoted() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://held.example/s")];
        let state = sleep(
            &State::empty(),
            "http://held.example/s",
            "operator asked",
            "2026-08-24T00:00:00Z",
        )
        .unwrap();
        let plan = plan_sweep(&state, &endpoints, "2026-08-25T00:00:00Z", &t).unwrap();
        assert!(plan.probe.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::OperatorHold);
        assert_eq!(plan.skipped[0].dormant_since.as_deref(), Some("2026-08-24T00:00:00Z"));
        // An outcome can only arrive here if an operator slept it mid-sweep. A
        // sleep is permanent until a human wakes it, so a positive verdict
        // arriving afterwards must not promote it.
        let after = update(
            &state,
            &endpoints,
            &[answered("http://held.example/s", 1_000)],
            "2026-08-25T00:00:00Z",
            &t,
        )
        .unwrap();
        let e = after.get("http://held.example/s").unwrap();
        assert_eq!(e.dormant_since.as_deref(), Some("2026-08-24T00:00:00Z"));
        assert!(matches!(e.hold, Some(Hold::Dormant { .. })));
    }

    #[test]
    fn an_operator_sleep_outranks_a_replay() {
        // Sweep at D probes E; the operator sleeps E; a re-run at D must NOT probe it.
        let t = Thresholds::default();
        let endpoints = vec![ep("http://e.example/s"), ep("http://f.example/s")];
        let swept = update(
            &State::empty(),
            &endpoints,
            &[expensive("http://e.example/s"), expensive("http://f.example/s")],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        let slept =
            sleep(&swept, "http://e.example/s", "operator asked", "2026-08-24T00:00:00Z")
                .unwrap();
        let rerun = plan_sweep(&slept, &endpoints, "2026-08-24T00:00:00Z", &t).unwrap();
        assert_eq!(rerun.probe, vec![ep("http://f.example/s")]);
        assert_eq!(rerun.skipped.len(), 1);
        assert_eq!(rerun.skipped[0].url, ep("http://e.example/s"));
        assert_eq!(rerun.skipped[0].reason, SkipReason::OperatorHold);
    }

    #[test]
    fn an_operator_slept_endpoint_carries_the_instant_it_was_slept() {
        let first =
            sleep(&State::empty(), "http://held.example/s", "operator asked", "2026-08-24T00:00:00Z")
                .unwrap();
        assert_eq!(
            first.get("http://held.example/s").unwrap().dormant_since.as_deref(),
            Some("2026-08-24T00:00:00Z")
        );
        let again =
            sleep(&first, "http://held.example/s", "operator asked again", "2026-09-01T00:00:00Z")
                .unwrap();
        assert_eq!(
            again.get("http://held.example/s").unwrap().dormant_since.as_deref(),
            Some("2026-08-24T00:00:00Z"),
            "a run graph already published the original relegation instant"
        );
    }

    #[test]
    fn sleeping_a_never_probed_endpoint_writes_a_state_that_parses() {
        // The bricking case: dormant_since set with last_probed absent is legal
        // when a Dormant hold is present, and read_state must accept it.
        let slept = sleep(&State::empty(), "http://new.example/s", "operator asked",
                          "2026-08-26T00:00:00Z").unwrap();
        State::parse(&slept.render()).unwrap();
    }

    #[test]
    fn a_wake_is_immune_from_relegation_and_not_merely_from_skipping() {
        let endpoints = vec![ep("http://a.example/s")];
        let t = Thresholds::default();
        let mut state = State::empty();
        for day in ["2026-08-24T00:00:00Z", "2026-08-25T00:00:00Z"] {
            state = update(&state, &endpoints, &[expensive("http://a.example/s")], day, &t).unwrap();
        }
        assert!(state.get("http://a.example/s").unwrap().dormant_since.is_some());
        let mut woken = wake(&state, "http://a.example/s", "operator emailed",
                             "2026-08-26T00:00:00Z", false, &t).unwrap();
        assert!(woken.get("http://a.example/s").unwrap().dormant_since.is_none(),
                "a wake that leaves it relegated is not immunity");
        for day in ["2026-08-27T00:00:00Z", "2026-08-28T00:00:00Z"] {
            assert_eq!(plan_sweep(&woken, &endpoints, day, &t).unwrap().probe.len(), 1);
            woken = update(&woken, &endpoints, &[expensive("http://a.example/s")], day, &t).unwrap();
        }
        assert_eq!(woken.get("http://a.example/s").unwrap().strikes, 2, "strikes accumulate");
        assert!(woken.get("http://a.example/s").unwrap().dormant_since.is_none(),
                "but rule F does not relegate under a live Awake hold");
        // The grace ran to 09-02, so on 09-03 rule A clears the hold and the strikes.
        let lapsed = update(&woken, &endpoints, &[], "2026-09-03T00:00:00Z", &t).unwrap();
        assert!(lapsed.get("http://a.example/s").unwrap().hold.is_none());
        assert_eq!(lapsed.get("http://a.example/s").unwrap().strikes, 0);
        assert_eq!(lapsed.get("http://a.example/s").unwrap().strikes_before_last, 0,
                   "or a replay of the last grace sweep re-relegates at once");
        assert!(lapsed.get("http://a.example/s").unwrap().dormant_since.is_none());
        assert_eq!(lapsed.get("http://a.example/s").unwrap().lapsed_hold.as_deref(),
                   Some("operator emailed"));
        // Relegation now costs two fresh sweeps.
        assert_eq!(plan_sweep(&lapsed, &endpoints, "2026-09-04T00:00:00Z", &t).unwrap().probe.len(), 1);
    }

    #[test]
    fn a_pinned_wake_never_lapses() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://pinned.example/s")];
        let pinned = wake(
            &State::empty(),
            "http://pinned.example/s",
            "operator pinned",
            "2026-08-24T00:00:00Z",
            true,
            &t,
        )
        .unwrap();
        assert!(matches!(
            pinned.get("http://pinned.example/s").unwrap().hold,
            Some(Hold::Awake { until: None, .. })
        ));
        let mut later = pinned;
        for day in ["2099-01-01T00:00:00Z", "2099-01-02T00:00:00Z", "2099-01-03T00:00:00Z"] {
            assert_eq!(plan_sweep(&later, &endpoints, day, &t).unwrap().probe.len(), 1);
            later =
                update(&later, &endpoints, &[expensive("http://pinned.example/s")], day, &t)
                    .unwrap();
        }
        let e = later.get("http://pinned.example/s").unwrap();
        assert!(matches!(e.hold, Some(Hold::Awake { until: None, .. })), "a pin does not lapse");
        assert!(e.lapsed_hold.is_none());
        assert_eq!(e.strikes, 3, "strikes still accumulate underneath");
        assert!(e.dormant_since.is_none(), "a pin suppresses relegation for as long as it stands");
    }

    #[test]
    fn a_lapsed_hold_is_cleared_even_for_an_endpoint_the_registry_dropped() {
        let t = Thresholds::default();
        let mut state = State::empty();
        state.endpoint.push(EndpointState {
            url: ep("http://dropped.example/s"),
            strikes: 1,
            strikes_before_last: 1,
            last_probed: Some(ep("2026-08-25T00:00:00Z")),
            last_cost_ms: Some(210_000),
            dormant_since: None,
            hold: Some(Hold::Awake {
                reason: ep("operator emailed"),
                until: Some(ep("2026-08-25T00:00:00Z")),
            }),
            lapsed_hold: None,
        });
        let after = update(&state, &[], &[], "2026-08-26T00:00:00Z", &t).unwrap();
        let e = after.get("http://dropped.example/s").expect("no pruning");
        assert!(e.hold.is_none());
        assert_eq!(e.lapsed_hold.as_deref(), Some("operator emailed"));
        assert_eq!(e.strikes, 0);
        assert_eq!(e.strikes_before_last, 0);
    }

    // --- state file shape ---

    #[test]
    fn state_round_trips_through_toml() {
        let state = State {
            version: STATE_VERSION,
            updated_at: Some(ep("2026-08-26T00:00:00Z")),
            endpoint: vec![
                EndpointState {
                    url: ep("http://a.example/s"),
                    strikes: 2,
                    strikes_before_last: 1,
                    last_probed: Some(ep("2026-08-25T00:00:00Z")),
                    last_cost_ms: Some(210_017),
                    dormant_since: Some(ep("2026-08-25T00:00:00Z")),
                    hold: None,
                    lapsed_hold: Some(ep("operator emailed")),
                },
                EndpointState {
                    url: ep("http://b.example/s"),
                    strikes: 0,
                    strikes_before_last: 0,
                    last_probed: None,
                    last_cost_ms: None,
                    dormant_since: Some(ep("2026-08-26T00:00:00Z")),
                    hold: Some(Hold::Dormant { reason: ep("operator asked") }),
                    lapsed_hold: None,
                },
                EndpointState {
                    url: ep("http://c.example/s"),
                    strikes: 1,
                    strikes_before_last: 0,
                    last_probed: Some(ep("2026-08-26T00:00:00Z")),
                    last_cost_ms: Some(4_000),
                    dormant_since: None,
                    hold: Some(Hold::Awake {
                        reason: ep("operator emailed"),
                        until: Some(ep("2026-09-02T00:00:00Z")),
                    }),
                    lapsed_hold: None,
                },
                EndpointState {
                    url: ep("http://d.example/s"),
                    hold: Some(Hold::Awake { reason: ep("operator pinned"), until: None }),
                    ..Default::default()
                },
            ],
        };
        let rendered = state.render();
        assert!(
            rendered.lines().next().unwrap().starts_with('#'),
            "the file opens with a comment saying what wrote it: {rendered}"
        );
        assert!(rendered.contains("dormancy"), "and the comment names the dormancy binary");
        assert_eq!(State::parse(&rendered).unwrap(), state);
    }

    #[test]
    fn an_unknown_state_version_is_refused() {
        let error = State::parse("version = 2\n").unwrap_err().to_string();
        assert!(error.contains('2'), "{error}");
        assert!(error.contains('1'), "{error}");
        State::parse("version = 1\n").expect("a state with no endpoints is a state");
    }

    #[test]
    fn parse_names_the_field_of_a_malformed_instant() {
        let error = State::parse(
            "version = 1\n\n[[endpoint]]\nurl = \"http://a.example/s\"\nlast_probed = \"banana\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("last_probed"), "{error}");
        assert!(error.contains("banana"), "{error}");
        assert!(error.contains("http://a.example/s"), "{error}");
    }

    #[test]
    fn parse_refuses_dormant_since_without_last_probed_when_there_is_no_hold() {
        let unheld = "version = 1\n\n[[endpoint]]\nurl = \"http://a.example/s\"\n\
                      dormant_since = \"2026-08-26T00:00:00Z\"\n";
        let error = State::parse(unheld).unwrap_err().to_string();
        assert!(error.contains("dormant_since"), "{error}");
        assert!(error.contains("last_probed"), "{error}");
        assert!(error.contains("http://a.example/s"), "{error}");
        let held = format!("{unheld}\n[endpoint.hold]\nstate = \"dormant\"\nreason = \"operator asked\"\n");
        State::parse(&held).expect("a Dormant hold is what dormant_since means here");
    }

    #[test]
    fn duplicate_outcomes_are_an_error_not_two_strikes() {
        let t = Thresholds::default();
        let endpoints = vec![ep("http://a.example/s")];
        let error = update(
            &State::empty(),
            &endpoints,
            &[expensive("http://a.example/s"), expensive("http://a.example/s")],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("http://a.example/s"), "{error}");
    }

    #[test]
    fn a_narrowed_sweep_does_not_delete_the_rest_of_the_state() {
        // The 57 were re-probed alone with their own --endpoints file. Pruning
        // here would have destroyed the other 486 entries.
        let t = Thresholds::default();
        let (state, endpoints) =
            dormant_fleet(3, "2026-08-10T00:00:00Z", "2026-08-23T00:00:00Z");
        let narrowed = update(
            &state,
            &endpoints[0..1],
            &[expensive(&endpoints[0])],
            "2026-08-24T00:00:00Z",
            &t,
        )
        .unwrap();
        assert_eq!(narrowed.endpoint.len(), 3);
        assert_eq!(
            narrowed.get(&endpoints[2]).unwrap().last_probed.as_deref(),
            Some("2026-08-10T00:00:00Z")
        );
    }

    #[test]
    fn prunable_reports_unheld_entries_and_keeps_held_ones() {
        let mut state = State::empty();
        state.endpoint.push(entry("http://listed.example/s"));
        state.endpoint.push(EndpointState {
            url: ep("http://pinned.example/s"),
            hold: Some(Hold::Awake { reason: ep("operator pinned"), until: None }),
            ..Default::default()
        });
        state.endpoint.push(EndpointState {
            url: ep("http://slept.example/s"),
            dormant_since: Some(ep("2026-08-24T00:00:00Z")),
            hold: Some(Hold::Dormant { reason: ep("operator asked") }),
            ..Default::default()
        });
        state.endpoint.push(entry("http://gone.example/s"));
        let (pruned, kept) = prunable(&state, &[ep("http://listed.example/s")]);
        assert_eq!(pruned, vec![ep("http://gone.example/s")]);
        assert_eq!(kept.endpoint.len(), 3);
        assert!(kept.get("http://pinned.example/s").is_some(), "a hold outlives the registry");
        assert!(kept.get("http://slept.example/s").is_some());
        assert!(kept.get("http://gone.example/s").is_none());
    }

    #[test]
    fn the_two_skip_reason_slugs_are_automatic_and_operator_hold() {
        assert_eq!(SkipReason::Automatic.slug(), "automatic");
        assert_eq!(SkipReason::OperatorHold.slug(), "operator-hold");
    }

    // --- the instant helper, which owns the day arithmetic ---

    #[test]
    fn the_day_helper_places_a_leap_day() {
        let day = |i: &str| day_of(i, "test").unwrap();
        assert_eq!(day("2028-02-29T00:00:00Z") - day("2028-02-28T00:00:00Z"), 1);
        assert_eq!(day("2028-03-01T00:00:00Z") - day("2028-02-29T00:00:00Z"), 1);
        assert_eq!(day("2029-03-01T00:00:00Z") - day("2028-03-01T00:00:00Z"), 365);
        // 2027 has no 29 February, so that is not an instant.
        assert!(day_of("2027-02-29T00:00:00Z", "test").is_err());
    }

    #[test]
    fn the_day_helper_crosses_a_year_boundary() {
        let day = |i: &str| day_of(i, "test").unwrap();
        assert_eq!(day("2027-01-01T00:00:00Z") - day("2026-12-31T23:59:59Z"), 1);
        assert_eq!(day("1970-01-01T00:00:00Z"), 0);
        assert_eq!(day("1969-12-31T00:00:00Z"), -1);
    }

    #[test]
    fn an_instant_with_an_offset_is_refused() {
        let error = day_of("2026-08-24T00:00:00+02:00", "--at").unwrap_err().to_string();
        assert!(error.contains("--at"), "{error}");
        assert!(error.contains("2026-08-24T00:00:00+02:00"), "{error}");
    }

    #[test]
    fn an_instant_with_a_fractional_second_is_refused() {
        // Two lexical forms for one instant would give a replay two run IRIs and
        // no string match, which is what replay detection is built on.
        assert!(day_of("2026-08-24T00:00:00.000Z", "--at").is_err());
    }

    #[test]
    fn a_leap_second_is_refused() {
        // A day-number helper cannot place second 60.
        assert!(day_of("2026-12-31T23:59:60Z", "--at").is_err());
        assert!(day_of("2026-12-31T23:59:59Z", "--at").is_ok());
    }

    #[test]
    fn the_day_helper_measures_the_cadence_from_an_early_start() {
        // Rule 6's early-start case: 6 days 23 h 45 min apart is seven days.
        let day = |i: &str| day_of(i, "test").unwrap();
        assert_eq!(day("2026-09-09T19:30:00Z") - day("2026-09-02T19:45:03Z"), 7);
    }

    #[test]
    fn a_grace_period_keeps_the_time_of_day_it_started_at() {
        assert_eq!(
            instant_plus_days("2026-08-26T13:45:07Z", "--at", 7).unwrap(),
            "2026-09-02T13:45:07Z"
        );
        assert_eq!(
            instant_plus_days("2028-02-22T00:00:00Z", "--at", 7).unwrap(),
            "2028-02-29T00:00:00Z"
        );
    }

    #[test]
    fn a_malformed_now_is_refused_and_named() {
        let t = Thresholds::default();
        for error in [
            plan_sweep(&State::empty(), &[], "banana", &t).unwrap_err().to_string(),
            update(&State::empty(), &[], &[], "banana", &t).unwrap_err().to_string(),
            wake(&State::empty(), "http://a.example/s", "why", "banana", false, &t)
                .unwrap_err()
                .to_string(),
            sleep(&State::empty(), "http://a.example/s", "why", "banana").unwrap_err().to_string(),
        ] {
            assert!(error.contains("banana"), "{error}");
        }
    }
}

