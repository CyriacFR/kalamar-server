//! Small helpers: secure randomness, identifiers and codes, token hashing, UTC time formatting and parsing, weekly
//! windows.

use base64::Engine;
use hmac::{KeyInit, Mac};
use kalamar_protocol::contract::{crockford, room_from_bytes};
use sha2::{Digest, Sha256};

/// Bytes from the operating system's secure generator. A failing generator is unrecoverable for a server that
/// issues credentials, so it aborts the request (axum turns a panic in a handler into a 500).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the operating system's random generator failed");
    b
}

/// A uniform index in `0..n` (n > 0).
pub fn random_index(n: usize) -> usize {
    let r = u64::from_le_bytes(random_bytes::<8>());
    (r % n.max(1) as u64) as usize
}

/// An opaque id: `<prefix>_` and 16 lower-case Crockford characters (80 bits).
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", crockford(&random_bytes::<10>(), 16).to_lowercase())
}

/// A relay room / game match id: 26 upper-case Crockford characters (130 bits).
pub fn new_relay_room() -> String {
    room_from_bytes(&random_bytes::<17>())
}

/// A code typed by humans: `len` upper-case Crockford characters.
pub fn new_code(len: usize) -> String {
    crockford(&random_bytes::<16>(), len)
}

/// Friend codes are shown `XXXX-XXXX-XXXX`.
pub fn format_friend_code(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// An opaque token: `<prefix>.` and 43 base64url characters (256 bits).
pub fn new_token(prefix: &str) -> String {
    format!(
        "{prefix}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes::<32>())
    )
}

/// The hash a token is stored and looked up under: SHA-256 over a domain, a zero byte and the token.
pub fn token_hash(token: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"kalamar-token-v1");
    h.update([0u8]);
    h.update(token.as_bytes());
    h.finalize().to_vec()
}

/// A keyed hash (HMAC-SHA-256 under the server secret) of a login identity: the provider's user id never lands in
/// the database in clear.
pub fn identity_hash(secret: &[u8], provider: &str, subject: &str) -> Vec<u8> {
    let mut m = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    m.update(provider.as_bytes());
    m.update(&[0u8]);
    m.update(subject.as_bytes());
    m.finalize().into_bytes().to_vec()
}

pub fn sha256_b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(data))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// An id usable in configuration and paths: 1 to 64 of `A-Z a-z 0-9 _ -`, not a reserved path segment.
pub fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !matches!(s, "current" | "join" | "requests" | "invites" | "tickets" | "me")
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds to RFC 3339 UTC (`2026-11-11T20:15:03Z`).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// RFC 3339 (UTC `Z` or a numeric offset, optional fraction) to Unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse::<i64>().ok() };
    let (y, mo, d, h, mi, se) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &frac[n..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) && &o[3..4] == ":" => {
            let v = o[1..3].parse::<i64>().ok()? * 3600 + o[4..6].parse::<i64>().ok()? * 60;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se - offset)
}

/// A duration like `30s`, `15m`, `12h`, `7d` in seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    (n >= 0).then(|| n.saturating_mul(mult))
}

/// A parsed weekly window: day numbers (0 = Monday) and minutes of the day `[from, to)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub days: Vec<u32>,
    pub from_min: u32,
    pub to_min: u32,
}

pub fn parse_window(w: &crate::config::WindowEntry) -> Option<Window> {
    let day = |d: &str| {
        ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            .iter()
            .position(|x| *x == d)
            .map(|i| i as u32)
    };
    let hm = |t: &str| -> Option<u32> {
        let (h, m) = t.split_once(':')?;
        let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
        (m < 60 && (h < 24 || (h == 24 && m == 0))).then_some(h * 60 + m)
    };
    let days: Option<Vec<u32>> = w.days.iter().map(|d| day(d)).collect();
    let (days, from_min, to_min) = (days?, hm(&w.from)?, hm(&w.to)?);
    (!days.is_empty() && from_min < to_min).then_some(Window { days, from_min, to_min })
}

/// Whether `now` is inside one of `windows` (always true without windows), and the next opening windows as
/// `(start, end)` Unix seconds within a week.
pub fn schedule_state(windows: &[Window], now: i64) -> (bool, Vec<(i64, i64)>) {
    if windows.is_empty() {
        return (true, Vec::new());
    }
    let day0 = now.div_euclid(86_400);
    let mut spans = Vec::new();
    for offset in -1..=7 {
        let day = day0 + offset;
        // 1970-01-01 was a Thursday (3 with Monday = 0).
        let weekday = (day + 3).rem_euclid(7) as u32;
        for w in windows {
            if w.days.contains(&weekday) {
                spans.push((
                    day * 86_400 + i64::from(w.from_min) * 60,
                    day * 86_400 + i64::from(w.to_min) * 60,
                ));
            }
        }
    }
    spans.sort_unstable();
    let open = spans.iter().any(|&(a, b)| a <= now && now < b);
    let upcoming: Vec<(i64, i64)> = spans.into_iter().filter(|&(_, b)| b > now).take(3).collect();
    (open, upcoming)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_round_trip() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_794_346_807), "2026-11-10T21:40:07Z");
        assert_eq!(parse_rfc3339("2026-11-10T21:40:07Z"), Some(1_794_346_807));
        assert_eq!(parse_rfc3339("2026-11-10T22:40:07.250+01:00"), Some(1_794_346_807));
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00Z").map(rfc3339).as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        for bad in [
            "",
            "2026-13-01T00:00:00Z",
            "2026-11-10 22:20:07",
            "2026-11-10T22:20:07",
            "x026-11-10T22:20:07Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        for t in [-86_400 * 400, 951_782_400, 4_102_444_800] {
            assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
        }
    }

    #[test]
    fn ids_tokens_hashes() {
        let a = new_id("u");
        assert!(a.starts_with("u_") && a.len() == 18 && is_id(&a));
        assert_ne!(a, new_id("u"));
        let t = new_token("kat1");
        assert_eq!(t.len(), 5 + 43);
        assert_eq!(token_hash(&t), token_hash(&t));
        assert_ne!(token_hash(&t), token_hash(&new_token("kat1")));
        assert_ne!(
            identity_hash(b"k1", "dev", "alice"),
            identity_hash(b"k2", "dev", "alice")
        );
        assert_ne!(
            identity_hash(b"k1", "dev", "alice"),
            identity_hash(b"k1", "guest", "alice")
        );
        assert_eq!(format_friend_code("ABCDEFGHJKMN"), "ABCD-EFGH-JKMN");
        assert_eq!(unhex(&hex(&[1, 2, 255])), Some(vec![1, 2, 255]));
        assert_eq!(unhex("abc"), None);
        assert!(!is_id("current") && !is_id("") && !is_id("a b"));
        assert_eq!(parse_duration("7d"), Some(604_800));
        assert_eq!(parse_duration("x"), None);
        // PKCE example of RFC 7636, appendix B.
        assert_eq!(
            sha256_b64url(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(random_index(3) < 3);
    }

    #[test]
    fn weekly_windows() {
        let w = crate::config::WindowEntry {
            days: vec!["sat".into(), "sun".into()],
            from: "18:00".into(),
            to: "24:00".into(),
        };
        let win = parse_window(&w).unwrap();
        // 2026-11-14 is a Saturday.
        let sat = parse_rfc3339("2026-11-14T19:00:00Z").unwrap();
        let (open, next) = schedule_state(std::slice::from_ref(&win), sat);
        assert!(open);
        assert_eq!(rfc3339(next[0].0), "2026-11-14T18:00:00Z");
        let wed = parse_rfc3339("2026-11-11T12:00:00Z").unwrap();
        let (open, next) = schedule_state(&[win], wed);
        assert!(!open);
        assert_eq!(rfc3339(next[0].0), "2026-11-14T18:00:00Z");
        let bad = crate::config::WindowEntry {
            days: vec!["xyz".into()],
            from: "18:00".into(),
            to: "17:00".into(),
        };
        assert!(parse_window(&bad).is_none());
        assert!(schedule_state(&[], 0).0);
    }
}
