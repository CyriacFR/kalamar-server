//! Rooms only the holders of a secret can open (`--room-secret-file`): the one implementation of sealed room codes,
//! used by this relay to check them and by the reference service to issue them (`relay.md` 4.2).
//!
//! Without a secret the relay is open: whoever knows its address may open a room under any code. With one, a JOIN is
//! accepted only for a room code sealed with the same secret. The game and its relay protocol are unchanged: the
//! proof is the room code itself.
//!
//! A sealed room code is 32 characters of Crockford base 32, upper case:
//!
//! ```text
//! RRRRRRRRRRRRR DDD TTTTTTTTTTTTTTTT
//! 13 random     day 16 = the first 80 bits of HMAC-SHA-256(secret, "kalamar-room-v1" || R || D)
//! ```
//!
//! `D` is the issuing day (days since the Unix epoch, modulo 32768) on 15 bits: a code is accepted on its day, the
//! day before and the day after (clocks, sessions over midnight), that is from 24 to 48 hours after it was issued.
//!
//! What it guarantees: without the secret a code cannot be made (80-bit tag) and a code opens nothing after its
//! window. What it does not: whoever holds a code (every participant of the match) can open or join that room until
//! the window ends, a code seen on the network can be replayed until then, and whoever holds the secret (every relay
//! that shares it) can make codes for every relay that shares it. One secret per relay limits the last point.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SEALED_LEN: usize = 32;
const RANDOM_LEN: usize = 13;
const DAY_LEN: usize = 3;
const TAG_LEN: usize = 16;
const DAYS: u64 = 1 << 15;
const DOMAIN: &[u8] = b"kalamar-room-v1";
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A room secret held in memory: shared without copying (the service and the relay it runs hold the same value),
/// compared in constant time, and never printed (`Debug` shows its length only).
#[derive(Clone)]
pub struct RoomSecret(Arc<[u8]>);

impl RoomSecret {
    pub fn new(bytes: Vec<u8>) -> RoomSecret {
        RoomSecret(bytes.into())
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
    /// The very same value in memory (not only an equal one).
    pub fn is_same_allocation(&self, other: &RoomSecret) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Reads a secret file (hexadecimal, at least 32 digits). Errors name the file, never its content.
    pub fn from_file(path: &Path) -> Result<RoomSecret, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        parse_secret(&text)
            .map(RoomSecret::new)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl std::fmt::Debug for RoomSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoomSecret(<{} bytes>)", self.0.len())
    }
}

impl PartialEq for RoomSecret {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && bool::from(self.0.ct_eq(&other.0))
    }
}
impl Eq for RoomSecret {}

/// `len` characters of Crockford base 32 from the leading bits of `bytes`.
fn base32(bytes: &[u8], len: usize) -> String {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, String::with_capacity(len));
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 && out.len() < len {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// The day of `unix` seconds as the three characters of a code.
pub fn day_code(unix: u64) -> String {
    let d = (unix / 86_400) % DAYS;
    [(d >> 10) & 31, (d >> 5) & 31, d & 31]
        .iter()
        .map(|&v| ALPHABET[v as usize] as char)
        .collect()
}

fn day_of(code: &[u8]) -> Option<u64> {
    code.iter().try_fold(0u64, |acc, c| {
        ALPHABET.iter().position(|a| a == c).map(|v| (acc << 5) | v as u64)
    })
}

fn tag(secret: &[u8], head: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    m.update(DOMAIN);
    m.update(head);
    base32(&m.finalize().into_bytes(), TAG_LEN)
}

/// The sealed code of `random` (its first 13 characters are used) issued at `unix`.
pub fn seal(secret: &[u8], random: &str, unix: u64) -> Option<String> {
    let r = random.get(..RANDOM_LEN)?;
    if !r.bytes().all(|c| ALPHABET.contains(&c)) {
        return None;
    }
    let head = format!("{r}{}", day_code(unix));
    Some(format!("{head}{}", tag(secret, head.as_bytes())))
}

/// Whether `room` was issued with `secret`, on the day of `unix` or a neighboring one.
pub fn verify(secret: &[u8], room: &[u8], unix: u64) -> bool {
    if room.len() != SEALED_LEN {
        return false;
    }
    let (head, given) = room.split_at(RANDOM_LEN + DAY_LEN);
    if !bool::from(tag(secret, head).as_bytes().ct_eq(given)) {
        return false;
    }
    let Some(day) = day_of(&head[RANDOM_LEN..]) else {
        return false;
    };
    let today = (unix / 86_400) % DAYS;
    [today, (today + 1) % DAYS, (today + DAYS - 1) % DAYS].contains(&day)
}

/// The code a relay bound by `secret` opens for the room `room` at `unix`: `room` itself when it is a sealed code
/// still accepted (the rounds of one game session keep their room), else `room`'s first 13 characters sealed now.
/// `None` when `room` does not start with 13 characters of the alphabet.
pub fn seal_room(secret: &RoomSecret, room: &str, unix: u64) -> Option<String> {
    if verify(secret.bytes(), room.as_bytes(), unix) {
        return Some(room.to_string());
    }
    seal(secret.bytes(), room, unix)
}

/// A secret from its file: hexadecimal, at least 32 digits (16 bytes); `openssl rand -hex 32`, or
/// `kalamar-relay --new-room-secret <file>`.
pub fn parse_secret(text: &str) -> Result<Vec<u8>, String> {
    let t = text.trim();
    if t.len() < 32 || t.len() % 2 != 0 || !t.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("a room secret is hexadecimal, at least 32 digits".into());
    }
    Ok((0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).unwrap_or(0))
        .collect())
}

/// 32 random bytes as 64 hexadecimal digits.
pub fn new_secret_hex() -> String {
    let mut s = [0u8; 32];
    rand::fill(&mut s[..]);
    s.iter().map(|b| format!("{b:02x}")).collect()
}

/// Creates `path` for a secret: it must not exist yet (no file is ever replaced), and on Unix it is created with
/// mode 0600 in the same call, so that no other account can open it even for an instant. Windows gives the file the
/// permissions of its folder: keep secrets in a folder only the service's account can read.
pub fn create_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    let written = f.write_all(contents).and_then(|()| f.sync_all());
    if written.is_err() {
        drop(f);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Writes a new random secret to `path` (see [`create_private_file`]) and returns it.
pub fn create_secret_file(path: &Path) -> std::io::Result<RoomSecret> {
    let hex = new_secret_hex();
    create_private_file(path, format!("{hex}\n").as_bytes())?;
    Ok(RoomSecret::new(parse_secret(&hex).expect("a fresh secret is valid")))
}

/// What the logs show of a room code: 8 hexadecimal digits of its SHA-256, enough to follow one room through the
/// log lines, useless to join it (`relay.md` section 3: codes are never logged).
pub fn room_tag(room: &[u8]) -> String {
    Sha256::digest(room)[..4].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    /// 2026-11-12T00:00:00Z.
    const DAY: u64 = 1_794_441_600;

    #[test]
    fn the_shared_test_vector() {
        // relay.md 4.2 gives the same values.
        let room = seal(SECRET, "K7Q2MXA9P84DZB3W8R6TNY5HCF", DAY).unwrap();
        assert_eq!(room.len(), SEALED_LEN);
        assert_eq!(&room[..13], "K7Q2MXA9P84DZ");
        assert_eq!(&room[13..16], day_code(DAY));
        assert_eq!(room, VECTOR);
        assert!(verify(SECRET, room.as_bytes(), DAY));
    }
    const VECTOR: &str = "K7Q2MXA9P84DZM91NYAXD5NJAJ56RNH3";

    #[test]
    fn only_the_secrets_codes_of_these_days_open_a_room() {
        let room = seal(SECRET, "K7Q2MXA9P84DZB3W8R6TNY5HCF", DAY + 3600).unwrap();
        assert!(verify(SECRET, room.as_bytes(), DAY + 86_400 + 100));
        assert!(verify(SECRET, room.as_bytes(), DAY - 100));
        assert!(
            !verify(SECRET, room.as_bytes(), DAY + 2 * 86_400),
            "two days later it opens nothing"
        );
        assert!(!verify(SECRET, room.as_bytes(), DAY - 86_400 - 100));
        assert!(!verify(b"another secret, as long as it is", room.as_bytes(), DAY));
        // Any change of the code, a plain random code, a short one.
        let mut other = room.clone().into_bytes();
        other[0] = if other[0] == b'0' { b'1' } else { b'0' };
        assert!(!verify(SECRET, &other, DAY));
        let mut other = room.clone().into_bytes();
        other[14] = if other[14] == b'0' { b'1' } else { b'0' };
        assert!(!verify(SECRET, &other, DAY), "the day is signed too");
        assert!(!verify(SECRET, b"K7Q2MXA9P84DZB3W8R6TNY5HCF", DAY));
        assert!(!verify(SECRET, b"FRIENDS-ROOM", DAY));
        assert!(seal(SECRET, "short", DAY).is_none() && seal(SECRET, "lowercase-is-not-ok", DAY).is_none());
        // The day wraps every 32768 days.
        let late = (DAYS - 1) * 86_400;
        let room = seal(SECRET, "K7Q2MXA9P84DZ", late).unwrap();
        assert!(verify(SECRET, room.as_bytes(), late + 86_400));
    }

    #[test]
    fn a_session_keeps_its_sealed_room_while_it_is_accepted() {
        let s = RoomSecret::new(SECRET.to_vec());
        let room = seal_room(&s, "K7Q2MXA9P84DZB3W8R6TNY5HCF", DAY).unwrap();
        assert_eq!(room, VECTOR);
        assert_eq!(seal_room(&s, &room, DAY + 86_400).unwrap(), room, "kept the next day");
        let later = seal_room(&s, &room, DAY + 2 * 86_400).unwrap();
        assert_ne!(later, room, "sealed again once it expired");
        assert_eq!(&later[..13], &room[..13]);
        assert!(seal_room(&s, "short", DAY).is_none());
    }

    #[test]
    fn secrets_are_hexadecimal_and_never_printed() {
        assert_eq!(parse_secret(&"ab".repeat(16)).unwrap(), vec![0xab; 16]);
        assert_eq!(parse_secret(&format!("  {}\n", new_secret_hex())).unwrap().len(), 32);
        assert!(parse_secret("abcd").is_err() && parse_secret(&"zz".repeat(16)).is_err());
        assert!(parse_secret(&"a".repeat(33)).is_err());
        let s = RoomSecret::new(vec![0xab; 16]);
        assert_eq!(format!("{s:?}"), "RoomSecret(<16 bytes>)");
        assert_eq!(s, RoomSecret::new(vec![0xab; 16]));
        assert_ne!(s, RoomSecret::new(vec![0xab; 17]));
        assert!(s.is_same_allocation(&s.clone()));
        assert!(!s.is_same_allocation(&RoomSecret::new(vec![0xab; 16])));
    }

    #[test]
    fn a_secret_file_is_created_once_and_private() {
        let dir = std::env::temp_dir().join(format!("kalamar-roomauth-{}", new_secret_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("relay.key");
        let s = create_secret_file(&f).unwrap();
        assert_eq!(RoomSecret::from_file(&f).unwrap(), s);
        // Never replaced.
        let again = create_secret_file(&f).unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(RoomSecret::from_file(&f).unwrap(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "created private");
        }
        let err = RoomSecret::from_file(&dir.join("missing.key")).unwrap_err();
        assert!(err.contains("missing.key"), "{err}");
        std::fs::write(dir.join("bad.key"), "secret words").unwrap();
        let err = RoomSecret::from_file(&dir.join("bad.key")).unwrap_err();
        assert!(err.contains("bad.key") && !err.contains("secret words"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn logs_show_a_tag_not_the_code() {
        let t = room_tag(VECTOR.as_bytes());
        assert_eq!(t.len(), 8);
        assert!(t.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(!VECTOR.to_ascii_lowercase().contains(&t));
        assert_eq!(t, room_tag(VECTOR.as_bytes()));
        assert_ne!(t, room_tag(b"FRIENDS-ROOM"));
    }
}
