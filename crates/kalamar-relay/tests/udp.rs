//! The real server over loopback UDP sockets (127.0.0.1 only, ephemeral ports).

use std::net::SocketAddr;
use std::time::Duration;

use kalamar_relay::relay::RelayConfig;
use kalamar_relay::server;
use kalamar_relay::wire::{self, closed, Auth, DataHeader, Msg};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::time::timeout;

struct Peer {
    sock: UdpSocket,
    relay: SocketAddr,
    auth: Auth,
    vip: u32,
}

async fn recv(sock: &UdpSocket) -> Option<Vec<u8>> {
    let mut buf = [0u8; 2048];
    match timeout(Duration::from_millis(1500), sock.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => Some(buf[..n].to_vec()),
        _ => None,
    }
}

async fn expect_silence(sock: &UdpSocket) {
    let mut buf = [0u8; 2048];
    assert!(
        timeout(Duration::from_millis(250), sock.recv_from(&mut buf))
            .await
            .is_err(),
        "unexpected datagram"
    );
}

async fn join(relay: SocketAddr, room: &str, name: &str, want: u32) -> Result<Peer, u8> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let nonce: u64 = 0xABCD_0000 + sock.local_addr().unwrap().port() as u64;
    let mut h = Vec::new();
    wire::encode_hello(&mut h, nonce);
    sock.send_to(&h, relay).await.unwrap();
    let d = recv(&sock).await.expect("CHALLENGE");
    assert_eq!(d.len(), 32);
    let Ok(Msg::Challenge { nonce: n, cookie }) = wire::parse(&d) else {
        panic!("not a CHALLENGE")
    };
    assert_eq!(n, nonce);
    let mut j = Vec::new();
    wire::encode_join(&mut j, nonce, &cookie, want, room.as_bytes(), name.as_bytes());
    sock.send_to(&j, relay).await.unwrap();
    let d = recv(&sock).await.expect("JOIN answer");
    match wire::parse(&d).unwrap() {
        Msg::JoinAck(a) => Ok(Peer {
            sock,
            relay,
            auth: Auth {
                session: a.session,
                token: a.token,
            },
            vip: a.vip,
        }),
        Msg::JoinNak { reason, .. } => Err(reason),
        m => panic!("{m:?}"),
    }
}

impl Peer {
    async fn send_data(&self, dst: u32, payload: &[u8]) {
        let mut v = Vec::new();
        let h = DataHeader {
            src_vip: self.vip,
            src_port: 49152,
            dst_vip: dst,
            dst_port: 35000,
        };
        wire::encode_data(&mut v, &self.auth, &h, payload);
        self.sock.send_to(&v, self.relay).await.unwrap();
    }
    /// Next DATA, skipping MEMBER messages.
    async fn next_data(&self) -> Option<(u32, Vec<u8>)> {
        loop {
            let d = recv(&self.sock).await?;
            match wire::parse(&d).unwrap() {
                Msg::Data { auth, hdr, payload } => {
                    assert_eq!(auth, self.auth);
                    return Some((hdr.src_vip, payload.to_vec()));
                }
                Msg::Member { .. } => {}
                m => panic!("unexpected {m:?}"),
            }
        }
    }
}

async fn start(
    cfg: RelayConfig,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<kalamar_relay::relay::Stats>>,
) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let h = tokio::spawn(server::run(sock, cfg, Duration::ZERO, async move {
        let _ = rx.await;
    }));
    (addr, tx, h)
}

#[tokio::test]
async fn join_broadcast_unicast_over_udp() {
    let (relay, stop, handle) = start(RelayConfig::default()).await;
    let a = join(relay, "UDP-ROOM", "A", 0).await.unwrap();
    let b = join(relay, "UDP-ROOM", "B", 0).await.unwrap();
    let c = join(relay, "UDP-ROOM", "C", 0).await.unwrap();
    let other = join(relay, "OTHER-ROOM", "X", 0).await.unwrap();
    assert_eq!(
        (a.vip, b.vip, c.vip, other.vip),
        (0x0A0D_0001, 0x0A0D_0002, 0x0A0D_0003, 0x0A0D_0001)
    );

    a.send_data(wire::VBCAST, b"broadcast").await;
    assert_eq!(b.next_data().await, Some((a.vip, b"broadcast".to_vec())));
    assert_eq!(c.next_data().await, Some((a.vip, b"broadcast".to_vec())));

    b.send_data(c.vip, b"unicast").await;
    assert_eq!(c.next_data().await, Some((b.vip, b"unicast".to_vec())));

    // a maximum size datagram survives the trip
    let big = vec![0x5A; wire::MAX_PAYLOAD];
    c.send_data(a.vip, &big).await;
    assert_eq!(a.next_data().await, Some((c.vip, big)));

    // nothing leaked into the other room
    expect_silence(&other.sock).await;

    // keepalive round trip
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &a.auth, 3);
    a.sock.send_to(&ka, relay).await.unwrap();
    loop {
        let d = recv(&a.sock).await.expect("KEEPALIVE_ACK");
        if let Ok(Msg::KeepaliveAck { seq, members, .. }) = wire::parse(&d) {
            assert_eq!((seq, members), (3, 3));
            break;
        }
    }
    drop(stop);
    // the sender being dropped also ends the future: the server shuts down
    let stats = handle.await.unwrap().unwrap();
    assert!(stats.forwarded >= 4);
}

#[tokio::test]
async fn graceful_shutdown_sends_closed_4() {
    let (relay, stop, handle) = start(RelayConfig::default()).await;
    let a = join(relay, "BYE", "A", 0).await.unwrap();
    let b = join(relay, "BYE", "B", 0).await.unwrap();
    stop.send(()).unwrap();
    for p in [&a, &b] {
        loop {
            let d = recv(&p.sock).await.expect("CLOSED");
            if let Ok(Msg::Closed { auth, reason }) = wire::parse(&d) {
                assert_eq!((auth, reason), (p.auth, closed::SHUTDOWN));
                break;
            }
        }
    }
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn garbage_does_not_disturb_the_server() {
    let (relay, stop, handle) = start(RelayConfig::default()).await;
    let a = join(relay, "ROOM", "A", 0).await.unwrap();
    let b = join(relay, "ROOM", "B", 0).await.unwrap();
    let junk = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut x = 0x1234_5678_9ABC_DEF1u64;
    for i in 0..2000usize {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = (x % 1700) as usize;
        let mut v: Vec<u8> = (0..n).map(|k| (x >> (k % 56)) as u8 ^ k as u8).collect();
        if i % 2 == 0 && n >= 8 {
            v[..6].copy_from_slice(b"S3LR\x01\x10");
        }
        junk.send_to(&v, relay).await.unwrap();
        if i % 20 == 19 {
            // the burst would overflow the default socket buffer of a loopback receiver
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    junk.send_to(&[], relay).await.unwrap();
    // a datagram larger than any legal one
    junk.send_to(&vec![1u8; 4000], relay).await.unwrap();
    a.send_data(wire::VBCAST, b"still alive").await;
    assert_eq!(b.next_data().await, Some((a.vip, b"still alive".to_vec())));
    drop(stop);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn room_full_over_udp() {
    let cfg = RelayConfig {
        max_members: 2,
        ..RelayConfig::default()
    };
    let (relay, stop, handle) = start(cfg).await;
    let _a = join(relay, "R", "A", 0).await.unwrap();
    let _b = join(relay, "R", "B", 0).await.unwrap();
    assert_eq!(join(relay, "R", "C", 0).await.err(), Some(wire::nak::ROOM_FULL));
    drop(stop);
    handle.await.unwrap().unwrap();
}

/// The small client (`client.rs`) against a relay bound by a secret: a HELLO is answered, a sealed code is opened
/// and left at once, any other code is refused with reason 3.
#[tokio::test]
async fn the_minimal_client_against_a_bound_relay() {
    use kalamar_relay::client::{hello_rtt, try_join, JoinOutcome};
    use kalamar_relay::roomauth::{seal, RoomSecret};
    let secret = RoomSecret::new(b"0123456789abcdef0123456789abcdef".to_vec());
    let cfg = RelayConfig {
        room_secret: Some(secret.clone()),
        ..RelayConfig::default()
    };
    let (relay, stop, handle) = start(cfg).await;
    let rtt = hello_rtt(relay, Duration::from_secs(2)).await.expect("CHALLENGE");
    assert!(rtt < Duration::from_secs(1));
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sealed = seal(secret.bytes(), "K7Q2MXA9P84DZB3W8R6TNY5HCF", unix).unwrap();
    let t = Duration::from_secs(2);
    assert_eq!(try_join(relay, &sealed, "Ana", t).await.unwrap(), JoinOutcome::Joined);
    assert_eq!(
        try_join(relay, "K7Q2MXA9P84DZB3W8R6TNY5HCF", "Ana", t).await.unwrap(),
        JoinOutcome::Refused(wire::nak::ROOM)
    );
    // A port where nothing answers.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let nobody = silent.local_addr().unwrap();
    assert_eq!(hello_rtt(nobody, Duration::from_millis(300)).await, None);
    assert_eq!(
        try_join(nobody, &sealed, "Ana", Duration::from_millis(300))
            .await
            .unwrap(),
        JoinOutcome::NoAnswer
    );
    drop(stop);
    let stats = handle.await.unwrap().unwrap();
    assert_eq!((stats.joins, stats.leaves, stats.refused), (1, 1, 1));
}

#[tokio::test]
async fn leave_over_udp_notifies_the_others() {
    let (relay, stop, handle) = start(RelayConfig::default()).await;
    let a = join(relay, "R", "A", 0).await.unwrap();
    let b = join(relay, "R", "B", 0).await.unwrap();
    let mut lv = Vec::new();
    wire::encode_leave(&mut lv, &b.auth);
    b.sock.send_to(&lv, relay).await.unwrap();
    loop {
        let d = recv(&a.sock).await.expect("MEMBER(left)");
        if let Ok(Msg::Member { event: 2, vip, .. }) = wire::parse(&d) {
            assert_eq!(vip, b.vip);
            break;
        }
    }
    drop(stop);
    handle.await.unwrap().unwrap();
}

async fn start_workers(
    cfg: RelayConfig,
    workers: usize,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<kalamar_relay::relay::Stats>>,
) {
    let sockets = server::bind("127.0.0.1:0".parse().unwrap(), workers).unwrap();
    let addr = sockets[0].local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let h = tokio::spawn(server::run_on(sockets, workers, cfg, Duration::ZERO, async move {
        let _ = rx.await;
    }));
    (addr, tx, h)
}

/// Four workers (1.1.13): the same exchanges as one, rooms still isolated, and the shutdown reaches every member.
#[tokio::test]
async fn four_workers_relay_like_one() {
    let (relay, stop, handle) = start_workers(RelayConfig::default(), 4).await;
    let mut room = Vec::new();
    for n in ["A", "B", "C", "D", "E"] {
        room.push(join(relay, "WORKERS", n, 0).await.unwrap());
    }
    let other = join(relay, "ELSEWHERE", "X", 0).await.unwrap();
    for (i, p) in room.iter().enumerate() {
        p.send_data(wire::VBCAST, format!("from {i}").as_bytes()).await;
    }
    for (i, p) in room.iter().enumerate() {
        let mut got = Vec::new();
        for _ in 0..room.len() - 1 {
            got.push(p.next_data().await.expect("a broadcast"));
        }
        got.sort();
        let want: Vec<(u32, Vec<u8>)> = room
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, q)| (q.vip, format!("from {j}").into_bytes()))
            .collect();
        assert_eq!(got, want, "member {i}");
    }
    expect_silence(&other.sock).await;
    stop.send(()).unwrap();
    for p in room.iter().chain([&other]) {
        loop {
            let d = recv(&p.sock).await.expect("CLOSED");
            if let Ok(Msg::Closed { reason, .. }) = wire::parse(&d) {
                assert_eq!(reason, closed::SHUTDOWN);
                break;
            }
        }
    }
    let stats = handle.await.unwrap().unwrap();
    assert_eq!((stats.joins, stats.forwarded), (6, 20));
}

/// The service bound to a relay asks for its load (1.1.13); an open relay, or a query with another secret, gets
/// nothing.
#[tokio::test]
async fn a_bound_relay_tells_its_service_its_load() {
    use kalamar_relay::client::status;
    use kalamar_relay::roomauth::{seal, RoomSecret};
    let secret = RoomSecret::new(b"0123456789abcdef0123456789abcdef".to_vec());
    let cfg = RelayConfig {
        room_secret: Some(secret.clone()),
        max_sessions: 100,
        max_rooms: 0,
        ..RelayConfig::default()
    };
    let (relay, stop, handle) = start_workers(cfg, 2).await;
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let room = seal(secret.bytes(), "K7Q2MXA9P84DZB3W8R6TNY5HCF", unix).unwrap();
    let _a = join(relay, &room, "A", 0).await.unwrap();
    let _b = join(relay, &room, "B", 0).await.unwrap();
    let l = status(relay, &secret, Duration::from_secs(2)).await.expect("an answer");
    assert_eq!(
        (l.workers, l.sessions, l.rooms, l.max_sessions, l.max_rooms),
        (2, 2, 1, 100, 0)
    );
    assert_eq!((l.max_members, l.worker_busy.len()), (12, 2));
    let wrong = RoomSecret::new(b"another secret of 32 bytes, too.".to_vec());
    assert_eq!(status(relay, &wrong, Duration::from_millis(400)).await, None);
    drop(stop);
    let stats = handle.await.unwrap().unwrap();
    assert_eq!(stats.dropped[kalamar_relay::relay::DropReason::Status as usize], 1);

    let (open, stop, handle) = start(RelayConfig::default()).await;
    assert_eq!(
        status(open, &secret, Duration::from_millis(400)).await,
        None,
        "an open relay says nothing"
    );
    drop(stop);
    let stats = handle.await.unwrap().unwrap();
    assert_eq!(stats.tx, 0);
}

/// On Linux each worker gets a socket of its own on one port (SO_REUSEPORT: the kernel keeps each client on one of
/// them); elsewhere the workers share one socket. Either way they answer on the same address.
#[test]
fn the_workers_sockets() {
    let s = server::bind("127.0.0.1:0".parse().unwrap(), 4).unwrap();
    let port = s[0].local_addr().unwrap().port();
    assert_ne!(port, 0);
    assert!(s.iter().all(|x| x.local_addr().unwrap().port() == port));
    assert_eq!(s.len(), if cfg!(target_os = "linux") { 4 } else { 1 });
    assert_eq!(server::bind("127.0.0.1:0".parse().unwrap(), 1).unwrap().len(), 1);
}
