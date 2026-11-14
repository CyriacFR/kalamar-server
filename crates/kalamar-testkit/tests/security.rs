//! Regression tests of the security review (`docs/security-model.md`) that need a WebSocket client or a
//! raw TCP connection: each failed before its fix. Every connection goes to 127.0.0.1.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const CFG: &str = r#"
[server]
name = "Flood"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
queues = false
ratings = false
leaderboards = false
parties = false
stats = false
[auth.guest]
enabled = true
[[relays]]
host = "127.0.0.1"
[rate_limits]
per_ip_rps = 100000.0
per_ip_burst = 100000.0
per_user_rps = 100000.0
per_user_burst = 100000.0
"#;

async fn serve(extra: &str) -> (kalamar_service::app::Running, std::net::SocketAddr) {
    let mut doc: toml::Table = format!("{CFG}{extra}").parse().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    doc["server"]
        .as_table_mut()
        .unwrap()
        .insert("api_url".into(), format!("http://{addr}/api").into());
    let cfg: Config = toml::Value::Table(doc).try_into().unwrap();
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::new(1_800_000_000))).unwrap();
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .unwrap();
    (running, addr)
}

/// L-S1. Frames on the event WebSocket bypassed every rate limit, each `ack` taking the service's lock (one connection
/// sent 44 772 a second). A connection that floods is now closed (1008) after its budget.
#[tokio::test]
async fn a_flood_of_frames_on_the_event_stream_closes_it() {
    let (running, addr) = serve("").await;
    let http = reqwest::Client::new();
    let v: Value = http
        .post(format!("{}/v1/auth/guest/login", running.base_url()))
        .json(&json!({"display_name": "flood"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = v["access_token"].as_str().unwrap().to_string();
    let mut req = format!("ws://{addr}/api/v1/events").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let io = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (ws, _) = tokio_tungstenite::client_async(req, io).await.unwrap();
    let (mut sink, mut source) = ws.split();
    let writer = tokio::spawn(async move {
        let frame = json!({"type": "ack", "seq": 1}).to_string();
        let mut sent = 0u64;
        while sink.send(Message::Text(frame.clone().into())).await.is_ok() && sent < 100_000 {
            sent += 1;
        }
        sent
    });
    // The server closes the connection (with 1008, which the client may or may not read while it writes).
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(m) = source.next().await {
            match m {
                Ok(Message::Close(c)) => return c.map(|c| u16::from(c.code)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    })
    .await;
    let sent = writer.await.unwrap_or(0);
    assert!(
        matches!(closed, Ok(None | Some(1008))),
        "the flooding connection stays open ({closed:?}, {sent} frames sent)"
    );
    assert!(sent < 100_000, "{sent} frames accepted");
    running.stop();
}

/// L-S2. Without a reverse proxy in front, a client that sends its request headers slowly held a connection of the
/// service for ever (slowloris). The headers must now arrive within `limits.header_timeout_s`.
#[tokio::test]
async fn a_client_that_never_finishes_its_headers_is_disconnected() {
    let (running, addr) = serve("").await;
    let started = Instant::now();
    let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
    c.write_all(b"GET /api/v1/meta HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 256];
    let r = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // A header line every 2 s, never the end of the headers.
            tokio::select! {
                n = c.read(&mut buf) => return n.map(|n| n == 0 || String::from_utf8_lossy(&buf[..n]).contains("408")),
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    if c.write_all(b"X-Slow: 1\r\n").await.is_err() {
                        return Ok(true);
                    }
                }
            }
        }
    })
    .await;
    assert!(
        matches!(r, Ok(Ok(true))),
        "still connected after {:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(25));
    running.stop();
}
