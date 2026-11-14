//! Helpers of the login tests: the service on 127.0.0.1 behind a "proxy" (so a test chooses each request's client
//! address with `X-Forwarded-For`), and a fake SMTP relay on 127.0.0.1 that keeps what it receives. Nothing leaves
//! the machine.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A configuration with a guest login (the protocol forbids requiring an e-mail address), plain HTTP on loopback,
/// generous request limits, and 127.0.0.1 as the trusted proxy.
pub const BASE: &str = r#"
[server]
name = "Logins"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
trusted_proxies = ["127.0.0.1/32"]
operator = { name = "T", contact = "mailto:op@example.invalid" }
[features]
test_clock = true
[auth.guest]
enabled = true
[[relays]]
host = "127.0.0.1"
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
accounts_per_ip_per_day = 0.0
room_starts_per_hour = 0.0
[[ladders]]
id = "duel"
name = { en = "Duel" }
[[queues]]
id = "duel"
name = { en = "Duel" }
teams = [1, 1]
pool = [{ mode = "turf" }]
result_policy = "standard"
"#;

pub struct Server {
    pub running: kalamar_service::app::Running,
    pub base: String,
    pub http: reqwest::Client,
}

pub struct Resp {
    pub status: u16,
    pub body: Value,
}

impl Resp {
    pub fn code(&self) -> Option<&str> {
        self.body["error"]["code"].as_str()
    }
}

pub fn config(doc: toml::Table, port: u16) -> Config {
    let mut doc = doc;
    doc["server"]
        .as_table_mut()
        .expect("server")
        .insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    toml::Value::Table(doc).try_into().expect("configuration")
}

pub async fn start(doc: toml::Table) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let cfg = config(doc, port);
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::new(1_800_000_000))).expect("state");
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .expect("start");
    let base = running.base_url();
    kalamar_service::auth::oauth::install_crypto();
    Server {
        running,
        base,
        http: reqwest::Client::new(),
    }
}

impl Server {
    /// A request from client address `ip` (`X-Forwarded-For` through the trusted proxy).
    pub async fn call(&self, ip: &str, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> Resp {
        self.call_lang(ip, method, path, token, body, None).await
    }

    pub async fn call_lang(
        &self,
        ip: &str,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        lang: Option<&str>,
    ) -> Resp {
        let mut rb = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
                format!("{}{path}", self.base),
            )
            .header("x-forwarded-for", ip);
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(l) = lang {
            rb = rb.header("accept-language", l);
        }
        if let Some(b) = body {
            rb = rb.header("content-type", "application/json").body(b.to_string());
        }
        let r = rb.send().await.expect("request");
        let status = r.status().as_u16();
        let body = r.json::<Value>().await.unwrap_or(Value::Null);
        Resp { status, body }
    }

    pub async fn post(&self, ip: &str, path: &str, body: &Value) -> Resp {
        self.call(ip, "POST", path, None, Some(body)).await
    }

    pub async fn meta(&self) -> Value {
        self.call("192.0.2.1", "GET", "/v1/meta", None, None).await.body
    }
}

/// One message the fake relay received.
#[derive(Debug, Clone)]
pub struct Mail {
    pub rcpt: Vec<String>,
    pub data: String,
}

#[derive(Clone, Default)]
pub struct Inbox(pub Arc<Mutex<Vec<Mail>>>);

impl Inbox {
    pub fn all(&self) -> Vec<Mail> {
        self.0.lock().expect("inbox").clone()
    }

    /// Waits for the `n`-th message (1-based) to arrive.
    pub async fn wait(&self, n: usize) -> Mail {
        for _ in 0..100 {
            if let Some(m) = self.all().get(n - 1) {
                return m.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no mail number {n}; got {:?}", self.all());
    }

    /// Nothing more than `n` messages arrives within a short while.
    pub async fn stays(&self, n: usize) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(self.all().len(), n, "{:?}", self.all());
    }
}

/// The `XXXXX-XXXXX` code of a mail.
pub fn code_in(m: &Mail) -> String {
    let b = m.data.as_bytes();
    for (i, w) in b.windows(11).enumerate() {
        let shape = w[5] == b'-'
            && w.iter()
                .enumerate()
                .all(|(k, c)| k == 5 || c.is_ascii_digit() || c.is_ascii_uppercase());
        let alone =
            (i == 0 || !b[i - 1].is_ascii_alphanumeric()) && b.get(i + 11).is_none_or(|c| !c.is_ascii_alphanumeric());
        if shape && alone {
            return String::from_utf8_lossy(w).into_owned();
        }
    }
    panic!("no code in {}", m.data);
}

/// A fake SMTP relay (plain text, 127.0.0.1): accepts every message.
pub async fn fake_smtp() -> (u16, Inbox) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let inbox = Inbox::default();
    let ib = inbox.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = l.accept().await {
            let ib = ib.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                let _ = w.write_all(b"220 relay.test ESMTP fake\r\n").await;
                let mut rcpt = Vec::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim_end().to_string();
                    let up = cmd.to_ascii_uppercase();
                    let answer: &[u8] = if up.starts_with("EHLO") || up.starts_with("HELO") {
                        b"250-relay.test\r\n250 8BITMIME\r\n"
                    } else if up.starts_with("MAIL FROM") || up == "RSET" || up == "NOOP" {
                        rcpt.clear();
                        b"250 OK\r\n"
                    } else if up.starts_with("RCPT TO:") {
                        rcpt.push(cmd[8..].trim().trim_matches(['<', '>']).to_string());
                        b"250 OK\r\n"
                    } else if up == "DATA" {
                        let _ = w.write_all(b"354 go on\r\n").await;
                        let mut data = String::new();
                        loop {
                            line.clear();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == ".\r\n" {
                                break;
                            }
                            data.push_str(&line);
                        }
                        ib.0.lock().expect("inbox").push(Mail {
                            rcpt: std::mem::take(&mut rcpt),
                            data,
                        });
                        b"250 queued\r\n"
                    } else if up == "QUIT" {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        b"502 unknown\r\n"
                    };
                    if w.write_all(answer).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, inbox)
}

/// A temporary file holding `content` (a client secret), removed at the end of the process's tests by the OS temp
/// cleaner; the path is unique.
pub fn secret_file(content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kalamar-test-{}", kalamar_service::util::new_id("s")));
    std::fs::create_dir_all(&dir).expect("dir");
    let p = dir.join("secret");
    std::fs::write(&p, content).expect("write");
    p
}
