//! Measures how fast a relay answers: one `HELLO` every `1/rate` seconds for `seconds` seconds, each timed against
//! its `CHALLENGE` (nothing is joined, the relay keeps no state), then the percentiles.
//!
//!   cargo run --release -p kalamar-relay --example hello_latency -- 127.0.0.1:47313 --rate 10 --seconds 120
//!
//! Stay below the relay's HELLO limit per address (`--hello-rate`, 20 per second by default).

use std::net::SocketAddr;
use std::time::Duration;

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(addr) = args.first().and_then(|a| a.parse::<SocketAddr>().ok()) else {
        eprintln!("usage: hello_latency <ip:port> [--rate N per second] [--seconds S]");
        std::process::exit(2);
    };
    let rate: f64 = arg(&args, "--rate", 10.0);
    let seconds: f64 = arg(&args, "--seconds", 60.0);
    let every = Duration::from_secs_f64(1.0 / rate.clamp(0.1, 1000.0));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let until = tokio::time::Instant::now() + Duration::from_secs_f64(seconds);
    let (mut sent, mut rtts) = (0usize, Vec::new());
    while tokio::time::Instant::now() < until {
        tick.tick().await;
        sent += 1;
        if let Some(d) = kalamar_relay::client::hello_rtt(addr, Duration::from_secs(1)).await {
            rtts.push(d.as_secs_f64() * 1000.0);
        }
    }
    rtts.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        if rtts.is_empty() {
            f64::NAN
        } else {
            rtts[((rtts.len() as f64 - 1.0) * p).round() as usize]
        }
    };
    let mean = rtts.iter().sum::<f64>() / rtts.len().max(1) as f64;
    println!(
        "relay {addr}: sent {sent}, answered {}, lost {}; ms: mean {mean:.3} p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3}; over 5 ms: {}, over 20 ms: {}",
        rtts.len(),
        sent - rtts.len(),
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(0.999),
        rtts.last().copied().unwrap_or(f64::NAN),
        rtts.iter().filter(|&&x| x > 5.0).count(),
        rtts.iter().filter(|&&x| x > 20.0).count(),
    );
}
