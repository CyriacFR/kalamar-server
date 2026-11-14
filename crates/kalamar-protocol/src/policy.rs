//! Result policies (`api.md`, "Result policies"): the declared rules a server decides results with.
//!
//! Every enum has an `Unknown` variant so a client can read any server's declaration; a server only declares values
//! it implements (the reference service refuses a configuration with a value it does not implement).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How much support a result needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quorum {
    Any,
    Majority,
    All,
    /// At least min(k, participants).
    AtLeast(u32),
    Unknown,
}

impl Quorum {
    /// Whether `support` of `n` participants meets this quorum. `None` for [`Quorum::Unknown`].
    pub fn met(self, support: usize, n: usize) -> Option<bool> {
        Some(match self {
            Quorum::Any => support >= 1,
            Quorum::Majority => support * 2 > n,
            Quorum::All => support >= n,
            Quorum::AtLeast(k) => support >= (k as usize).min(n).max(1),
            Quorum::Unknown => return None,
        })
    }
}

impl Serialize for Quorum {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Quorum::Any => s.serialize_str("any"),
            Quorum::Majority => s.serialize_str("majority"),
            Quorum::All => s.serialize_str("all"),
            Quorum::AtLeast(k) => s.serialize_u32(*k),
            Quorum::Unknown => s.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for Quorum {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(match &v {
            serde_json::Value::String(s) => match s.as_str() {
                "any" => Quorum::Any,
                "majority" => Quorum::Majority,
                "all" => Quorum::All,
                _ => Quorum::Unknown,
            },
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(k) if (1..=64).contains(&k) => Quorum::AtLeast(k as u32),
                _ => Quorum::Unknown,
            },
            _ => Quorum::Unknown,
        })
    }
}

macro_rules! open_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant,)+ Unknown }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text,)+ $name::Unknown => "unknown" }
            }
            pub fn parse(s: &str) -> Self {
                match s { $($text => $name::$variant,)+ _ => $name::Unknown }
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.as_str()) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok($name::parse(&s))
            }
        }
    };
}

open_enum!(
    /// What silence (no terminal report) counts for.
    Silence { ConsentAfterPlay => "consent_after_play", Ignore => "ignore" }
);
open_enum!(
    /// What happens when valid results differ.
    Disagreement { Majority => "majority", Void => "void", Rematch => "rematch" }
);
open_enum!(
    /// What happens when no valid result arrives.
    Abort { Void => "void", Rematch => "rematch" }
);
open_enum!(
    /// How disconnected players affect the winner.
    Disconnect { GameResult => "game_result", Forfeit => "forfeit" }
);
open_enum!(
    /// What happens when a participant has not launched by the launch deadline.
    NoShow { Void => "void", Rematch => "rematch" }
);
open_enum!(
    /// 1.1.1: what a loadout violation in the accepted record does.
    LoadoutCheck { Ignore => "ignore", Void => "void", Forfeit => "forfeit" }
);
open_enum!(
    /// 1.1: who decides (`operators.md`).
    Decider { Auto => "auto", Referee => "referee", AutoThenReferee => "auto_then_referee" }
);
open_enum!(
    /// 1.1.7 (`spectators.md`): what the reports of the match's spectators (witnesses) are used for.
    WitnessUse { Ignore => "ignore", Tiebreak => "tiebreak", Required => "required" }
);
open_enum!(
    /// 1.1.7: whose witness reports count: every spectator's, or the referees' only.
    WitnessFrom { Any => "any", Referees => "referees" }
);
open_enum!(
    /// 1.1.8 (`api.md`, "Result decision", step 2): what the reports' counters (the teams' scores and each player's
    /// statistics) weigh. `flag`: reports agree on what decides the match and their counters are compared with a wide
    /// tolerance, a larger gap being flagged without voiding anything; `ignore`: the counters are not compared;
    /// `strict`: every shared field must be equal (which voids real matches: each game computes its own counters).
    StatsCheck { Flag => "flag", Ignore => "ignore", Strict => "strict" }
);
open_enum!(
    /// 1.1.11 (`leaving.md`, `api.md` "Result decision", step 3b): what a player's voluntary departure from a match in
    /// play does when the match has no valid result. `void`: the match is void (the default); `forfeit`: the
    /// leaver's team loses, counted and rated as any result; `forfeit_unrated`: the same, but no rating changes. Only a
    /// departure the leaver's own launcher reported counts (`left-by-player` with `in_play: true`): a crash or a lost
    /// connection never makes anyone lose.
    Leave { Void => "void", Forfeit => "forfeit", ForfeitUnrated => "forfeit_unrated" }
);

#[allow(clippy::derivable_impls)]
impl Default for Leave {
    /// A declaration without `leave` voids such a match.
    fn default() -> Self {
        Leave::Void
    }
}

/// A declared policy without `stats` compares every shared field exactly.
fn stats_strict() -> StatsCheck {
    StatsCheck::Strict
}

#[allow(clippy::derivable_impls)]
impl Default for WitnessUse {
    fn default() -> Self {
        WitnessUse::Ignore
    }
}

#[allow(clippy::derivable_impls)]
impl Default for WitnessFrom {
    fn default() -> Self {
        WitnessFrom::Any
    }
}

#[allow(clippy::derivable_impls)] // the enum comes from a macro
impl Default for LoadoutCheck {
    fn default() -> Self {
        LoadoutCheck::Ignore
    }
}

#[allow(clippy::derivable_impls)]
impl Default for Decider {
    fn default() -> Self {
        Decider::Auto
    }
}

/// Deadlines of a policy, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadlines {
    pub accept_s: u32,
    pub launch_s: u32,
    pub result_s: u32,
    pub grace_s: u32,
}

/// The decision-relevant members of a declared result policy (presentation members are the server's).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub quorum: Quorum,
    pub silence: Silence,
    pub disagreement: Disagreement,
    pub rated_requires_both_teams: bool,
    pub abort: Abort,
    pub disconnect: Disconnect,
    pub no_show: NoShow,
    pub deadlines: Deadlines,
    #[serde(default)]
    pub loadout_check: LoadoutCheck,
    #[serde(default)]
    pub decider: Decider,
    /// 1.1.7: what the spectators' reports are used for (`ignore` by default).
    #[serde(default)]
    pub witness: WitnessUse,
    /// 1.1.7: whose spectator reports count as a witness's (`any` by default).
    #[serde(default)]
    pub witness_from: WitnessFrom,
    /// 1.1.8: what the reports' counters weigh. Absent from a declaration: `strict` (every shared field is
    /// compared); the reference service declares it on every policy (`flag` unless configured).
    #[serde(default = "stats_strict")]
    pub stats: StatsCheck,
    /// 1.1.11: what a voluntary departure in play does to a match without a valid result (`void` by default).
    #[serde(default)]
    pub leave: Leave,
}

impl Policy {
    /// The reference policy `standard` (api.md, "Guarantees").
    pub fn standard() -> Policy {
        Policy {
            quorum: Quorum::Majority,
            silence: Silence::ConsentAfterPlay,
            disagreement: Disagreement::Majority,
            rated_requires_both_teams: true,
            abort: Abort::Void,
            disconnect: Disconnect::GameResult,
            no_show: NoShow::Void,
            deadlines: Deadlines {
                accept_s: 20,
                launch_s: 90,
                result_s: 1800,
                grace_s: 120,
            },
            loadout_check: LoadoutCheck::Ignore,
            decider: Decider::Auto,
            witness: WitnessUse::Ignore,
            witness_from: WitnessFrom::Any,
            stats: StatsCheck::Flag,
            leave: Leave::Void,
        }
    }

    /// Whether every value is known (a decision can be computed).
    pub fn is_known(&self) -> bool {
        self.quorum != Quorum::Unknown
            && self.silence != Silence::Unknown
            && self.disagreement != Disagreement::Unknown
            && self.abort != Abort::Unknown
            && self.disconnect != Disconnect::Unknown
            && self.no_show != NoShow::Unknown
            && self.loadout_check != LoadoutCheck::Unknown
            && self.witness != WitnessUse::Unknown
            && self.witness_from != WitnessFrom::Unknown
            && self.stats != StatsCheck::Unknown
            && self.leave != Leave::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_and_unknown() {
        let p: Policy = serde_json::from_value(serde_json::json!({
            "quorum": 3, "silence": "consent_after_play", "disagreement": "coin_flip",
            "rated_requires_both_teams": true, "abort": "void", "disconnect": "forfeit", "no_show": "void",
            "deadlines": {"accept_s": 20, "launch_s": 90, "result_s": 1800, "grace_s": 120}
        }))
        .unwrap();
        assert_eq!(p.quorum, Quorum::AtLeast(3));
        assert_eq!(p.disagreement, Disagreement::Unknown);
        assert_eq!(
            p.stats,
            StatsCheck::Strict,
            "a declaration without `stats` compares every shared field"
        );
        assert!(!p.is_known());
        assert!(Policy::standard().is_known());
        let back = serde_json::to_value(Policy::standard()).unwrap();
        assert_eq!(back["quorum"], "majority");
        assert_eq!(back["silence"], "consent_after_play");
        assert_eq!(back["stats"], "flag");
        let mut v = back.clone();
        v["stats"] = serde_json::json!("ignore");
        assert_eq!(
            serde_json::from_value::<Policy>(v.clone()).unwrap().stats,
            StatsCheck::Ignore
        );
        v["stats"] = serde_json::json!("x-tolerant");
        assert!(!serde_json::from_value::<Policy>(v).unwrap().is_known());
        // 1.1.11: `leave`, absent from older declarations: void.
        assert_eq!(p.leave, Leave::Void);
        assert_eq!(back["leave"], "void");
        let mut v = back.clone();
        v["leave"] = serde_json::json!("forfeit_unrated");
        assert_eq!(
            serde_json::from_value::<Policy>(v.clone()).unwrap().leave,
            Leave::ForfeitUnrated
        );
        v["leave"] = serde_json::json!("x-later");
        assert!(!serde_json::from_value::<Policy>(v).unwrap().is_known());
    }

    #[test]
    fn quorum_arithmetic() {
        assert_eq!(Quorum::Majority.met(1, 2), Some(false));
        assert_eq!(Quorum::Majority.met(2, 2), Some(true));
        assert_eq!(Quorum::Majority.met(4, 8), Some(false));
        assert_eq!(Quorum::Majority.met(5, 8), Some(true));
        assert_eq!(Quorum::All.met(7, 8), Some(false));
        assert_eq!(Quorum::Any.met(1, 8), Some(true));
        assert_eq!(Quorum::AtLeast(3).met(2, 2), Some(true));
        assert_eq!(Quorum::AtLeast(3).met(2, 8), Some(false));
        assert_eq!(Quorum::Unknown.met(2, 2), None);
    }
}
