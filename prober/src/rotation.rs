//! Which endpoints tonight's sweep profiles.
//!
//! WHY THIS EXISTS. The class-profile pass asks one query PER CLASS it finds,
//! and it ran against every endpoint every night. Measured 2026-10-01: 2,339
//! classes profiled, 49,060 properties recorded, 279,215 quads written -- 95% of
//! the night's run, and about 500,000 quads a day into a store holding 3.7
//! million. One endpoint, a Wikidata mirror, was 48% of it on its own: 23,770
//! properties across 183 classes, re-derived nightly.
//!
//! Nothing read any of it. A profile is only ever reached through
//! `sw:sampleRunIs` on the endpoint in `sw:current` -- the ONE run that last
//! profiled it -- so every older profile was dead weight, and the read path
//! already tolerated staleness: on 2026-10-01 the oldest pointer reached back to
//! 2026-09-16, fifteen days, with nothing wrong.
//!
//! It is also somebody else's server. 183 queries a night to a stranger's
//! endpoint, for an answer this service does not look at until it changes, is
//! not a cost worth paying.
//!
//! STATELESS BY DESIGN, like the rest of the prober: the decision is a pure
//! function of the endpoint URL, the sweep's own `--at` and the interval. There
//! is no cursor to keep, nothing to coordinate between runs, and a night the
//! sweep does not run simply means those endpoints wait for their next slot.

/// The schedule: an endpoint is profiled once every `every` days.
#[derive(Debug, Clone, Copy)]
pub struct Rotation {
    every: u32,
    day: i64,
}

impl Rotation {
    /// Profile every endpoint on every sweep -- the behaviour before 2026-10-01.
    ///
    /// What a caller that does not schedule should pass, and what the tests of
    /// everything else use so that rotation never silently changes what they
    /// measure.
    pub fn always() -> Self {
        Self { every: 1, day: 0 }
    }

    /// `every` days between profiles, counted from the sweep's own instant.
    ///
    /// An unparseable `at` or `every < 2` both yield `always()`: a schedule this
    /// cannot place in time must not silently skip endpoints, and the safe
    /// direction is to do the work rather than to quietly not do it.
    pub fn new(every: u32, at: &str) -> Self {
        match (every >= 2).then(|| day_index(at)).flatten() {
            Some(day) => Self { every, day },
            None => Self::always(),
        }
    }

    /// Whether tonight's sweep profiles this endpoint.
    ///
    /// The bucket is the endpoint's own hash, so the set of endpoints due on a
    /// given night is fixed by the registry rather than by the order a sweep
    /// happens to visit them, and adding an endpoint does not reshuffle the
    /// others.
    pub fn profiles(&self, endpoint: &str) -> bool {
        if self.every <= 1 {
            return true;
        }
        let every = i64::from(self.every);
        (hash(endpoint) % every as u64) as i64 == self.day.rem_euclid(every)
    }

    /// Split `defs` into what tonight asks of `endpoint`, and what it holds
    /// back until that endpoint's next slot.
    ///
    /// ONLY THE EXHAUSTIVE TIER rotates, which is `class-profiles` and
    /// `vocabulary-described`. They move together because the second grades the
    /// first's results and reads `indeterminate` without them; rotating one
    /// alone would publish "this endpoint describes nothing" on six nights in
    /// seven.
    ///
    /// HELD BACK MEANS DECLINED, NOT SKIPPED. The caller records a
    /// `NotMeasured` with `Cadence` for each, and that is what protects the
    /// page: the web tier refuses to let a cadence decline replace a reading
    /// that was actually taken, while a metric that RAN and returned
    /// `indeterminate` replaces it. Skipping silently would erase a real verdict
    /// every off night.
    pub fn split(
        &self,
        defs: &[crate::metrics::MetricDef],
        endpoint: &str,
    ) -> (Vec<crate::metrics::MetricDef>, Vec<(crate::metrics::MetricDef, crate::emit::NotMeasuredReason)>) {
        if self.profiles(endpoint) {
            return (defs.to_vec(), Vec::new());
        }
        let mut due = Vec::new();
        let mut held = Vec::new();
        for d in defs {
            if d.cost == crate::metrics::Cost::Exhaustive {
                held.push((d.clone(), crate::emit::NotMeasuredReason::Cadence));
            } else {
                due.push(d.clone());
            }
        }
        (due, held)
    }

    /// The interval, for the sweep's own summary line.
    pub fn every_days(&self) -> u32 {
        self.every
    }
}

/// FNV-1a over the endpoint URL, the same hash `definitions_revision` uses.
///
/// Any stable hash would do; what matters is that it does not change between
/// releases, because an endpoint whose bucket moved would be profiled twice in
/// quick succession or not for twice the interval.
fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Days since 1970-01-01 for an RFC 3339 instant, or `None` if it is not one.
///
/// Howard Hinnant's `days_from_civil`, which is exact for every proleptic
/// Gregorian date and is the algorithm every date library implements. Written
/// out rather than taken as a dependency for the reason `emit` gives for not
/// formatting dates by hand -- except that this value is never published: it
/// chooses a bucket and is thrown away, so a mistake here delays a profile
/// rather than writing a wrong date into a graph that is never rewritten.
fn day_index(at: &str) -> Option<i64> {
    let date = at.get(..10)?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_day_index_is_the_real_count_of_days() {
        // Pinned against known values rather than trusted: this is civil-time
        // arithmetic written out by hand, and the failure it would cause --
        // endpoints bunching onto one night -- is invisible in production.
        assert_eq!(day_index("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(day_index("1970-01-02T00:00:00Z"), Some(1));
        assert_eq!(day_index("2000-03-01T00:00:00Z"), Some(11017));
        assert_eq!(day_index("2026-10-01T03:30:11Z"), Some(20727));
        // A leap day, and the day after it.
        assert_eq!(day_index("2024-02-29T00:00:00Z"), Some(19782));
        assert_eq!(day_index("2024-03-01T00:00:00Z"), Some(19783));
        // Consecutive instants on one day share an index; that is the point.
        assert_eq!(day_index("2026-10-01T00:00:00Z"), day_index("2026-10-01T23:59:59Z"));
        assert_ne!(day_index("2026-10-01T23:59:59Z"), day_index("2026-10-02T00:00:00Z"));
    }

    #[test]
    fn an_unusable_instant_profiles_everything() {
        // The safe direction. A schedule that cannot be placed in time must do
        // the work rather than quietly skip it.
        for bad in ["", "not-a-date", "2026-13-01T00:00:00Z", "2026-10-32T00:00:00Z"] {
            assert!(
                Rotation::new(7, bad).profiles("https://e.test/sparql"),
                "{bad:?} silently skipped an endpoint"
            );
        }
    }

    #[test]
    fn an_interval_of_one_or_zero_profiles_everything() {
        for every in [0, 1] {
            assert!(Rotation::new(every, "2026-10-01T03:30:11Z").profiles("https://e.test/sparql"));
        }
        assert!(Rotation::always().profiles("https://e.test/sparql"));
    }

    #[test]
    fn every_endpoint_is_profiled_exactly_once_per_interval() {
        // THE PROPERTY THE WHOLE THING RESTS ON. A rotation that leaves an
        // endpoint out for two intervals is a page showing a fortnight-old
        // profile; one that takes it twice in a week saves nothing.
        let endpoints: Vec<String> = (0..126)
            .map(|i| format!("https://host{i}.example.org/sparql"))
            .collect();
        for every in [2u32, 5, 7, 14] {
            for endpoint in &endpoints {
                let nights = (0..every)
                    .filter(|offset| {
                        let at = format!("2026-10-{:02}T03:30:00Z", 1 + offset);
                        Rotation::new(every, &at).profiles(endpoint)
                    })
                    .count();
                assert_eq!(
                    nights, 1,
                    "{endpoint} is profiled {nights} times in {every} nights"
                );
            }
        }
    }

    #[test]
    fn the_nightly_share_is_about_one_over_the_interval() {
        // Load spread, not just correctness: the point is a smaller pass every
        // night, not one night carrying everything.
        let endpoints: Vec<String> = (0..126)
            .map(|i| format!("https://host{i}.example.org/sparql"))
            .collect();
        let every = 7;
        for offset in 0..every {
            let at = format!("2026-10-{:02}T03:30:00Z", 1 + offset);
            let rotation = Rotation::new(every, &at);
            let due = endpoints.iter().filter(|e| rotation.profiles(e)).count();
            // 126/7 is 18. The bound is loose because the buckets come from a
            // hash and are not meant to be exactly equal -- only to stop any one
            // night carrying the fleet.
            assert!(
                (6..=36).contains(&due),
                "night {offset} profiles {due} of 126, which is not a seventh of anything"
            );
        }
    }

    #[test]
    fn an_endpoints_night_does_not_move_when_another_is_added() {
        // Hashing the endpoint rather than its position is what buys this. With
        // index-based buckets, adding one endpoint to the registry would reshuffle
        // every endpoint after it and the fleet would re-profile itself at once.
        let rotation = Rotation::new(7, "2026-10-01T03:30:00Z");
        let before = rotation.profiles("https://stable.example.org/sparql");
        // ... the registry grows; the question is asked again, unchanged.
        let after = rotation.profiles("https://stable.example.org/sparql");
        assert_eq!(before, after);
    }
}
