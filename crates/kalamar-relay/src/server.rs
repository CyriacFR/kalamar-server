//! The relay's sockets and threads around [`Relay`]. `workers` threads each wait on a blocking receive and handle
//! what they get to the end ([`Relay::handle_on`], then the sends); the async task of [`serve`] does the periodic
//! work once a second and the statistics, and stops the workers at the shutdown.
//!
//! Sockets ([`bind`]): on Linux each worker has a socket of its own on the same address (`SO_REUSEPORT`): the kernel
//! hands each client's datagrams to one of them, always the same while the client's address and port stay the same,
//! so a member's datagrams are handled in the order they came. Elsewhere (Windows) the workers share one socket and
//! take its datagrams in turn: two datagrams of one member a few microseconds apart may then be handled by two
//! workers and leave in the other order, which UDP allows and the game's transport accepts. With one worker there
//! is one thread, and every datagram is handled in the order it came.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{debug, error, info, warn};

use crate::cookie::CookieKeys;
use crate::relay::{DropReason, Outbox, Relay, RelayConfig, Stats};
use crate::status::Load;

/// How long a worker waits for a datagram before it looks at the stop flag.
const POLL: Duration = Duration::from_millis(200);

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The number of workers `0` (auto) means: the processors this program may use.
pub fn auto_workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn flush(socket: &std::net::UdpSocket, out: &mut Outbox) -> u64 {
    let mut failed = 0;
    for (to, data) in out.iter() {
        if let Err(e) = socket.send_to(data, to) {
            failed += 1;
            debug!(error = %e, "send failed");
        }
    }
    out.clear();
    failed
}

/// One line with the counters (never payloads, never addresses).
pub fn stats_line(s: &Stats, sessions: usize, rooms: usize) -> String {
    let mut drops = String::new();
    for r in DropReason::ALL {
        let n = s.dropped[r as usize];
        if n > 0 {
            drops.push_str(&format!(" {}={}", r.name(), n));
        }
    }
    format!(
        "rx={} tx={} forwarded={} joins={} leaves={} timeouts={} refused={} dropped={} sessions={} rooms={}{}",
        s.rx,
        s.tx,
        s.forwarded,
        s.joins,
        s.leaves,
        s.timeouts,
        s.refused,
        s.dropped_total(),
        sessions,
        rooms,
        if drops.is_empty() {
            String::new()
        } else {
            format!(" drops:{drops}")
        },
    )
}

/// The load of the last seconds, after [`stats_line`]: rates and each worker's share of time spent working.
pub fn load_line(l: &Load) -> String {
    let busy: Vec<String> = l.worker_busy.iter().map(|b| b.to_string()).collect();
    format!(
        "rx_pps={} tx_pps={} rx_kbit_s={} tx_kbit_s={} workers={} load_pct={}",
        l.rx_pps,
        l.tx_pps,
        l.rx_bps * 8 / 1000,
        l.tx_bps * 8 / 1000,
        l.workers,
        busy.join(","),
    )
}

/// The receive buffer the relay asks for each socket, in KiB (`--recv-buffer-kb`): some 40 ms of one saturated
/// worker's traffic (measured: 1300 datagrams of a match), so that a worker the system did not run for a moment
/// (another program, a page fault) loses nothing, without holding datagrams for long when the relay is overloaded (a
/// datagram that waits 600 ms in a 4 MiB buffer is worth no more to a game than a lost one). The system may give
/// less (Linux: `net.core.rmem_max`, 208 KiB by default).
pub const DEFAULT_RECV_BUFFER_KB: usize = 256;

/// [`bind_with`] and the default receive buffer.
pub fn bind(addr: SocketAddr, workers: usize) -> io::Result<Vec<std::net::UdpSocket>> {
    bind_with(addr, workers, DEFAULT_RECV_BUFFER_KB)
}

/// The sockets of a relay of `workers` workers on `addr`: on Linux one per worker (`SO_REUSEPORT`; one socket when
/// the option is refused), elsewhere one that the workers share. A port of 0 picks one port for all of them.
/// `recv_buffer_kb`: the receive buffer asked for (0: the system's default).
pub fn bind_with(addr: SocketAddr, workers: usize, recv_buffer_kb: usize) -> io::Result<Vec<std::net::UdpSocket>> {
    if cfg!(any(target_os = "linux", target_os = "android")) && workers > 1 {
        match bind_shared_port(addr, workers, recv_buffer_kb) {
            Ok(v) => return Ok(v),
            Err(e) => warn!(error = %e, "SO_REUSEPORT refused: the workers share one socket"),
        }
    }
    let s = new_socket(addr, recv_buffer_kb)?;
    s.bind(&addr.into())?;
    Ok(vec![s.into()])
}

fn new_socket(addr: SocketAddr, recv_buffer_kb: usize) -> io::Result<socket2::Socket> {
    let s = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if recv_buffer_kb > 0 {
        if let Err(e) = s.set_recv_buffer_size(recv_buffer_kb.saturating_mul(1024)) {
            warn!(error = %e, "the receive buffer was refused: the system's default stays");
        }
    }
    Ok(s)
}

/// `n` sockets on one address and port, the kernel spreading the clients between them (Linux, `SO_REUSEPORT`).
fn bind_shared_port(addr: SocketAddr, n: usize, recv_buffer_kb: usize) -> io::Result<Vec<std::net::UdpSocket>> {
    let mut at = addr;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let s = new_socket(at, recv_buffer_kb)?;
        reuse_port(&s)?;
        s.bind(&at.into())?;
        if at.port() == 0 {
            at = s
                .local_addr()?
                .as_socket()
                .ok_or_else(|| io::Error::other("not an IP socket"))?;
        }
        out.push(s.into());
    }
    Ok(out)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn reuse_port(s: &socket2::Socket) -> io::Result<()> {
    s.set_reuse_port(true)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn reuse_port(_: &socket2::Socket) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SO_REUSEPORT spreads datagrams on Linux only",
    ))
}

/// Serves on `socket` with one worker until `shutdown` completes, then sends CLOSED (reason 4) to every member and
/// returns the final counters (one thread; what the tests use).
pub async fn run(
    socket: tokio::net::UdpSocket,
    cfg: RelayConfig,
    stats_every: Duration,
    shutdown: impl Future<Output = ()>,
) -> io::Result<Stats> {
    let s = socket.into_std()?;
    run_on(vec![s], 1, cfg, stats_every, shutdown).await
}

/// Serves on `sockets` ([`bind`]) with `workers` workers (at least one).
pub async fn run_on(
    sockets: Vec<std::net::UdpSocket>,
    workers: usize,
    cfg: RelayConfig,
    stats_every: Duration,
    shutdown: impl Future<Output = ()>,
) -> io::Result<Stats> {
    let now = Instant::now();
    let cookies = CookieKeys::new(now, cfg.cookie_rotation);
    let relay = Arc::new(Relay::with_workers(cfg, cookies, now, workers.max(1)));
    serve(relay, sockets, stats_every, shutdown).await
}

/// Tells the server task that a worker ended (also when it panicked).
struct Ended(usize, tokio::sync::mpsc::UnboundedSender<usize>);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.1.send(self.0);
    }
}

fn worker(w: usize, relay: Arc<Relay>, sock: std::net::UdpSocket, stop: Arc<AtomicBool>, ended: Ended) {
    let _ended = ended;
    let mut out = Outbox::new();
    // one byte more than the largest legal datagram, so an oversized one is seen as oversized, not truncated
    let mut buf = [0u8; 2048];
    while !stop.load(Relaxed) {
        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                let t0 = Instant::now();
                relay.handle_on(w, t0, unix_now(), src, &buf[..n], &mut out);
                flush(&sock, &mut out);
                relay.add_busy(w, t0.elapsed().as_nanos() as u64);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            // Windows reports an earlier ICMP "port unreachable" as a receive error; it says nothing about
            // the datagram we are waiting for.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => {
                warn!(error = %e, worker = w, "receive failed");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Serves `relay` on `sockets` with one thread per worker of the relay until `shutdown` completes, then sends CLOSED
/// (reason 4) to every member and returns the final counters. With fewer sockets than workers, the workers share the
/// first one. Fails when a worker stops by itself (a bug: the relay then stops, and its service manager restarts it).
pub async fn serve(
    relay: Arc<Relay>,
    sockets: Vec<std::net::UdpSocket>,
    stats_every: Duration,
    shutdown: impl Future<Output = ()>,
) -> io::Result<Stats> {
    let first = sockets
        .first()
        .ok_or_else(|| io::Error::other("no socket"))?
        .try_clone()?;
    first.set_nonblocking(false)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (ended_tx, mut ended_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut threads = Vec::new();
    for w in 0..relay.workers() {
        let s = sockets.get(w).unwrap_or(&sockets[0]).try_clone()?;
        s.set_nonblocking(false)?;
        s.set_read_timeout(Some(POLL))?;
        let (r, st, e) = (relay.clone(), stop.clone(), Ended(w, ended_tx.clone()));
        threads.push(
            std::thread::Builder::new()
                .name(format!("relay-worker-{w}"))
                .spawn(move || worker(w, r, s, st, e))?,
        );
    }
    drop(ended_tx);
    let mut out = Outbox::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stats_tick = tokio::time::interval(if stats_every.is_zero() {
        Duration::from_secs(3600)
    } else {
        stats_every
    });
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    stats_tick.reset();
    let mut last_rx = 0;
    let mut failed = None;
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                relay.tick(Instant::now(), &mut out);
                flush(&first, &mut out);
            }
            _ = stats_tick.tick(), if !stats_every.is_zero() => {
                let s = relay.stats();
                let (sessions, rooms) = (relay.session_count(), relay.room_count());
                if s.rx != last_rx || sessions > 0 {
                    info!("stats {} {}", stats_line(&s, sessions, rooms), load_line(&relay.load(Instant::now())));
                }
                last_rx = s.rx;
            }
            Some(w) = ended_rx.recv() => {
                error!(worker = w, "a relay worker stopped: the relay stops");
                failed = Some(w);
                break;
            }
            _ = &mut shutdown => break,
        }
    }
    stop.store(true, Relaxed);
    // every worker sees the flag within POLL
    while ended_rx.recv().await.is_some() {}
    for t in threads {
        let _ = t.join();
    }
    relay.shutdown(&mut out);
    flush(&first, &mut out);
    let s = relay.stats();
    info!("final stats {}", stats_line(&s, 0, 0));
    match failed {
        Some(w) => Err(io::Error::other(format!("relay worker {w} stopped"))),
        None => Ok(s),
    }
}

/// Resolves on Ctrl-C, and on SIGTERM where the platform has it.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
