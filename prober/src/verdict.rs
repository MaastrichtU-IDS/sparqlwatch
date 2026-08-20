use serde::{Deserialize, Serialize};

/// The closed verdict vocabulary. Every state here was observed in the LOD
/// Cloud survey (see the spec's Conformance model section); none is
/// speculative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Verdict {
    /// A probe confirms it works, and it is declared.
    Verified,
    /// Works, but the endpoint advertises nothing. The common case: 18
    /// endpoints evaluate geof:sfWithin and none declares it.
    UndeclaredButVerified,
    /// Claimed or bound, but behaves incorrectly. 9 endpoints answered a
    /// point-in-polygon filter `false` when a conformant engine must say true.
    DeclaredButWrong,
    /// Claimed, not confirmable by probe.
    DeclaredOnly,
    /// Neither claimed nor observed.
    Absent,
    /// The engine or the time budget prevented an answer. Never a silent zero.
    Indeterminate,
}

impl Verdict {
    pub const ALL: [Verdict; 6] = [
        Verdict::Verified,
        Verdict::UndeclaredButVerified,
        Verdict::DeclaredOnly,
        Verdict::Absent,
        Verdict::DeclaredButWrong,
        Verdict::Indeterminate,
    ];

    /// Lower is better. Used only for ordering a worklist, never published as
    /// a score.
    pub fn severity(&self) -> u8 {
        match self {
            Verdict::Verified => 0,
            Verdict::UndeclaredButVerified => 1,
            Verdict::DeclaredOnly => 2,
            Verdict::Absent => 3,
            Verdict::DeclaredButWrong => 4,
            Verdict::Indeterminate => 5,
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::UndeclaredButVerified => "undeclared-but-verified",
            Verdict::DeclaredButWrong => "declared-but-wrong",
            Verdict::DeclaredOnly => "declared-only",
            Verdict::Absent => "absent",
            Verdict::Indeterminate => "indeterminate",
        }
    }
}

/// A graded conformance level, 0..=4. Used where a boolean would credit the
/// engine rather than the publisher, e.g. service-description informativeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level(pub u8);

impl Level {
    pub fn new(v: u8) -> Option<Level> {
        if v <= 4 { Some(Level(v)) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_but_wrong_is_more_severe_than_absent() {
        // A false claim misleads a client that trusts it, so it ranks worse
        // than simply not having the capability.
        assert!(Verdict::DeclaredButWrong.severity() > Verdict::Absent.severity());
    }

    #[test]
    fn verified_is_least_severe() {
        for v in Verdict::ALL {
            assert!(Verdict::Verified.severity() <= v.severity());
        }
    }

    #[test]
    fn slugs_are_stable_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for v in Verdict::ALL {
            assert!(seen.insert(v.slug()), "duplicate slug {}", v.slug());
        }
        assert_eq!(Verdict::UndeclaredButVerified.slug(), "undeclared-but-verified");
    }

    #[test]
    fn levels_are_bounded_to_zero_through_four() {
        assert!(Level::new(4).is_some());
        assert!(Level::new(5).is_none());
    }
}
