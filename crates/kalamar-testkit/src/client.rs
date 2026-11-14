//! A headless launcher's network side: the HTTPS API with bearer tokens (retrying on 429 as the protocol asks) and
//! the event WebSocket (heartbeat answered, events buffered so a test can wait for one).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// An HTTP answer: status and JSON body (`Null` when empty or not JSON).
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<u64>,
    /// The `Cache-Control` header, when there is one.
    pub cache_control: Option<String>,
}

impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    /// The protocol error code, if the body has the error shape.
    pub fn code(&self) -> Option<&str> {
        self.body.get("error")?.get("code")?.as_str()
    }
    /// Whether the body has the protocol's error shape (`error.code` and `error.message` strings).
    pub fn has_error_shape(&self) -> bool {
        let e = &self.body["error"];
        e["code"].is_string() && e["message"].is_string()
    }
}

/// Installs the TLS crypto provider once (ring).
pub fn install_crypto() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Clone)]
pub struct Api {
    pub http: reqwest::Client,
    /// The API base URL (`<api>` of the specification), without a trailing slash.
    pub base: String,
    pub token: Option<String>,
}

impl Api {
    pub fn new(base: &str) -> Api {
        install_crypto();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("kalamar-testkit/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client");
        Api {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: None,
        }
    }

    pub fn with_token(&self, token: &str) -> Api {
        Api {
            token: Some(token.to_string()),
            ..self.clone()
        }
    }

    /// Sends a request with a JSON body (or none) and reads the answer; waits and retries on 429 (at most 4 times).
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Resp {
        self.call_raw(method, path, body.map(|b| b.to_string().into_bytes()))
            .await
    }

    pub async fn call_raw(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Resp {
        self.send(method, path, body, 5).await
    }

    /// One request, even when it is answered `429` (to check a limit).
    pub async fn call_once(&self, method: &str, path: &str, body: Option<Value>) -> Resp {
        self.send(method, path, body.map(|b| b.to_string().into_bytes()), 1)
            .await
    }

    async fn send(&self, method: &str, path: &str, body: Option<Vec<u8>>, tries: u32) -> Resp {
        let url = format!("{}{}", self.base, path);
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
        for attempt in 0..tries {
            let mut rb = self
                .http
                .request(m.clone(), &url)
                .header("kalamar-protocol", kalamar_protocol::PROTOCOL_VERSION);
            if let Some(t) = &self.token {
                rb = rb.bearer_auth(t);
            }
            if let Some(b) = &body {
                rb = rb.header("content-type", "application/json").body(b.clone());
            }
            let resp = match rb.send().await {
                Ok(r) => r,
                Err(e) => {
                    return Resp {
                        status: 0,
                        body: json!({"transport_error": e.to_string()}),
                        retry_after: None,
                        cache_control: None,
                    };
                }
            };
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let cache_control = resp
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = resp.bytes().await.unwrap_or_default();
            let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            if status == 429 && attempt + 1 < tries {
                tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(1).clamp(1, 10))).await;
                continue;
            }
            return Resp {
                status,
                body,
                retry_after,
                cache_control,
            };
        }
        unreachable!("the loop returns")
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.call("GET", path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> Resp {
        self.call("POST", path, Some(body)).await
    }
    pub async fn post_empty(&self, path: &str) -> Resp {
        self.call("POST", path, None).await
    }
    pub async fn put(&self, path: &str, body: Value) -> Resp {
        self.call("PUT", path, Some(body)).await
    }
    pub async fn patch(&self, path: &str, body: Value) -> Resp {
        self.call("PATCH", path, Some(body)).await
    }
    pub async fn delete(&self, path: &str) -> Resp {
        self.call("DELETE", path, None).await
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// One event WebSocket.
pub struct Events {
    rx: mpsc::UnboundedReceiver<Value>,
    out: mpsc::UnboundedSender<Message>,
    pending: Vec<Value>,
    pub hello: Value,
    pub last_seq: u64,
    task: tokio::task::JoinHandle<()>,
}

/// How to authenticate the WebSocket.
pub enum WsAuth<'a> {
    Bearer(&'a str),
    Ticket(&'a str),
}

impl Events {
    /// Connects to `<base>/v1/events` (plus `resume` query when given). Returns the HTTP status on refusal.
    pub async fn connect(base: &str, auth: WsAuth<'_>, resume: Option<(&str, u64)>) -> Result<Events, String> {
        install_crypto();
        let mut url = format!("{}/v1/events", base.trim_end_matches('/'));
        let mut q = Vec::new();
        if let WsAuth::Ticket(t) = &auth {
            q.push(format!("ticket={t}"));
        }
        if let Some((s, n)) = resume {
            q.push(format!("resume={s}&last_seq={n}"));
        }
        if !q.is_empty() {
            url = format!("{url}?{}", q.join("&"));
        }
        let tls = url.starts_with("https://");
        let ws_url = url.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        let mut req = ws_url.as_str().into_client_request().map_err(|e| e.to_string())?;
        if let WsAuth::Bearer(t) = auth {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().map_err(|_| "bad token")?);
        }
        let host = req.uri().host().ok_or("no host")?.trim_matches(['[', ']']).to_string();
        let port = req.uri().port_u16().unwrap_or(if tls { 443 } else { 80 });
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| e.to_string())?;
        let io: Box<dyn Io> = if tls {
            use rustls_platform_verifier::ConfigVerifierExt;
            let cfg = rustls::ClientConfig::with_platform_verifier().map_err(|e| e.to_string())?;
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
            let s = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(name, tcp)
                .await
                .map_err(|e| e.to_string())?;
            Box::new(s)
        } else {
            Box::new(tcp)
        };
        let (ws, _) = tokio_tungstenite::client_async(req, io).await.map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => format!("http {}", r.status().as_u16()),
            other => other.to_string(),
        })?;
        let (mut sink, mut source) = ws.split();
        let (tx, rx) = mpsc::unbounded_channel::<Value>();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    m = source.next() => {
                        let Some(Ok(m)) = m else { break };
                        if let Message::Text(t) = m {
                            let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                            if v["type"] == "ping" && v.get("seq").is_none() {
                                let pong = json!({"type": "pong", "t": v["t"]}).to_string();
                                let _ = sink.send(Message::Text(pong.into())).await;
                            }
                            if tx.send(v).is_err() { break; }
                        }
                    }
                    o = out_rx.recv() => {
                        match o {
                            Some(Message::Close(_)) | None => { let _ = sink.close().await; break; }
                            Some(m) => { if sink.send(m).await.is_err() { break; } }
                        }
                    }
                }
            }
        });
        let mut ev = Events {
            rx,
            out,
            pending: Vec::new(),
            hello: Value::Null,
            last_seq: 0,
            task,
        };
        let hello = ev.next(Duration::from_secs(10)).await.ok_or("no hello")?;
        if hello["type"] != "hello" {
            return Err(format!("first frame is not hello: {hello}"));
        }
        ev.hello = hello;
        Ok(ev)
    }

    async fn next(&mut self, timeout: Duration) -> Option<Value> {
        let v = tokio::time::timeout(timeout, self.rx.recv()).await.ok().flatten()?;
        if let Some(s) = v.get("seq").and_then(Value::as_u64) {
            self.last_seq = self.last_seq.max(s);
        }
        Some(v)
    }

    /// Waits for a frame of `kind` matching `pred` (frames already received count).
    pub async fn wait(&mut self, kind: &str, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Option<Value> {
        self.wait_any(&[kind], pred, timeout).await
    }

    /// Waits for a frame of one of `kinds` matching `pred` (frames already received count).
    pub async fn wait_any(
        &mut self,
        kinds: &[&str],
        pred: impl Fn(&Value) -> bool,
        timeout: Duration,
    ) -> Option<Value> {
        let hit = |e: &Value| kinds.iter().any(|k| e["type"] == *k) && pred(e);
        if let Some(i) = self.pending.iter().position(hit) {
            return Some(self.pending.remove(i));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let v = self.next(left).await?;
            if hit(&v) {
                return Some(v);
            }
            self.pending.push(v);
            if self.pending.len() > 2000 {
                self.pending.drain(..1000);
            }
        }
    }

    /// Frames received so far that nobody waited for (and the ones arriving within `settle`).
    pub async fn drain(&mut self, settle: Duration) -> Vec<Value> {
        while let Some(v) = self.next(settle).await {
            self.pending.push(v);
        }
        std::mem::take(&mut self.pending)
    }

    pub fn send(&self, v: Value) {
        let _ = self.out.send(Message::Text(v.to_string().into()));
    }

    /// Closes the connection (the server keeps the stream for resumption).
    pub async fn close(self) {
        let _ = self.out.send(Message::Close(None));
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }

    pub fn stream_id(&self) -> String {
        self.hello["stream"].as_str().unwrap_or_default().to_string()
    }
}
