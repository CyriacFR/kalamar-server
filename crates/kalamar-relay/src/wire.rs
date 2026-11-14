//! Wire format of the LAN relay protocol, version 1 (`lan-relay-protocol.md`).
//!
//! Parsing is strict and total: every function takes untrusted bytes and returns a value or an error, never
//! panics, and allocates nothing (results borrow the datagram). Encoders append to a caller-owned `Vec<u8>`.

/// ASCII `S3LR`.
pub const MAGIC: [u8; 4] = *b"S3LR";
pub const VERSION: u8 = 1;
/// Largest UDP payload either side sends or accepts.
pub const MAX_DATAGRAM: usize = 1472;
pub const DATA_HEADER: usize = 32;
pub const MAX_PAYLOAD: usize = MAX_DATAGRAM - DATA_HEADER;
pub const SESSION_HEADER: usize = 20;
pub const HELLO_LEN: usize = 64;
pub const JOIN_MIN_LEN: usize = 64;
pub const MAX_ROOM_LEN: usize = 32;
pub const MAX_NAME_LEN: usize = 32;
pub const COOKIE_LEN: usize = 16;
pub const TOKEN_LEN: usize = 8;

/// First address of the virtual subnet 10.13.0.0/16 and its netmask.
pub const VNET: u32 = 0x0A0D_0000;
pub const VNET_MASK: u32 = 0xFFFF_0000;
/// Broadcast address 10.13.255.255; 255.255.255.255 is accepted as the same.
pub const VBCAST: u32 = 0x0A0D_FFFF;
pub const LIMITED_BCAST: u32 = 0xFFFF_FFFF;

/// Message types (section 3).
pub mod ty {
    pub const HELLO: u8 = 0x01;
    pub const CHALLENGE: u8 = 0x02;
    pub const JOIN: u8 = 0x03;
    pub const JOIN_ACK: u8 = 0x04;
    pub const JOIN_NAK: u8 = 0x05;
    pub const KEEPALIVE: u8 = 0x06;
    pub const KEEPALIVE_ACK: u8 = 0x07;
    pub const LEAVE: u8 = 0x08;
    pub const CLOSED: u8 = 0x09;
    pub const DATA: u8 = 0x10;
    pub const MEMBER: u8 = 0x11;
    /// Between a relay and the service bound to it, never a game (protocol 1.1.13, `status.rs`): the service asks
    /// for the relay's load, the relay answers. An open relay ignores both, as before.
    pub const STATUS_QUERY: u8 = 0x20;
    pub const STATUS: u8 = 0x21;
}

/// Size of a status query and of its answer (`status.rs`).
pub const STATUS_LEN: usize = 128;

/// JOIN_NAK reasons (section 3.5).
pub mod nak {
    pub const VERSION: u8 = 1;
    pub const COOKIE: u8 = 2;
    pub const ROOM: u8 = 3;
    pub const ROOM_FULL: u8 = 4;
    pub const RELAY_FULL: u8 = 5;
    pub const NAME: u8 = 6;
    pub const RATE: u8 = 7;
    pub const MALFORMED: u8 = 8;

    /// Byte 17 of a JOIN_NAK (the first reserved byte; protocol 1.1.13): why a `ROOM_FULL` or `RELAY_FULL` came,
    /// for a client that shows it. A client that does not know it ignores it, as every reserved byte.
    pub mod detail {
        /// Not said (and every other reason).
        pub const NONE: u8 = 0;
        /// The limit counts the source address: too many members of this room (`ROOM_FULL`) or sessions of this
        /// relay (`RELAY_FULL`) from one IPv4 address or IPv6 /64. Without it, the room or the relay is full.
        pub const SOURCE: u8 = 1;
    }
}

/// CLOSED reasons (section 3.9).
pub mod closed {
    pub const IDLE: u8 = 1;
    pub const UNKNOWN_SESSION: u8 = 2;
    pub const OPERATOR: u8 = 3;
    pub const SHUTDOWN: u8 = 4;
    pub const VIOLATION: u8 = 5;
}

/// MEMBER events (section 3.11).
pub const MEMBER_JOINED: u8 = 1;
pub const MEMBER_LEFT: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Shorter than the minimum for its type (or shorter than the 8-byte header).
    TooShort,
    /// Longer than 1472 bytes.
    TooLarge,
    BadMagic,
    /// Valid magic, a version other than 1. Carries the type byte so a relay can answer a HELLO.
    OtherVersion {
        ty: u8,
    },
    UnknownType(u8),
}

/// Session id and token (bytes 8-19 of every message from KEEPALIVE on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Auth {
    pub session: u32,
    pub token: [u8; TOKEN_LEN],
}

/// A JOIN with its fixed fields decoded; room and name are validated by [`JoinRaw::room_and_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinRaw<'a> {
    pub nonce: u64,
    pub cookie: [u8; COOKIE_LEN],
    pub vip: u32,
    pub room_len: u8,
    pub name_len: u8,
    /// The datagram from byte 40 on (room, name, padding).
    pub body: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// NAK reason 8.
    Malformed,
    /// NAK reason 3.
    Room,
    /// NAK reason 6.
    Name,
}

impl JoinError {
    pub fn reason(self) -> u8 {
        match self {
            JoinError::Malformed => nak::MALFORMED,
            JoinError::Room => nak::ROOM,
            JoinError::Name => nak::NAME,
        }
    }
}

pub fn is_room_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-' || c == b'_'
}

pub fn is_name_byte(c: u8) -> bool {
    c >= 0x20 && c != 0x7F
}

impl<'a> JoinRaw<'a> {
    /// Checks lengths, then room characters, then name bytes, in the order of section 5.1.
    pub fn room_and_name(&self) -> Result<(&'a [u8], &'a [u8]), JoinError> {
        let (r, n) = (self.room_len as usize, self.name_len as usize);
        if !(1..=MAX_ROOM_LEN).contains(&r) || n > MAX_NAME_LEN || r + n > self.body.len() {
            return Err(JoinError::Malformed);
        }
        let room = &self.body[..r];
        let name = &self.body[r..r + n];
        if !room.iter().copied().all(is_room_char) {
            return Err(JoinError::Room);
        }
        if !name.iter().copied().all(is_name_byte) {
            return Err(JoinError::Name);
        }
        Ok((room, name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinAck {
    pub nonce: u64,
    pub session: u32,
    pub token: [u8; TOKEN_LEN],
    pub vip: u32,
    pub prefix: u8,
    pub max_members: u8,
    pub keepalive_s: u8,
    pub idle_s: u8,
    pub max_payload: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    pub src_vip: u32,
    pub src_port: u16,
    pub dst_vip: u32,
    pub dst_port: u16,
}

/// A parsed datagram. Both directions are decoded so that simulated clients can use the same code; the relay
/// itself rejects the relay-to-client types as "wrong way".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg<'a> {
    Hello {
        nonce: u64,
    },
    Challenge {
        nonce: u64,
        cookie: [u8; COOKIE_LEN],
    },
    Join(JoinRaw<'a>),
    JoinAck(JoinAck),
    JoinNak {
        version: u8,
        nonce: u64,
        reason: u8,
        /// [`nak::detail`].
        detail: u8,
    },
    Keepalive {
        auth: Auth,
        seq: u32,
    },
    KeepaliveAck {
        auth: Auth,
        seq: u32,
        members: u8,
    },
    Leave {
        auth: Auth,
    },
    Closed {
        auth: Auth,
        reason: u8,
    },
    Data {
        auth: Auth,
        hdr: DataHeader,
        payload: &'a [u8],
    },
    Member {
        auth: Auth,
        event: u8,
        vip: u32,
        name: &'a [u8],
    },
    /// A status query; its tag is checked by `status.rs`.
    StatusQuery,
    /// A status answer; its tag is checked by `status.rs`.
    Status,
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}
fn arr<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(&b[o..o + N]);
    a
}
fn auth(b: &[u8]) -> Auth {
    Auth {
        session: be32(b, 8),
        token: arr(b, 12),
    }
}

/// Decodes the common header: `(version, type)`. Reserved bytes are ignored.
pub fn peek_header(b: &[u8]) -> Result<(u8, u8), ParseError> {
    if b.len() > MAX_DATAGRAM {
        return Err(ParseError::TooLarge);
    }
    if b.len() < 8 {
        return Err(ParseError::TooShort);
    }
    if b[..4] != MAGIC {
        return Err(ParseError::BadMagic);
    }
    Ok((b[4], b[5]))
}

/// Strict parse of one datagram (sections 2 and 3).
pub fn parse(b: &[u8]) -> Result<Msg<'_>, ParseError> {
    let (version, t) = peek_header(b)?;
    if version != VERSION {
        // A JOIN_NAK carries the relay's own version, so a client can still decode it (section 6.1).
        if t == ty::JOIN_NAK && b.len() >= 24 {
            return Ok(Msg::JoinNak {
                version,
                nonce: be64(b, 8),
                reason: b[16],
                detail: b[17],
            });
        }
        return Err(ParseError::OtherVersion { ty: t });
    }
    let need = |n: usize| {
        if b.len() >= n {
            Ok(())
        } else {
            Err(ParseError::TooShort)
        }
    };
    Ok(match t {
        ty::HELLO => {
            need(HELLO_LEN)?;
            Msg::Hello { nonce: be64(b, 8) }
        }
        ty::CHALLENGE => {
            need(32)?;
            Msg::Challenge {
                nonce: be64(b, 8),
                cookie: arr(b, 16),
            }
        }
        ty::JOIN => {
            need(JOIN_MIN_LEN)?;
            Msg::Join(JoinRaw {
                nonce: be64(b, 8),
                cookie: arr(b, 16),
                vip: be32(b, 32),
                room_len: b[36],
                name_len: b[37],
                body: &b[40..],
            })
        }
        ty::JOIN_ACK => {
            need(40)?;
            Msg::JoinAck(JoinAck {
                nonce: be64(b, 8),
                session: be32(b, 16),
                token: arr(b, 20),
                vip: be32(b, 28),
                prefix: b[32],
                max_members: b[33],
                keepalive_s: b[34],
                idle_s: b[35],
                max_payload: be16(b, 36),
            })
        }
        ty::JOIN_NAK => {
            need(24)?;
            Msg::JoinNak {
                version,
                nonce: be64(b, 8),
                reason: b[16],
                detail: b[17],
            }
        }
        ty::KEEPALIVE => {
            need(28)?;
            Msg::Keepalive {
                auth: auth(b),
                seq: be32(b, 20),
            }
        }
        ty::KEEPALIVE_ACK => {
            need(28)?;
            Msg::KeepaliveAck {
                auth: auth(b),
                seq: be32(b, 20),
                members: b[24],
            }
        }
        ty::LEAVE => {
            need(SESSION_HEADER)?;
            Msg::Leave { auth: auth(b) }
        }
        ty::CLOSED => {
            need(24)?;
            Msg::Closed {
                auth: auth(b),
                reason: b[20],
            }
        }
        ty::DATA => {
            need(DATA_HEADER)?;
            Msg::Data {
                auth: auth(b),
                hdr: DataHeader {
                    src_vip: be32(b, 20),
                    src_port: be16(b, 24),
                    dst_vip: be32(b, 26),
                    dst_port: be16(b, 30),
                },
                payload: &b[DATA_HEADER..],
            }
        }
        ty::MEMBER => {
            need(28)?;
            let n = b[21] as usize;
            if n > MAX_NAME_LEN || b.len() < 28 + n {
                return Err(ParseError::TooShort);
            }
            Msg::Member {
                auth: auth(b),
                event: b[20],
                vip: be32(b, 24),
                name: &b[28..28 + n],
            }
        }
        ty::STATUS_QUERY => {
            need(STATUS_LEN)?;
            Msg::StatusQuery
        }
        ty::STATUS => {
            need(STATUS_LEN)?;
            Msg::Status
        }
        other => return Err(ParseError::UnknownType(other)),
    })
}

// ---- encoders -------------------------------------------------------------------------------------------------

pub fn put_header_v(out: &mut Vec<u8>, version: u8, t: u8) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[version, t, 0, 0]);
}

pub fn put_header(out: &mut Vec<u8>, t: u8) {
    put_header_v(out, VERSION, t);
}

fn put_session(out: &mut Vec<u8>, t: u8, a: &Auth) {
    put_header(out, t);
    out.extend_from_slice(&a.session.to_be_bytes());
    out.extend_from_slice(&a.token);
}

pub fn encode_hello(out: &mut Vec<u8>, nonce: u64) {
    encode_hello_v(out, VERSION, nonce);
}

pub fn encode_hello_v(out: &mut Vec<u8>, version: u8, nonce: u64) {
    put_header_v(out, version, ty::HELLO);
    out.extend_from_slice(&nonce.to_be_bytes());
    out.resize(out.len() + 48, 0);
}

pub fn encode_challenge(out: &mut Vec<u8>, nonce: u64, cookie: &[u8; COOKIE_LEN]) {
    put_header(out, ty::CHALLENGE);
    out.extend_from_slice(&nonce.to_be_bytes());
    out.extend_from_slice(cookie);
}

/// Encodes a JOIN, zero-padded to 64 bytes. Lengths are the caller's business (tests build bad ones on purpose).
pub fn encode_join(out: &mut Vec<u8>, nonce: u64, cookie: &[u8; COOKIE_LEN], vip: u32, room: &[u8], name: &[u8]) {
    let start = out.len();
    put_header(out, ty::JOIN);
    out.extend_from_slice(&nonce.to_be_bytes());
    out.extend_from_slice(cookie);
    out.extend_from_slice(&vip.to_be_bytes());
    out.extend_from_slice(&[room.len() as u8, name.len() as u8, 0, 0]);
    out.extend_from_slice(room);
    out.extend_from_slice(name);
    if out.len() - start < JOIN_MIN_LEN {
        out.resize(start + JOIN_MIN_LEN, 0);
    }
}

pub fn encode_join_ack(out: &mut Vec<u8>, a: &JoinAck) {
    put_header(out, ty::JOIN_ACK);
    out.extend_from_slice(&a.nonce.to_be_bytes());
    out.extend_from_slice(&a.session.to_be_bytes());
    out.extend_from_slice(&a.token);
    out.extend_from_slice(&a.vip.to_be_bytes());
    out.extend_from_slice(&[a.prefix, a.max_members, a.keepalive_s, a.idle_s]);
    out.extend_from_slice(&a.max_payload.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
}

/// A JOIN_NAK carries the relay's own version (section 3.5), and in byte 17 the [`nak::detail`] of the refusal.
pub fn encode_join_nak(out: &mut Vec<u8>, nonce: u64, reason: u8, detail: u8) {
    put_header(out, ty::JOIN_NAK);
    out.extend_from_slice(&nonce.to_be_bytes());
    out.push(reason);
    out.push(detail);
    out.extend_from_slice(&[0; 6]);
}

pub fn encode_keepalive(out: &mut Vec<u8>, a: &Auth, seq: u32) {
    put_session(out, ty::KEEPALIVE, a);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
}

pub fn encode_keepalive_ack(out: &mut Vec<u8>, a: &Auth, seq: u32, members: u8) {
    put_session(out, ty::KEEPALIVE_ACK, a);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&[members, 0, 0, 0]);
}

pub fn encode_leave(out: &mut Vec<u8>, a: &Auth) {
    put_session(out, ty::LEAVE, a);
}

pub fn encode_closed(out: &mut Vec<u8>, a: &Auth, reason: u8) {
    put_session(out, ty::CLOSED, a);
    out.extend_from_slice(&[reason, 0, 0, 0]);
}

pub fn encode_data(out: &mut Vec<u8>, a: &Auth, h: &DataHeader, payload: &[u8]) {
    put_session(out, ty::DATA, a);
    out.extend_from_slice(&h.src_vip.to_be_bytes());
    out.extend_from_slice(&h.src_port.to_be_bytes());
    out.extend_from_slice(&h.dst_vip.to_be_bytes());
    out.extend_from_slice(&h.dst_port.to_be_bytes());
    out.extend_from_slice(payload);
}

/// `name` is truncated to 32 bytes.
pub fn encode_member(out: &mut Vec<u8>, a: &Auth, event: u8, vip: u32, name: &[u8]) {
    let name = &name[..name.len().min(MAX_NAME_LEN)];
    put_session(out, ty::MEMBER, a);
    out.extend_from_slice(&[event, name.len() as u8, 0, 0]);
    out.extend_from_slice(&vip.to_be_bytes());
    out.extend_from_slice(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the hex dump layout of the specification (whitespace separated bytes).
    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|x| u8::from_str_radix(x, 16).unwrap())
            .collect()
    }

    const AUTH: Auth = Auth {
        session: 0xC0FF_EE01,
        token: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
    };
    const NONCE: u64 = 0x0123_4567_89AB_CDEF;
    const COOKIE: [u8; 16] = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf,
    ];

    fn v_hello() -> Vec<u8> {
        let mut v = hex("53 33 4c 52 01 01 00 00 01 23 45 67 89 ab cd ef");
        v.extend_from_slice(&[0; 48]);
        v
    }
    fn v_challenge() -> Vec<u8> {
        hex("53 33 4c 52 01 02 00 00 01 23 45 67 89 ab cd ef a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa ab ac ad ae af")
    }
    fn v_join() -> Vec<u8> {
        hex("53 33 4c 52 01 03 00 00 01 23 45 67 89 ab cd ef
             a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa ab ac ad ae af
             00 00 00 00 06 07 00 00 52 4f 4f 4d 2d 31 50 6c
             61 79 65 72 31 00 00 00 00 00 00 00 00 00 00 00")
    }
    fn v_join_ack() -> Vec<u8> {
        hex("53 33 4c 52 01 04 00 00 01 23 45 67 89 ab cd ef
             c0 ff ee 01 11 22 33 44 55 66 77 88 0a 0d 00 02
             10 0c 05 1e 05 a0 00 00")
    }
    fn v_join_nak() -> Vec<u8> {
        hex("53 33 4c 52 01 05 00 00 01 23 45 67 89 ab cd ef 04 00 00 00 00 00 00 00")
    }
    fn v_keepalive() -> Vec<u8> {
        hex("53 33 4c 52 01 06 00 00 c0 ff ee 01 11 22 33 44 55 66 77 88 00 00 00 07 00 00 00 00")
    }
    fn v_keepalive_ack() -> Vec<u8> {
        hex("53 33 4c 52 01 07 00 00 c0 ff ee 01 11 22 33 44 55 66 77 88 00 00 00 07 02 00 00 00")
    }
    fn v_leave() -> Vec<u8> {
        hex("53 33 4c 52 01 08 00 00 c0 ff ee 01 11 22 33 44 55 66 77 88")
    }
    fn v_closed() -> Vec<u8> {
        hex("53 33 4c 52 01 09 00 00 c0 ff ee 01 11 22 33 44 55 66 77 88 01 00 00 00")
    }
    fn v_data() -> Vec<u8> {
        hex("53 33 4c 52 01 10 00 00 c0 ff ee 01 11 22 33 44
             55 66 77 88 0a 0d 00 02 c0 00 0a 0d ff ff 88 b8
             de ad be ef")
    }
    fn v_member() -> Vec<u8> {
        hex("53 33 4c 52 01 11 00 00 c0 ff ee 01 11 22 33 44 55 66 77 88 01 04 00 00 0a 0d 00 03 4f 63 74 6f")
    }

    fn enc(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut v = Vec::new();
        f(&mut v);
        v
    }

    #[test]
    fn vector_hello() {
        assert_eq!(enc(|o| encode_hello(o, NONCE)), v_hello());
        assert_eq!(v_hello().len(), 64);
        assert_eq!(parse(&v_hello()), Ok(Msg::Hello { nonce: NONCE }));
    }

    #[test]
    fn vector_challenge() {
        assert_eq!(enc(|o| encode_challenge(o, NONCE, &COOKIE)), v_challenge());
        assert_eq!(
            parse(&v_challenge()),
            Ok(Msg::Challenge {
                nonce: NONCE,
                cookie: COOKIE
            })
        );
    }

    #[test]
    fn vector_join() {
        assert_eq!(
            enc(|o| encode_join(o, NONCE, &COOKIE, 0, b"ROOM-1", b"Player1")),
            v_join()
        );
        let v = v_join();
        let Ok(Msg::Join(j)) = parse(&v) else {
            panic!("not a JOIN")
        };
        assert_eq!((j.nonce, j.cookie, j.vip), (NONCE, COOKIE, 0));
        assert_eq!(j.room_and_name(), Ok((&b"ROOM-1"[..], &b"Player1"[..])));
    }

    #[test]
    fn vector_join_ack() {
        let a = JoinAck {
            nonce: NONCE,
            session: AUTH.session,
            token: AUTH.token,
            vip: 0x0A0D_0002,
            prefix: 16,
            max_members: 12,
            keepalive_s: 5,
            idle_s: 30,
            max_payload: 1440,
        };
        assert_eq!(enc(|o| encode_join_ack(o, &a)), v_join_ack());
        assert_eq!(parse(&v_join_ack()), Ok(Msg::JoinAck(a)));
    }

    #[test]
    fn vector_join_nak() {
        assert_eq!(
            enc(|o| encode_join_nak(o, NONCE, nak::ROOM_FULL, nak::detail::NONE)),
            v_join_nak()
        );
        assert_eq!(
            parse(&v_join_nak()),
            Ok(Msg::JoinNak {
                version: 1,
                nonce: NONCE,
                reason: 4,
                detail: 0
            })
        );
        // 1.1.13: the detail byte (byte 17) says a source's limit refused the join
        let mut v = v_join_nak();
        v[17] = nak::detail::SOURCE;
        assert_eq!(
            enc(|o| encode_join_nak(o, NONCE, nak::ROOM_FULL, nak::detail::SOURCE)),
            v
        );
        assert!(matches!(
            parse(&v),
            Ok(Msg::JoinNak {
                reason: 4,
                detail: 1,
                ..
            })
        ));
    }

    #[test]
    fn vector_keepalive_and_ack() {
        assert_eq!(enc(|o| encode_keepalive(o, &AUTH, 7)), v_keepalive());
        assert_eq!(parse(&v_keepalive()), Ok(Msg::Keepalive { auth: AUTH, seq: 7 }));
        assert_eq!(enc(|o| encode_keepalive_ack(o, &AUTH, 7, 2)), v_keepalive_ack());
        assert_eq!(
            parse(&v_keepalive_ack()),
            Ok(Msg::KeepaliveAck {
                auth: AUTH,
                seq: 7,
                members: 2
            })
        );
    }

    #[test]
    fn vector_leave_closed() {
        assert_eq!(enc(|o| encode_leave(o, &AUTH)), v_leave());
        assert_eq!(parse(&v_leave()), Ok(Msg::Leave { auth: AUTH }));
        assert_eq!(enc(|o| encode_closed(o, &AUTH, closed::IDLE)), v_closed());
        assert_eq!(parse(&v_closed()), Ok(Msg::Closed { auth: AUTH, reason: 1 }));
    }

    #[test]
    fn vector_data() {
        let h = DataHeader {
            src_vip: 0x0A0D_0002,
            src_port: 49152,
            dst_vip: VBCAST,
            dst_port: 35000,
        };
        assert_eq!(enc(|o| encode_data(o, &AUTH, &h, &[0xde, 0xad, 0xbe, 0xef])), v_data());
        assert_eq!(v_data().len(), 36);
        assert_eq!(
            parse(&v_data()),
            Ok(Msg::Data {
                auth: AUTH,
                hdr: h,
                payload: &[0xde, 0xad, 0xbe, 0xef]
            })
        );
    }

    #[test]
    fn vector_member() {
        assert_eq!(
            enc(|o| encode_member(o, &AUTH, MEMBER_JOINED, 0x0A0D_0003, b"Octo")),
            v_member()
        );
        assert_eq!(
            parse(&v_member()),
            Ok(Msg::Member {
                auth: AUTH,
                event: 1,
                vip: 0x0A0D_0003,
                name: b"Octo"
            })
        );
    }

    #[test]
    fn rejects_short_messages() {
        // (vector, smallest accepted length)
        let cases = [
            (v_hello(), 64),
            (v_challenge(), 32),
            (v_join(), 64),
            (v_join_ack(), 40),
            (v_join_nak(), 24),
            (v_keepalive(), 28),
            (v_keepalive_ack(), 28),
            (v_leave(), 20),
            (v_closed(), 24),
            (v_data(), 32),
            (v_member(), 32),
        ];
        for (v, min) in cases {
            for n in 0..v.len() {
                assert_eq!(parse(&v[..n]).is_ok(), n >= min, "type {:#x} at {n} bytes", v[5]);
            }
        }
    }

    #[test]
    fn rejects_bad_headers() {
        let mut v = v_keepalive();
        v[0] = b'X';
        assert_eq!(parse(&v), Err(ParseError::BadMagic));
        let mut v = v_keepalive();
        v[4] = 2;
        assert_eq!(parse(&v), Err(ParseError::OtherVersion { ty: ty::KEEPALIVE }));
        let mut v = v_keepalive();
        v[5] = 0x42;
        assert_eq!(parse(&v), Err(ParseError::UnknownType(0x42)));
        v[5] = 0x00;
        assert_eq!(parse(&v), Err(ParseError::UnknownType(0)));
        assert_eq!(parse(&vec![0u8; MAX_DATAGRAM + 1]), Err(ParseError::TooLarge));
        assert_eq!(parse(&[]), Err(ParseError::TooShort));
    }

    #[test]
    fn member_with_oversized_name_is_rejected() {
        let mut v = v_member();
        v[21] = 33;
        v.resize(80, b'x');
        assert!(parse(&v).is_err());
        let mut v = v_member();
        v[21] = 10; // longer than the datagram holds
        assert!(parse(&v).is_err());
    }

    #[test]
    fn reserved_fields_are_ignored() {
        let mut v = v_keepalive();
        v[6] = 0xFF;
        v[7] = 0xEE;
        v[27] = 0x99;
        assert_eq!(parse(&v), Ok(Msg::Keepalive { auth: AUTH, seq: 7 }));
    }

    #[test]
    fn join_validation_order_and_limits() {
        let mk = |room: &[u8], name: &[u8]| enc(|o| encode_join(o, NONCE, &COOKIE, 0, room, name));
        let check = |v: &[u8]| match parse(v).unwrap() {
            Msg::Join(j) => j.room_and_name().map(|(r, n)| (r.len(), n.len())),
            _ => unreachable!(),
        };
        assert_eq!(check(&mk(b"A", b"")), Ok((1, 0)));
        assert_eq!(check(&mk(&[b'A'; 32], &[b'x'; 32])), Ok((32, 32)));
        assert_eq!(check(&mk(b"", b"x")), Err(JoinError::Malformed));
        assert_eq!(check(&mk(&[b'A'; 33], b"")), Err(JoinError::Malformed));
        assert_eq!(check(&mk(b"A", &[b'x'; 33])), Err(JoinError::Malformed));
        assert_eq!(check(&mk(b"abc", b"x")), Err(JoinError::Room));
        assert_eq!(check(&mk(b"A B", b"x")), Err(JoinError::Room));
        assert_eq!(check(&mk(b"AB", b"x\ny")), Err(JoinError::Name));
        assert_eq!(check(&mk(b"AB", b"x\x7f")), Err(JoinError::Name));
        assert_eq!(check(&mk(b"AB", "été".as_bytes())), Ok((2, 5)));
        // room invalid and name invalid: the room is reported first (step 3 order)
        assert_eq!(check(&mk(b"ab", b"\x01")), Err(JoinError::Room));
        // lengths larger than the datagram
        let mut v = mk(b"ROOM", b"");
        v[36] = 32;
        v[37] = 32;
        assert_eq!(check(&v), Err(JoinError::Malformed));
    }

    #[test]
    fn data_max_payload_roundtrip() {
        let h = DataHeader {
            src_vip: 1,
            src_port: 2,
            dst_vip: 3,
            dst_port: 4,
        };
        let p = vec![7u8; MAX_PAYLOAD];
        let v = enc(|o| encode_data(o, &AUTH, &h, &p));
        assert_eq!(v.len(), MAX_DATAGRAM);
        assert!(matches!(parse(&v), Ok(Msg::Data { payload, .. }) if payload.len() == 1440));
    }
}
