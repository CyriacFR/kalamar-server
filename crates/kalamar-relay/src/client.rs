//! A minimal client of relay protocol 1 for tools and tests (the game has its own): a `HELLO` timed against its
//! `CHALLENGE` (the reachability check and the latency measurement of `relay.md` 4.1: nothing is joined, the relay
//! keeps no state), a `JOIN` followed at once by a `LEAVE` (whether a relay opens a given room), and the status query
//! of the service bound to a relay (its load, protocol 1.1.13).

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::wire::{self, Auth, Msg, COOKIE_LEN};

/// What a relay answered to a `JOIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOutcome {
    /// `JOIN_ACK`: the relay opened (or let us into) the room; the session was left at once.
    Joined,
    /// `JOIN_NAK` with its reason (`wire::nak`: 3 is a room this relay does not open).
    Refused(u8),
    /// No `CHALLENGE`, or no answer to the `JOIN`, in time.
    NoAnswer,
}

fn nonce() -> u64 {
    let mut n = [0u8; 8];
    rand::fill(&mut n[..]);
    u64::from_be_bytes(n)
}

async fn socket_for(addr: SocketAddr) -> io::Result<UdpSocket> {
    let local: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0".parse().expect("address")
    } else {
        "[::]:0".parse().expect("address")
    };
    let sock = UdpSocket::bind(local).await?;
    sock.connect(addr).await?;
    Ok(sock)
}

/// Waits for the first datagram `pick` accepts until `deadline`.
async fn wait_for<T>(sock: &UdpSocket, deadline: Instant, pick: impl Fn(Msg<'_>) -> Option<T>) -> Option<T> {
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())?;
        match tokio::time::timeout(left, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => {
                if let Some(v) = wire::parse(&buf[..n]).ok().and_then(&pick) {
                    return Some(v);
                }
            }
            // Windows reports an earlier ICMP "port unreachable" as a receive error: keep waiting.
            Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => {}
            _ => return None,
        }
    }
}

async fn challenge(sock: &UdpSocket, nonce: u64, deadline: Instant) -> io::Result<Option<[u8; COOKIE_LEN]>> {
    let mut hello = Vec::with_capacity(wire::HELLO_LEN);
    wire::encode_hello(&mut hello, nonce);
    sock.send(&hello).await?;
    Ok(wait_for(sock, deadline, |m| match m {
        Msg::Challenge { nonce: n, cookie } if n == nonce => Some(cookie),
        _ => None,
    })
    .await)
}

/// The round trip of one `HELLO` to the relay at `addr`; `None` when no `CHALLENGE` came within `timeout`.
pub async fn hello_rtt(addr: SocketAddr, timeout: Duration) -> Option<Duration> {
    let sock = socket_for(addr).await.ok()?;
    let start = Instant::now();
    challenge(&sock, nonce(), start + timeout).await.ok().flatten()?;
    Some(start.elapsed())
}

/// Joins `room` under `name` on the relay at `addr`, then leaves at once.
pub async fn try_join(addr: SocketAddr, room: &str, name: &str, timeout: Duration) -> io::Result<JoinOutcome> {
    let sock = socket_for(addr).await?;
    let deadline = Instant::now() + timeout;
    let n = nonce();
    let Some(cookie) = challenge(&sock, n, deadline).await? else {
        return Ok(JoinOutcome::NoAnswer);
    };
    let mut join = Vec::new();
    wire::encode_join(&mut join, n, &cookie, 0, room.as_bytes(), name.as_bytes());
    sock.send(&join).await?;
    let answer = wait_for(&sock, deadline, |m| match m {
        Msg::JoinAck(a) if a.nonce == n => Some(Ok(Auth {
            session: a.session,
            token: a.token,
        })),
        Msg::JoinNak { nonce, reason, .. } if nonce == n => Some(Err(reason)),
        _ => None,
    })
    .await;
    Ok(match answer {
        Some(Ok(auth)) => {
            let mut leave = Vec::new();
            wire::encode_leave(&mut leave, &auth);
            sock.send(&leave).await?;
            JoinOutcome::Joined
        }
        Some(Err(reason)) => JoinOutcome::Refused(reason),
        None => JoinOutcome::NoAnswer,
    })
}

/// The load of the relay at `addr`, asked with the secret it shares with its service (protocol 1.1.13,
/// `status.rs`); `None` when no valid answer came within `timeout` (an open relay, or one that does not know the query,
/// never answers).
pub async fn status(
    addr: SocketAddr,
    secret: &crate::roomauth::RoomSecret,
    timeout: Duration,
) -> Option<crate::status::Load> {
    let key = crate::status::StatusKey::new(secret);
    let sock = socket_for(addr).await.ok()?;
    let n = nonce();
    let mut q = Vec::with_capacity(wire::STATUS_LEN);
    key.encode_query(&mut q, n, crate::status::unix_ms());
    sock.send(&q).await.ok()?;
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())?;
        match tokio::time::timeout(left, sock.recv(&mut buf)).await {
            Ok(Ok(len)) => {
                if let Some(l) = key.parse_status(&buf[..len], n) {
                    return Some(l);
                }
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => {}
            _ => return None,
        }
    }
}
