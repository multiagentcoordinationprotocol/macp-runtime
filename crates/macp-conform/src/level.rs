//! Conformance levels and fixture grouping (spec `docs/conformance.md`).
//!
//! | Level | Fixtures |
//! |---|---|
//! | Core | `session_start_*.json`, and the `cmt-hash/` vectors |
//! | Core + Decision | every `decision_*.json` |
//! | Standard modes | every `proposal_*`, `task_*`, `handoff_*` and `quorum_*` file |
//!
//! Each level includes the levels above it. `multi_round_*.json` is the
//! optional "+ multi_round" group, part of no level. A file matching none of
//! these is `unclassified`: it is run and reported, but counts toward nothing.

use serde::Serialize;

/// A conformance level. Ordered: `Core < Decision < Standard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Core.
    Core,
    /// Core + Decision.
    Decision,
    /// Standard modes.
    Standard,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Core, Level::Decision, Level::Standard];

    /// The name a claim uses.
    pub fn claim_name(self) -> &'static str {
        match self {
            Level::Core => "Core",
            Level::Decision => "Core + Decision",
            Level::Standard => "Standard modes",
        }
    }

    /// Parse `core`, `decision` or `standard`.
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "core" => Some(Level::Core),
            "decision" | "core+decision" => Some(Level::Decision),
            "standard" | "standard-modes" => Some(Level::Standard),
            _ => None,
        }
    }
}

/// The group a fixture file counts toward. Serializes as `core`,
/// `decision`, `standard`, `multi_round` or `unclassified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Level(Level),
    /// The optional `ext.multi_round.v1` fixtures.
    MultiRound,
    Unclassified,
}

impl Level {
    /// The report's name: `core`, `decision` or `standard`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Core => "core",
            Level::Decision => "decision",
            Level::Standard => "standard",
        }
    }
}

impl Serialize for Level {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl Serialize for Group {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Group::Level(l) => l.as_str(),
            Group::MultiRound => "multi_round",
            Group::Unclassified => "unclassified",
        })
    }
}

/// Group a session fixture by its file name.
pub fn group_of_fixture(file_name: &str) -> Group {
    let starts = |p: &str| file_name.starts_with(p);
    if starts("session_start_") {
        Group::Level(Level::Core)
    } else if starts("decision_") {
        Group::Level(Level::Decision)
    } else if starts("proposal_") || starts("task_") || starts("handoff_") || starts("quorum_") {
        Group::Level(Level::Standard)
    } else if starts("multi_round_") {
        Group::MultiRound
    } else {
        Group::Unclassified
    }
}

/// Commitment-hash vectors are Core (and the whole of the Verifier role).
pub fn group_of_vector(_file_name: &str) -> Group {
    Group::Level(Level::Core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups() {
        assert_eq!(
            group_of_fixture("session_start_reject_paths.json"),
            Group::Level(Level::Core)
        );
        assert_eq!(
            group_of_fixture("decision_plurality.json"),
            Group::Level(Level::Decision)
        );
        for f in [
            "proposal_x.json",
            "task_x.json",
            "handoff_x.json",
            "quorum_x.json",
        ] {
            assert_eq!(group_of_fixture(f), Group::Level(Level::Standard));
        }
        assert_eq!(
            group_of_fixture("multi_round_happy_path.json"),
            Group::MultiRound
        );
        assert_eq!(group_of_fixture("other.json"), Group::Unclassified);
        assert!(Level::Core < Level::Decision && Level::Decision < Level::Standard);
    }
}
