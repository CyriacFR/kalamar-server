//! The normative, implementation-independent pieces of the Kalamar community protocol 1.0
//! (`docs/protocol/`): the `lan-auto-match` match contract (member names, the rules string, room codes),
//! result validation (checks V1 to V9) and the result decision procedure of `api.md`, "Result decision".
//!
//! The reference service decides results with this crate and the conformance suite uses the same code as its
//! oracle; another implementation may reuse it or follow the documents.

pub mod contract;
pub mod decision;
pub mod policy;

/// The protocol version this crate implements (1.1 only adds to 1.0).
pub const PROTOCOL_VERSION: &str = "1.1";
/// Every version a 1.1 implementation speaks.
pub const PROTOCOL_VERSIONS: &[&str] = &["1.0", "1.1"];
/// The match contract id and version of `match-contract.md`.
pub const CONTRACT_ID: &str = "lan-auto-match";
pub const CONTRACT_VERSION: u32 = 1;

/// A team of the game: `alpha` (the game's team 0, `A` in the rules string) or `bravo` (team 1, `B`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Team {
    Alpha,
    Bravo,
}

impl Team {
    pub fn as_str(self) -> &'static str {
        match self {
            Team::Alpha => "alpha",
            Team::Bravo => "bravo",
        }
    }
    pub fn parse(s: &str) -> Option<Team> {
        match s {
            "alpha" => Some(Team::Alpha),
            "bravo" => Some(Team::Bravo),
            _ => None,
        }
    }
    pub fn letter(self) -> char {
        match self {
            Team::Alpha => 'A',
            Team::Bravo => 'B',
        }
    }
    pub fn other(self) -> Team {
        match self {
            Team::Alpha => Team::Bravo,
            Team::Bravo => Team::Alpha,
        }
    }
}

/// The role of a participant in the game session: `host` creates it, `join` joins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
    Join,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Join => "join",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "host" => Some(Role::Host),
            "join" => Some(Role::Join),
            _ => None,
        }
    }
}
