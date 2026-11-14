//! Outgoing webhooks (protocol 1.1, `operators.md` section 1): events POSTed to the operator's endpoints, signed
//! `Kalamar-Signature: t=<unix seconds>,v1=<hex HMAC-SHA-256(secret, "<t>." + body)>`, retried with backoff.
//! The reference server keeps the queue in memory and retries 5 times over about 10 minutes.

use std::time::Duration;

use hmac::{KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::sync::mpsc;

use crate::config::{Config, WebhookEntry};
use crate::state::Core;
use crate::util::{hex, new_id, rfc3339};

/// Event types a server sends.
pub const EVENTS: &[&str] = &[
    "match.completed",
    "match.void",
    "match.drafted",
    "series.completed",
    "activity.updated",
    "report.created",
    "user.banned",
];

/// The signature header's value.
pub fn sign(secret: &[u8], t: i64, body: &str) -> String {
    let mut m = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    m.update(format!("{t}.").as_bytes());
    m.update(body.as_bytes());
    format!("t={t},v1={}", hex(&m.finalize().into_bytes()))
}

impl Core {
    /// Queues a webhook event (nothing when no webhook is configured).
    pub fn emit(&self, kind: &str, data: Value) {
        if let Some(tx) = &self.live.webhooks {
            let _ = tx.send((kind.to_string(), data));
        }
    }
}

fn secret_of(w: &WebhookEntry) -> Option<Vec<u8>> {
    match (&w.secret, &w.secret_file) {
        (Some(s), _) => Some(s.as_bytes().to_vec()),
        (None, Some(f)) => std::fs::read_to_string(f).ok().map(|s| s.trim().as_bytes().to_vec()),
        _ => None,
    }
}

/// Starts the delivery task; returns the sender the core emits into, or `None` without webhooks.
pub fn spawn(
    cfg: &Config,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
) -> Option<mpsc::UnboundedSender<(String, Value)>> {
    if cfg.webhooks.is_empty() {
        return None;
    }
    crate::auth::oauth::install_crypto();
    let hooks: Vec<(WebhookEntry, Vec<u8>)> = cfg
        .webhooks
        .iter()
        .filter_map(|w| match secret_of(w) {
            Some(s) => Some((w.clone(), s)),
            None => {
                tracing::error!(url = %w.url, "webhook left out: its secret could not be read");
                None
            }
        })
        .collect();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("kalamar-service/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, Value)>();
    tokio::spawn(async move {
        while let Some((kind, data)) = rx.recv().await {
            let now = clock.now();
            let body = json!({"id": new_id("wh"), "type": kind, "at": rfc3339(now), "data": data}).to_string();
            for (w, secret) in &hooks {
                if !w.events.is_empty() && !w.events.iter().any(|e| e == &kind) {
                    continue;
                }
                let (http, url, body, sig) = (http.clone(), w.url.clone(), body.clone(), sign(secret, now, &body));
                tokio::spawn(async move {
                    for (attempt, wait) in [0u64, 5, 30, 120, 450].iter().enumerate() {
                        tokio::time::sleep(Duration::from_secs(*wait)).await;
                        let r = http
                            .post(&url)
                            .header("content-type", "application/json")
                            .header("kalamar-signature", &sig)
                            .body(body.clone())
                            .send()
                            .await;
                        match r {
                            Ok(resp) if resp.status().is_success() => return,
                            _ => tracing::warn!(attempt, "webhook delivery failed"),
                        }
                    }
                });
            }
        }
    });
    Some(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_hmac_over_time_and_body() {
        let s = sign(b"secret", 1_800_000_000, "{}");
        assert!(s.starts_with("t=1800000000,v1="));
        assert_eq!(s.len(), "t=1800000000,v1=".len() + 64);
        assert_ne!(s, sign(b"other", 1_800_000_000, "{}"));
        assert_ne!(s, sign(b"secret", 1_800_000_001, "{}"));
    }
}
