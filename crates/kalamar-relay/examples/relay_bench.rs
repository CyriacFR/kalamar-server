//! Load bench of a relay: rooms of simulated members that exchange `DATA` like the members of a real match, on one
//! machine. Each member is a UDP socket of its own (its own source port, as players behind different addresses), joins
//! with the real handshake, then sends one datagram to every other member of its room `--pps` times a second (a full
//! mesh of unicasts: what the game does, measured from its network log), with payload sizes drawn from the same log.
//! Every datagram carries its send time: the receiving member computes the latency through the relay, the losses and
//! the datagrams that arrived out of order.
//!
//!   cargo run --release -p kalamar-relay --example relay_bench -- --relay 127.0.0.1:47399 --rooms 25,50,100 --pps 15
//!
//! The relay must accept the bench's handshakes from one address: start it with `--hello-rate 0 --join-rate 0`
//! (loopback is exempt from the per-source limits). With `--secret-file` the room codes are sealed with the relay's
//! secret (a bound relay); without it the relay must run with `--open`. `--direct` sends the same datagrams from
//! member to member without a relay: the latency of the machine and of this generator alone, to subtract.
//!
//! Members send from `--senders` threads that pace each member to the microsecond (they spin: each takes a whole
//! processor), so that the members' datagrams spread over time as real players' do; timers of the operating system
//! (15.6 ms on Windows by default) would send them in bursts. Members receive on a runtime of `--threads` threads.
//!
//! Each step adds rooms up to the next count of `--rooms`, waits `--warmup` seconds, then measures `--secs` seconds
//! (datagrams sent in that window only) and prints one line; `RESULT` lines are for scripts. The window's start and end
//! are printed in Unix milliseconds, so that a sampler of the relay's CPU and memory can be lined up with them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kalamar_relay::wire::{self, Auth, DataHeader, Msg};
use tokio::net::UdpSocket;

/// Payload sizes (bytes, without the relay's 32-byte header) and how often each was seen in the network log of a real
/// three-member match (12 most frequent sizes, about 16 000 datagrams; mean 158 bytes).
const SIZES: [(usize, u32); 12] = [
    (173, 4500),
    (125, 4117),
    (205, 3048),
    (141, 2208),
    (61, 628),
    (157, 582),
    (221, 445),
    (45, 357),
    (237, 174),
    (189, 154),
    (269, 124),
    (253, 123),
];
/// The bench's own fields at the start of a payload: tag, sender, receiver, sequence, send time.
const MARK: [u8; 2] = *b"KB";
const OWN: usize = 2 + 4 + 4 + 4 + 8;
/// IPv4 and UDP headers, for the bandwidth on a real link (Ethernet framing not counted).
const IP_UDP: u64 = 28;

static EPOCH: OnceLock<Instant> = OnceLock::new();

fn now_ns() -> u64 {
    EPOCH.get().expect("epoch").elapsed().as_nanos() as u64
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Latency histogram in microseconds: 1 us steps to 1 ms, 10 us to 10 ms, 1 ms to 1 s, then one overflow bucket.
const BUCKETS: usize = 1000 + 900 + 990 + 1;

fn bucket(us: u64) -> usize {
    if us < 1000 {
        us as usize
    } else if us < 10_000 {
        1000 + ((us - 1000) / 10) as usize
    } else if us < 1_000_000 {
        1900 + ((us - 10_000) / 1000) as usize
    } else {
        BUCKETS - 1
    }
}

fn bucket_us(i: usize) -> f64 {
    if i < 1000 {
        i as f64 + 0.5
    } else if i < 1900 {
        1000.0 + (i - 1000) as f64 * 10.0 + 5.0
    } else if i < BUCKETS - 1 {
        10_000.0 + (i - 1900) as f64 * 1000.0 + 500.0
    } else {
        1_000_000.0
    }
}

struct Shared {
    hist: Vec<AtomicU64>,
    sent: AtomicU64,
    sent_bytes: AtomicU64,
    recv: AtomicU64,
    recv_bytes: AtomicU64,
    reordered: AtomicU64,
    would_block: AtomicU64,
    /// Datagrams sent (by send time) in `[win_start, win_end)` are counted.
    win_start: AtomicU64,
    win_end: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn in_window(&self, ts: u64) -> bool {
        ts >= self.win_start.load(Relaxed) && ts < self.win_end.load(Relaxed)
    }
    fn reset(&self) {
        for h in &self.hist {
            h.store(0, Relaxed);
        }
        for c in [
            &self.sent,
            &self.sent_bytes,
            &self.recv,
            &self.recv_bytes,
            &self.reordered,
            &self.would_block,
        ] {
            c.store(0, Relaxed);
        }
    }
    fn percentile(&self, p: f64) -> f64 {
        let counts: Vec<u64> = self.hist.iter().map(|h| h.load(Relaxed)).collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return f64::NAN;
        }
        let want = ((total as f64) * p).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, c) in counts.iter().enumerate() {
            acc += c;
            if acc >= want {
                return bucket_us(i);
            }
        }
        bucket_us(BUCKETS - 1)
    }
    fn max_us(&self) -> f64 {
        self.hist
            .iter()
            .rposition(|h| h.load(Relaxed) > 0)
            .map_or(f64::NAN, bucket_us)
    }
}

struct Opts {
    relay: Option<SocketAddr>,
    direct: bool,
    rooms: Vec<usize>,
    members: usize,
    pps: f64,
    payload: Option<usize>,
    warmup: f64,
    secs: f64,
    threads: usize,
    senders: usize,
    secret: Option<Vec<u8>>,
    stop_loss: f64,
}

fn parse_opts() -> Result<Opts, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let num = |name: &str, d: f64| -> Result<f64, String> {
        get(name).map_or(Ok(d), |v| v.parse().map_err(|_| format!("{name}: a number")))
    };
    let secret = match get("--secret-file") {
        Some(f) => Some(
            kalamar_relay::roomauth::RoomSecret::from_file(std::path::Path::new(&f))?
                .bytes()
                .to_vec(),
        ),
        None => None,
    };
    let rooms = get("--rooms")
        .unwrap_or_else(|| "10".into())
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<usize>()
                .map_err(|_| "--rooms: counts like 10,20,40".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let o = Opts {
        relay: get("--relay")
            .map(|a| a.parse().map_err(|_| "--relay: ip:port".to_string()))
            .transpose()?,
        direct: args.iter().any(|a| a == "--direct"),
        rooms,
        members: num("--members", 8.0)? as usize,
        pps: num("--pps", 15.0)?,
        payload: get("--payload")
            .map(|v| v.parse().map_err(|_| "--payload: bytes".to_string()))
            .transpose()?,
        warmup: num("--warmup", 3.0)?,
        secs: num("--secs", 10.0)?,
        threads: num("--threads", 4.0)? as usize,
        senders: num("--senders", 2.0)? as usize,
        secret,
        stop_loss: num("--stop-loss", 20.0)?,
    };
    if o.relay.is_none() && !o.direct {
        return Err("--relay ip:port (or --direct)".into());
    }
    if !(2..=254).contains(&o.members) || o.pps <= 0.0 || o.threads == 0 || o.senders == 0 {
        return Err("--members 2..=254, --pps above 0, --threads and --senders at least 1".into());
    }
    Ok(o)
}

/// The payload size of datagram `n`, following the measured distribution.
fn size_table(fixed: Option<usize>) -> Vec<usize> {
    if let Some(n) = fixed {
        return vec![n.clamp(OWN, wire::MAX_PAYLOAD)];
    }
    let total: u32 = SIZES.iter().map(|s| s.1).sum();
    let mut t = Vec::with_capacity(1024);
    for &(size, n) in &SIZES {
        let k = ((n as f64 / total as f64) * 1024.0).round() as usize;
        t.extend(std::iter::repeat(size).take(k.max(1)));
    }
    // interleave, so that consecutive datagrams differ like in the log
    let mut out = Vec::with_capacity(t.len());
    let step = 389; // prime, coprime with the table's length
    let len = t.len();
    for i in 0..len {
        out.push(t[(i * step) % len]);
    }
    out
}

struct Member {
    idx: u32,
    /// The handshake and the receiving side.
    sock: Arc<UdpSocket>,
    /// The same socket, for the sender threads (non-blocking).
    send: std::net::UdpSocket,
    auth: Auth,
    vip: u32,
}

/// A bound socket, as a tokio socket and a clone for the sender threads.
fn socket(connect: Option<SocketAddr>) -> Result<(UdpSocket, std::net::UdpSocket), String> {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    if let Some(a) = connect {
        s.connect(a).map_err(|e| e.to_string())?;
    }
    s.set_nonblocking(true).map_err(|e| e.to_string())?;
    let send = s.try_clone().map_err(|e| e.to_string())?;
    Ok((UdpSocket::from_std(s).map_err(|e| e.to_string())?, send))
}

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    rand::fill(&mut b[..]);
    u64::from_be_bytes(b)
}

fn room_code(secret: Option<&[u8]>) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut b = [0u8; 26];
    rand::fill(&mut b[..]);
    let raw: String = b.iter().map(|x| ALPHABET[(x & 31) as usize] as char).collect();
    match secret {
        Some(s) => {
            let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            kalamar_relay::roomauth::seal(s, &raw, unix).expect("a sealable code")
        }
        None => raw,
    }
}

/// The real handshake: HELLO, CHALLENGE, JOIN, JOIN_ACK (retried on silence).
async fn join(relay: SocketAddr, room: &str, idx: u32) -> Result<Member, String> {
    let (sock, send) = socket(Some(relay))?;
    let mut buf = [0u8; 2048];
    for _attempt in 0..8 {
        let nonce = random_u64();
        let mut out = Vec::new();
        wire::encode_hello(&mut out, nonce);
        let _ = sock.send(&out).await;
        let cookie = loop {
            match tokio::time::timeout(Duration::from_millis(1000), sock.recv(&mut buf)).await {
                Ok(Ok(n)) => match wire::parse(&buf[..n]) {
                    Ok(Msg::Challenge { nonce: x, cookie }) if x == nonce => break Some(cookie),
                    _ => continue,
                },
                Ok(Err(_)) => continue,
                Err(_) => break None,
            }
        };
        let Some(cookie) = cookie else { continue };
        out.clear();
        let name = format!("b{idx}");
        wire::encode_join(&mut out, nonce, &cookie, 0, room.as_bytes(), name.as_bytes());
        let _ = sock.send(&out).await;
        let answer = loop {
            match tokio::time::timeout(Duration::from_millis(1000), sock.recv(&mut buf)).await {
                Ok(Ok(n)) => match wire::parse(&buf[..n]) {
                    Ok(Msg::JoinAck(a)) if a.nonce == nonce => break Some(Ok(a)),
                    Ok(Msg::JoinNak { nonce: x, reason, .. }) if x == nonce => break Some(Err(reason)),
                    _ => continue,
                },
                Ok(Err(_)) => continue,
                Err(_) => break None,
            }
        };
        match answer {
            Some(Ok(a)) => {
                return Ok(Member {
                    idx,
                    sock: Arc::new(sock),
                    send,
                    auth: Auth {
                        session: a.session,
                        token: a.token,
                    },
                    vip: a.vip,
                })
            }
            Some(Err(reason)) => return Err(format!("JOIN_NAK reason {reason}")),
            None => continue,
        }
    }
    Err("no answer from the relay".into())
}

async fn direct_member(idx: u32) -> Result<Member, String> {
    let (sock, send) = socket(None)?;
    Ok(Member {
        idx,
        sock: Arc::new(sock),
        send,
        auth: Auth {
            session: idx,
            token: [0; 8],
        },
        vip: wire::VNET + 1 + idx,
    })
}

/// Receives everything addressed to member `me` until the bench stops.
async fn receive(me: u32, sock: Arc<UdpSocket>, sh: Arc<Shared>) {
    let mut buf = [0u8; 2048];
    let mut last: HashMap<u32, u32> = HashMap::new();
    while !sh.stop.load(Relaxed) {
        let n = match tokio::time::timeout(Duration::from_millis(500), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(_)) | Err(_) => continue,
        };
        let now = now_ns();
        let Ok(Msg::Data { payload, .. }) = wire::parse(&buf[..n]) else {
            continue;
        };
        if payload.len() < OWN || payload[..2] != MARK {
            continue;
        }
        let from = u32::from_be_bytes(payload[2..6].try_into().expect("4"));
        let to = u32::from_be_bytes(payload[6..10].try_into().expect("4"));
        let seq = u32::from_be_bytes(payload[10..14].try_into().expect("4"));
        let ts = u64::from_be_bytes(payload[14..22].try_into().expect("8"));
        if to != me {
            continue;
        }
        let prev = last.entry(from).or_insert(0);
        if seq < *prev {
            if sh.in_window(ts) {
                sh.reordered.fetch_add(1, Relaxed);
            }
        } else {
            *prev = seq;
        }
        if sh.in_window(ts) {
            sh.recv.fetch_add(1, Relaxed);
            sh.recv_bytes.fetch_add(n as u64, Relaxed);
            let us = now.saturating_sub(ts) / 1000;
            sh.hist[bucket(us)].fetch_add(1, Relaxed);
        }
    }
}

/// A member as a sender thread sees it: when it sends next, and to whom.
struct Paced {
    me: Arc<Member>,
    peers: Vec<(u32, u32, SocketAddr)>,
    direct: bool,
    next: Instant,
    seq: u32,
}

type Inbox = Arc<std::sync::Mutex<Vec<Paced>>>;

/// One sender thread: sends one datagram to each peer of each of its members every `1/pps` seconds, each member from
/// a random phase, spinning between sends. New members come through `inbox`.
fn sender(inbox: Inbox, pps: f64, sizes: Arc<Vec<usize>>, sh: Arc<Shared>) {
    let period = Duration::from_secs_f64(1.0 / pps);
    let mut members: Vec<Paced> = Vec::new();
    let mut out = Vec::with_capacity(wire::MAX_DATAGRAM);
    let mut payload = vec![0u8; wire::MAX_PAYLOAD];
    let mut checked = Instant::now();
    while !sh.stop.load(Relaxed) {
        let now = Instant::now();
        if now.duration_since(checked) > Duration::from_millis(20) {
            members.append(&mut inbox.lock().expect("inbox"));
            checked = now;
        }
        let mut sent_any = false;
        for p in members.iter_mut() {
            if p.next > now {
                continue;
            }
            // a member late by more than a period skips a turn (it never sends in a burst)
            p.next += period;
            if p.next < now {
                p.next = now + period;
            }
            sent_any = true;
            for &(peer, peer_vip, peer_addr) in &p.peers {
                p.seq = p.seq.wrapping_add(1);
                let size = sizes[(p.seq as usize + p.me.idx as usize) % sizes.len()];
                let ts = now_ns();
                payload[..2].copy_from_slice(&MARK);
                payload[2..6].copy_from_slice(&p.me.idx.to_be_bytes());
                payload[6..10].copy_from_slice(&peer.to_be_bytes());
                payload[10..14].copy_from_slice(&p.seq.to_be_bytes());
                payload[14..22].copy_from_slice(&ts.to_be_bytes());
                out.clear();
                let h = DataHeader {
                    src_vip: p.me.vip,
                    src_port: 49152,
                    dst_vip: peer_vip,
                    dst_port: 49152,
                };
                wire::encode_data(&mut out, &p.me.auth, &h, &payload[..size]);
                let r = if p.direct {
                    p.me.send.send_to(&out, peer_addr)
                } else {
                    p.me.send.send(&out)
                };
                let counted = sh.in_window(ts);
                match r {
                    Ok(n) if counted => {
                        sh.sent.fetch_add(1, Relaxed);
                        sh.sent_bytes.fetch_add(n as u64, Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) if counted => {
                        sh.would_block.fetch_add(1, Relaxed);
                    }
                    Err(_) => {}
                }
            }
        }
        if !sent_any {
            std::hint::spin_loop();
        }
    }
}

async fn add_room(o: &Opts, first: u32, sh: &Arc<Shared>, inboxes: &[Inbox]) -> Result<(), String> {
    let code = room_code(o.secret.as_deref());
    let mut joins = Vec::new();
    for k in 0..o.members as u32 {
        let idx = first + k;
        let code = code.clone();
        let relay = o.relay;
        let direct = o.direct;
        joins.push(tokio::spawn(async move {
            if direct {
                direct_member(idx).await
            } else {
                join(relay.expect("relay"), &code, idx).await
            }
        }));
    }
    let mut members = Vec::new();
    for j in joins {
        members.push(Arc::new(j.await.map_err(|e| e.to_string())??));
    }
    let addrs: Vec<SocketAddr> = members.iter().map(|m| m.sock.local_addr().expect("bound")).collect();
    for (i, m) in members.iter().enumerate() {
        let peers: Vec<(u32, u32, SocketAddr)> = members
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, p)| (p.idx, p.vip, addrs[j]))
            .collect();
        tokio::spawn(receive(m.idx, m.sock.clone(), sh.clone()));
        let phase = Duration::from_nanos(random_u64() % (1e9 / o.pps) as u64);
        inboxes[m.idx as usize % inboxes.len()]
            .lock()
            .expect("inbox")
            .push(Paced {
                me: m.clone(),
                peers,
                direct: o.direct,
                next: Instant::now() + phase,
                seq: 0,
            });
    }
    Ok(())
}

fn main() {
    let o = match parse_opts() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("relay_bench: {e}");
            std::process::exit(2);
        }
    };
    EPOCH.get_or_init(Instant::now);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(o.threads)
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(run(o));
}

async fn run(o: Opts) {
    let sh = Arc::new(Shared {
        hist: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
        sent: AtomicU64::new(0),
        sent_bytes: AtomicU64::new(0),
        recv: AtomicU64::new(0),
        recv_bytes: AtomicU64::new(0),
        reordered: AtomicU64::new(0),
        would_block: AtomicU64::new(0),
        win_start: AtomicU64::new(u64::MAX),
        win_end: AtomicU64::new(u64::MAX),
        stop: AtomicBool::new(false),
    });
    let sizes = Arc::new(size_table(o.payload));
    let inboxes: Vec<Inbox> = (0..o.senders)
        .map(|_| Arc::new(std::sync::Mutex::new(Vec::new())))
        .collect();
    let mut senders = Vec::new();
    for (k, inbox) in inboxes.iter().enumerate() {
        let (inbox, sizes, sh, pps) = (inbox.clone(), sizes.clone(), sh.clone(), o.pps);
        senders.push(
            std::thread::Builder::new()
                .name(format!("sender-{k}"))
                .spawn(move || sender(inbox, pps, sizes, sh))
                .expect("sender thread"),
        );
    }
    let mean_size = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    println!(
        "relay_bench: {} members per room, {} datagrams/s to each peer, payload mean {:.0} bytes ({} bytes with the header), {}",
        o.members,
        o.pps,
        mean_size,
        mean_size as usize + wire::DATA_HEADER,
        match o.relay {
            Some(r) if !o.direct => format!("through {r}"),
            _ => "direct (no relay)".into(),
        }
    );
    let mut rooms = 0usize;
    let mut next_idx = 0u32;
    for &target in &o.rooms {
        let joined_at = Instant::now();
        while rooms < target {
            // one room at a time; its members join together
            let first = next_idx;
            next_idx += o.members as u32;
            if let Err(e) = add_room(&o, first, &sh, &inboxes).await {
                println!("relay_bench: a room could not join: {e}");
                sh.stop.store(true, Relaxed);
                return;
            }
            rooms += 1;
        }
        let join_s = joined_at.elapsed().as_secs_f64();
        tokio::time::sleep(Duration::from_secs_f64(o.warmup)).await;
        sh.reset();
        let start_unix = unix_ms();
        sh.win_start.store(now_ns(), Relaxed);
        sh.win_end.store(u64::MAX, Relaxed);
        tokio::time::sleep(Duration::from_secs_f64(o.secs)).await;
        sh.win_end.store(now_ns(), Relaxed);
        let end_unix = unix_ms();
        // late datagrams of the window
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let sent = sh.sent.load(Relaxed);
        let recv = sh.recv.load(Relaxed);
        let loss = if sent == 0 {
            0.0
        } else {
            100.0 * (sent.saturating_sub(recv)) as f64 / sent as f64
        };
        let secs = o.secs;
        let in_pps = sent as f64 / secs;
        let out_pps = recv as f64 / secs;
        let in_mbit = (sh.sent_bytes.load(Relaxed) + sent * IP_UDP) as f64 * 8.0 / secs / 1e6;
        let out_mbit = (sh.recv_bytes.load(Relaxed) + recv * IP_UDP) as f64 * 8.0 / secs / 1e6;
        let members = rooms * o.members;
        println!(
            "rooms {rooms:5} members {members:6} | sent {in_pps:9.0}/s received {out_pps:9.0}/s ({in_mbit:7.1} / {out_mbit:7.1} Mbit/s) | loss {loss:6.3} % reordered {} would_block {} | latency us p50 {:.0} p99 {:.0} p99.9 {:.0} max {:.0} | joins {join_s:.1} s",
            sh.reordered.load(Relaxed),
            sh.would_block.load(Relaxed),
            sh.percentile(0.5),
            sh.percentile(0.99),
            sh.percentile(0.999),
            sh.max_us(),
        );
        println!(
            "RESULT rooms={rooms} members={members} window_start_ms={start_unix} window_end_ms={end_unix} sent={sent} received={recv} loss_pct={loss:.4} reordered={} would_block={} in_pps={in_pps:.0} out_pps={out_pps:.0} in_mbit={in_mbit:.2} out_mbit={out_mbit:.2} p50_us={:.0} p99_us={:.0} p999_us={:.0} max_us={:.0}",
            sh.reordered.load(Relaxed),
            sh.would_block.load(Relaxed),
            sh.percentile(0.5),
            sh.percentile(0.99),
            sh.percentile(0.999),
            sh.max_us(),
        );
        if loss > o.stop_loss {
            println!("relay_bench: loss above {} %, stopping", o.stop_loss);
            break;
        }
    }
    sh.stop.store(true, Relaxed);
    for t in senders {
        let _ = t.join();
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
}
