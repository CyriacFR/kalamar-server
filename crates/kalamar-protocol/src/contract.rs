//! The `lan-auto-match` contract, version 1 (`match-contract.md`): member names, the rules string, relay rooms
//! and game match ids.

use crate::Team;

/// Most Unicode scalar values in a member name (the game shows nicknames of at most 10 characters).
pub const MEMBER_NAME_MAX_CHARS: usize = 10;
/// Most UTF-8 bytes in a member name (the relay carries names of at most 32 bytes).
pub const MEMBER_NAME_MAX_BYTES: usize = 32;
/// Fewest and most players of one match of the supported game build.
pub const MIN_PLAYERS: usize = 2;
pub const MAX_PLAYERS: usize = 8;
/// 1.1.7 (`spectators.md`): most spectators of one match (the room's two places in the stands) and most members of
/// one game session, players and spectators together (`players=` counts them all).
pub const MAX_SPECTATORS: usize = 2;
pub const SESSION_MAX: usize = MAX_PLAYERS + MAX_SPECTATORS;
/// Crockford base 32 (no I, L, O, U): codes typed or read by humans and relay rooms.
pub const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Length of a relay room / game match id made by [`room_from_bytes`]: 26 characters, 130 bits.
pub const ROOM_LEN: usize = 26;

fn forbidden(c: char) -> bool {
    c.is_control() || matches!(c, ',' | ':' | '=' | '"')
}

/// A valid member name: 1 to 10 scalar values, at most 32 UTF-8 bytes, no control character, none of `,:="`, no
/// leading or trailing whitespace.
pub fn is_valid_member_name(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MEMBER_NAME_MAX_CHARS).contains(&n)
        && s.len() <= MEMBER_NAME_MAX_BYTES
        && !s.chars().any(forbidden)
        && s.trim() == s
}

/// Cuts `s` to at most `max_chars` scalar values and [`MEMBER_NAME_MAX_BYTES`] bytes.
fn cut(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for c in s.chars().take(max_chars) {
        if out.len() + c.len_utf8() > MEMBER_NAME_MAX_BYTES {
            break;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// A member name made from a display name: forbidden characters dropped, whitespace runs collapsed, cut to the
/// limits; `Player` when nothing is left.
pub fn member_name_base(display_name: &str) -> String {
    let cleaned: String = display_name
        .chars()
        .filter(|c| !forbidden(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let s = cut(&cleaned, MEMBER_NAME_MAX_CHARS);
    if s.is_empty() { "Player".to_string() } else { s }
}

/// Member names for the participants of one match, in order: unique case-insensitively, a digit (2, 3, ...)
/// appended on a collision (`Camille`, `Camille2`).
pub fn derive_member_names(display_names: &[&str]) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(display_names.len());
    for name in display_names {
        let base = member_name_base(name);
        let mut candidate = base.clone();
        let mut k = 2u32;
        while used.iter().any(|u| u.to_lowercase() == candidate.to_lowercase()) {
            let suffix = k.to_string();
            let room = MEMBER_NAME_MAX_CHARS - suffix.len();
            candidate = format!("{}{}", cut(&base, room), suffix);
            k += 1;
        }
        used.push(candidate.clone());
        out.push(candidate);
    }
    out
}

/// `players=<n>,teams=<name>:<A|B>,...,mode=<mode>` (match-contract.md, "The rules string").
pub fn build_rules(players: &[(&str, Team)], mode: &str) -> String {
    build_rules_seated(players, &[], mode)
}

/// 1.1.7 (`spectators.md`): the rules string with spectators: `players=` counts every member of the game session,
/// the spectators follow the players in `teams=` with the letter `S`.
/// `players=<n+s>,teams=<name>:<A|B>,...,<spectator>:S,...,mode=<mode>`.
pub fn build_rules_seated(players: &[(&str, Team)], spectators: &[&str], mode: &str) -> String {
    let teams: Vec<String> = players
        .iter()
        .map(|(n, t)| format!("{n}:{}", t.letter()))
        .chain(spectators.iter().map(|n| format!("{n}:S")))
        .collect();
    format!(
        "players={},teams={},mode={}",
        players.len() + spectators.len(),
        teams.join(","),
        mode
    )
}

/// What [`parse_rules`] reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRules {
    /// Every member of the game session, spectators included (1.1.7).
    pub players: usize,
    pub teams: Vec<(String, Team)>,
    /// 1.1.7: the members seated in the stands (`<name>:S`), in order.
    pub spectators: Vec<String>,
    pub mode: String,
    /// Keys the contract does not define, in order (`key`, `value`).
    pub other: Vec<(String, String)>,
}

/// One entry of `teams=`: a player of a team, or (1.1.7) a spectator.
enum Entry {
    Player(String, Team),
    Spectator(String),
}

/// Parses a rules string: comma-separated items; `key=value` items set a key, `name:A|B|S` items after `teams=`
/// continue the team list. `None` when it is malformed.
pub fn parse_rules(s: &str) -> Option<ParsedRules> {
    let mut players = None;
    let mut teams: Vec<(String, Team)> = Vec::new();
    let mut spectators: Vec<String> = Vec::new();
    let mut mode = None;
    let mut other = Vec::new();
    let mut in_teams = false;
    let entry_of = |entry: &str| -> Option<Entry> {
        let (name, letter) = entry.rsplit_once(':')?;
        if !is_valid_member_name(name) {
            return None;
        }
        Some(match letter {
            "A" => Entry::Player(name.to_string(), Team::Alpha),
            "B" => Entry::Player(name.to_string(), Team::Bravo),
            "S" => Entry::Spectator(name.to_string()),
            _ => return None,
        })
    };
    let mut push = |e: Entry| match e {
        Entry::Player(n, t) => teams.push((n, t)),
        Entry::Spectator(n) => spectators.push(n),
    };
    for item in s.split(',') {
        if let Some((k, v)) = item.split_once('=') {
            in_teams = false;
            match k {
                "players" => players = Some(v.parse::<usize>().ok()?),
                "teams" => {
                    push(entry_of(v)?);
                    in_teams = true;
                }
                "mode" => mode = Some(v.to_string()),
                _ => other.push((k.to_string(), v.to_string())),
            }
        } else if in_teams {
            push(entry_of(item)?);
        } else {
            return None;
        }
    }
    Some(ParsedRules {
        players: players?,
        teams,
        spectators,
        mode: mode?,
        other,
    })
}

/// A relay room code of relay protocol v1: 1 to 32 characters from `A-Z 0-9 - _`.
pub fn is_valid_relay_room(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A game match id of contract version 1: 1 to 32 characters from `A-Z 0-9`.
pub fn is_valid_game_match_id(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Encodes random bytes as Crockford base 32 characters (5 bits each), `len` characters.
pub fn crockford(bytes: &[u8], len: usize) -> String {
    let mut out = String::with_capacity(len);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut it = bytes.iter();
    while out.len() < len {
        if bits < 5 {
            match it.next() {
                Some(b) => {
                    acc = (acc << 8) | u32::from(*b);
                    bits += 8;
                }
                None => break,
            }
        }
        bits -= 5;
        out.push(CROCKFORD[((acc >> bits) & 31) as usize] as char);
    }
    out
}

/// A relay room (= game match id in contract version 1) from at least 17 random bytes: 26 characters, 130 bits.
pub fn room_from_bytes(bytes: &[u8; 17]) -> String {
    crockford(bytes, ROOM_LEN)
}

/// Normalizes a human-typed code (room code, friend code): upper case, dashes and spaces removed, and the
/// look-alikes of Crockford base 32 mapped (`O` to `0`, `I` and `L` to `1`).
pub fn normalize_code(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            u => u,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names() {
        assert!(is_valid_member_name("Camille"));
        assert!(is_valid_member_name("Éloïse"));
        assert!(!is_valid_member_name(""));
        assert!(!is_valid_member_name("a,b"));
        assert!(!is_valid_member_name(" lead"));
        assert!(!is_valid_member_name("abcdefghijk"));
        assert!(!is_valid_member_name("tab\there"));
        assert_eq!(member_name_base("  Ca,mi:lle  =  the  great "), "Camille th");
        assert_eq!(member_name_base(",,,"), "Player");
        let names = derive_member_names(&["Camille", "camille", "CAMILLE", "Lou", "abcdefghij", "abcdefghij"]);
        assert_eq!(
            names,
            ["Camille", "camille2", "CAMILLE3", "Lou", "abcdefghij", "abcdefghi2"]
        );
        assert!(names.iter().all(|n| is_valid_member_name(n)));
        // Ten copies of a long name stay within the limits.
        let many = derive_member_names(&["abcdefghij"; 12]);
        assert!(many.iter().all(|n| is_valid_member_name(n)));
        assert_eq!(many[11], "abcdefgh12");
        // Multibyte names respect the byte limit.
        let wide = member_name_base("😀😀😀😀😀😀😀😀😀😀");
        assert!(wide.len() <= 32 && is_valid_member_name(&wide));
    }

    #[test]
    fn rules_round_trip() {
        let s = build_rules(&[("Alpha", Team::Bravo), ("Bravo", Team::Alpha)], "turf");
        assert_eq!(s, "players=2,teams=Alpha:B,Bravo:A,mode=turf");
        let p = parse_rules(&s).unwrap();
        assert_eq!(p.players, 2);
        assert_eq!(
            p.teams,
            vec![("Alpha".into(), Team::Bravo), ("Bravo".into(), Team::Alpha)]
        );
        assert_eq!(p.mode, "turf");
        assert!(parse_rules("players=2,stray,mode=turf").is_none());
        assert!(parse_rules("players=x,teams=A:A,mode=turf").is_none());
        assert!(parse_rules("teams=A:C,mode=turf,players=2").is_none());
        assert_eq!(
            parse_rules("players=2,teams=A:A,B:B,mode=turf,stage=s1").unwrap().other,
            vec![("stage".to_string(), "s1".to_string())]
        );
    }

    #[test]
    fn spectators_follow_the_players_with_the_letter_s() {
        let s = build_rules_seated(&[("Ana", Team::Alpha), ("Bob", Team::Bravo)], &["Cleo"], "turf");
        assert_eq!(s, "players=3,teams=Ana:A,Bob:B,Cleo:S,mode=turf");
        let p = parse_rules(&format!("{s},spectators_wait=60")).unwrap();
        assert_eq!(
            (p.players, p.teams.len(), p.spectators.clone()),
            (3, 2, vec!["Cleo".to_string()])
        );
        assert_eq!(p.other, vec![("spectators_wait".to_string(), "60".to_string())]);
        // Without spectators the string is the contract 1 one.
        assert_eq!(
            build_rules_seated(&[("A", Team::Alpha), ("B", Team::Bravo)], &[], "turf"),
            build_rules(&[("A", Team::Alpha), ("B", Team::Bravo)], "turf")
        );
        assert!(parse_rules("players=3,teams=A:A,B:B,C:X,mode=turf").is_none());
    }

    #[test]
    fn codes() {
        let room = room_from_bytes(&[0xff; 17]);
        assert_eq!(room.len(), 26);
        assert!(is_valid_relay_room(&room) && is_valid_game_match_id(&room));
        assert_eq!(crockford(&[0, 0], 3), "000");
        assert_eq!(crockford(&[0b1111_1000], 1), "Z");
        assert!(!is_valid_relay_room("abc"));
        assert!(!is_valid_game_match_id("AB-C"));
        assert_eq!(normalize_code("7kq2-mxo i"), "7KQ2MX01");
    }
}
