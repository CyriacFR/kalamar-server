//! E-mail and password accounts (`[auth.password]`), offered as `direct` login providers whose forms any launcher
//! draws from their declared fields (`api.md`, "Password accounts"):
//!
//! | provider | form | answer |
//! |---|---|---|
//! | `password` | address, password | a session; `403 wrong_password` whether or not the address has an account |
//! | `password-signup` | address, password, display name (+ `code` with e-mail verification) | a new account's session |
//! | `password-reset` | address, code, new password | a session; every other session ends |
//! | `password-change` | address, current and new password | a session; every other session ends |
//!
//! and `POST /v1/me/password` (current and new password) for a logged-in player.
//!
//! What is stored: the keyed hash of the normalized address (the `identities` row of provider `password`, like any
//! login identity: the address itself never), an Argon2id PHC string (per-account random salt, parameters from the
//! configuration), and keyed hashes of one-time codes. Every mail answers a form where the player has just typed the
//! address, so the server never needs it afterwards.
//!
//! Defenses: Argon2id with OWASP parameters, computed outside the state lock on the blocking pool with a bounded
//! number at once; a fixed dummy hash is verified when the address has no account, so both cost the same and answer
//! the same; failures spend tokens of per-address and per-account buckets (memory only), and an account's bucket
//! never stops an address that account logged in from; codes are single use, expiring, limited to 5 tries and
//! hashed; a reset or a change ends every other session.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use argon2::{ARGON2ID_IDENT, Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use hmac::{KeyInit, Mac};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::auth::mail::{self, Mailer, Outgoing};
use crate::auth::{Authed, check_display_name, create_account, find_identity, ip_of, issue_session};
use crate::config::{Argon2Config, Config, PasswordConfig, Text};
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::ratelimit::Buckets;
use crate::state::{AppState, Core};
use crate::util::{identity_hash, rfc3339};

/// The login form, and the identity provider of every password account.
pub const LOGIN: &str = "password";
pub const SIGNUP: &str = "password-signup";
pub const RESET: &str = "password-reset";
pub const CHANGE: &str = "password-change";
/// Every id the password forms may use.
pub const IDS: [&str; 4] = [LOGIN, SIGNUP, RESET, CHANGE];

/// Tries of one code before it is destroyed.
const MAX_CODE_ATTEMPTS: i64 = 5;
/// Code length in Crockford base 32 characters (50 bits).
const CODE_LEN: usize = 10;
/// Longest wait for a hashing slot before answering `503`.
const HASH_WAIT: Duration = Duration::from_secs(10);
/// Most remembered addresses per account (lock exemption).
const KNOWN_PER_ACCOUNT: usize = 8;

/// Argon2 computations so far (tests check that an unknown address costs the same as a known one).
pub static HASHES: AtomicU64 = AtomicU64::new(0);

/// OWASP (Password Storage Cheat Sheet): at least 19 MiB x 2 iterations or an equivalent trade-off, down to 7 MiB x 5.
pub fn argon2_strong(a: &Argon2Config) -> bool {
    a.memory_kib >= 7168 && u64::from(a.memory_kib) * u64::from(a.iterations) >= 35_840
}

/// Values Argon2 accepts and this server can afford.
pub fn argon2_bounds(a: &Argon2Config) -> bool {
    (1..=16).contains(&a.parallelism)
        && (1..=20).contains(&a.iterations)
        && a.memory_kib >= 8 * a.parallelism
        && a.memory_kib <= 1_048_576
}

/// An address as the server compares it: trimmed, lower case, ASCII only, `local@domain` with a dotted domain of
/// letters, digits and hyphens; `None` when it is not one. No quoted local parts, comments or IP literals.
pub fn normalize_email(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() > 254 || !s.is_ascii() {
        return None;
    }
    let (local, domain) = s.rsplit_once('@')?;
    let atext = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~-".contains(&b);
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local.bytes().all(|b| atext(b) || b == b'.')
    {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if domain.len() > 253
        || labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || l.starts_with('-')
                || l.ends_with('-')
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || labels.last().is_some_and(|t| t.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(format!(
        "{}@{}",
        local.to_ascii_lowercase(),
        domain.to_ascii_lowercase()
    ))
}

/// A code as typed: case and look-alike letters ignored, dashes and spaces dropped.
pub fn normalize_code(s: &str) -> Option<String> {
    let c: String = s
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            x => x,
        })
        .collect();
    (c.len() == CODE_LEN && c.bytes().all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))).then_some(c)
}

/// A code as shown: `XXXXX-XXXXX`.
pub fn show_code(c: &str) -> String {
    format!("{}-{}", &c[..5], &c[5..])
}

fn code_hash(secret: &[u8], purpose: &str, subject: &[u8], code: &str) -> Vec<u8> {
    let mut m = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    for part in [
        b"kalamar-code-v1".as_slice(),
        purpose.as_bytes(),
        subject,
        code.as_bytes(),
    ] {
        m.update(part);
        m.update(&[0u8]);
    }
    m.finalize().into_bytes().to_vec()
}

/// Stores a fresh code for `(subject, purpose)`, replacing the previous one; returns the code. A `decoy` (origin
/// `decoy`, no account, never sent and never accepted) is stored where a real code would be when the address has
/// no account, so both cases cost the same database write and nothing in the answer's time tells them apart.
fn store_code(
    core: &mut Core,
    purpose: &str,
    subject: &[u8],
    user: Option<&str>,
    ttl_s: i64,
    origin: &str,
) -> ApiResult<String> {
    store_code_tried(core, purpose, subject, user, ttl_s, origin, 0)
}

fn store_code_tried(
    core: &mut Core,
    purpose: &str,
    subject: &[u8],
    user: Option<&str>,
    ttl_s: i64,
    origin: &str,
    attempts: i64,
) -> ApiResult<String> {
    let code = crate::util::new_code(CODE_LEN);
    let h = code_hash(&core.secret, purpose, subject, &code);
    core.db.execute(
        "INSERT OR REPLACE INTO password_codes (subject_hash, purpose, user_id, code_hash, origin, created_at, expires_at, attempts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![subject, purpose, user, h, origin, core.now, core.now + ttl_s, attempts],
    )?;
    Ok(code)
}

/// Checks a typed code: `Some(user_id of a reset code / None for a sign-up code)` when right. A wrong code counts a
/// try (the fifth destroys it); `consume` deletes a right one. Without any code for the address, a decoy that has
/// had one try is written, as a wrong try on a real code would write (`decoy_ttl_s`).
fn check_code(
    core: &mut Core,
    purpose: &str,
    subject: &[u8],
    typed: &str,
    consume: bool,
    decoy_ttl_s: i64,
) -> ApiResult<Option<Option<String>>> {
    type Row = (Option<String>, Vec<u8>, i64, i64, String);
    let row: Option<Row> = core
        .db
        .query_row(
            "SELECT user_id, code_hash, expires_at, attempts, origin FROM password_codes WHERE subject_hash = ?1 AND purpose = ?2",
            params![subject, purpose],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let typed_hash = normalize_code(typed).map(|c| code_hash(&core.secret, purpose, subject, &c));
    let Some((user, stored, expires, attempts, origin)) = row else {
        store_code_tried(core, purpose, subject, None, decoy_ttl_s, "decoy", 1)?;
        return Ok(None);
    };
    let forget = |core: &Core| {
        core.db.execute(
            "DELETE FROM password_codes WHERE subject_hash = ?1 AND purpose = ?2",
            params![subject, purpose],
        )
    };
    if expires <= core.now {
        forget(core)?;
        return Ok(None);
    }
    let right = typed_hash.is_some_and(|h| bool::from(h.ct_eq(&stored))) && origin != "decoy";
    if !right {
        if attempts + 1 >= MAX_CODE_ATTEMPTS {
            forget(core)?;
        } else {
            core.db.execute(
                "UPDATE password_codes SET attempts = attempts + 1 WHERE subject_hash = ?1 AND purpose = ?2",
                params![subject, purpose],
            )?;
        }
        return Ok(None);
    }
    if consume {
        forget(core)?;
    }
    Ok(Some(user))
}

/// The account and password hash of an address hash.
fn lookup(core: &Core, subject: &[u8]) -> ApiResult<Option<(String, String)>> {
    Ok(core
        .db
        .query_row(
            "SELECT p.user_id, p.phc FROM identities i JOIN password_logins p ON p.user_id = i.user_id
             WHERE i.provider = ?1 AND i.subject_hash = ?2",
            params![LOGIN, subject],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn current_phc(core: &Core, user: &str) -> ApiResult<Option<String>> {
    Ok(core
        .db
        .query_row("SELECT phc FROM password_logins WHERE user_id = ?1", [user], |r| {
            r.get(0)
        })
        .optional()?)
}

fn set_password(core: &Core, user: &str, phc: &str) -> ApiResult<()> {
    core.db.execute(
        "INSERT INTO password_logins (user_id, phc, changed_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET phc = excluded.phc, changed_at = excluded.changed_at",
        params![user, phc, core.now],
    )?;
    Ok(())
}

/// Ends the account's sessions but `keep` (their tokens stop working; their event streams close).
fn revoke_sessions(core: &mut Core, user: &str, keep: Option<&str>) -> ApiResult<()> {
    let ids: Vec<String> = {
        let mut s = core.db.prepare("SELECT id FROM sessions WHERE user_id = ?1")?;
        s.query_map([user], |r| r.get(0))?
            .flatten()
            .filter(|id: &String| Some(id.as_str()) != keep)
            .collect()
    };
    if keep.is_none() {
        core.push(user, "session.revoked", json!({"reason": "password_changed"}));
    }
    for id in ids {
        core.db.execute("DELETE FROM sessions WHERE id = ?1", [&id])?;
        core.close_streams(user, Some(&id), "session_revoked");
    }
    // Event tickets of the ended sessions stop working too.
    core.live
        .ws_tickets
        .retain(|_, (u, s, _)| u != user || Some(s.as_str()) == keep);
    Ok(())
}

/// Whether `user` logged in from `ip` recently (in memory only).
fn is_known(core: &Core, user: &str, ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && core
            .live
            .known_ips
            .get(user)
            .is_some_and(|v| v.iter().any(|(a, until)| *a == ip && *until > core.now))
}

fn remember(core: &mut Core, user: &str, ip: IpAddr, days: i64) {
    if ip.is_unspecified() || days <= 0 {
        return;
    }
    let until = core.now + days * 86_400;
    let now = core.now;
    let v = core.live.known_ips.entry(user.to_string()).or_default();
    v.retain(|(a, t)| *a != ip && *t > now);
    v.push((ip, until));
    if v.len() > KNOWN_PER_ACCOUNT {
        v.remove(0);
    }
}

/// Forgets remembered addresses past their time.
pub fn sweep_known(known: &mut HashMap<String, Vec<(IpAddr, i64)>>, now: i64) {
    known.retain(|_, v| {
        v.retain(|(_, t)| *t > now);
        !v.is_empty()
    });
}

fn txt(en: &str, fr: &str) -> Text {
    Text::from([("en".to_string(), en.to_string()), ("fr".to_string(), fr.to_string())])
}

fn wrong_credentials() -> ApiError {
    ApiError::forbidden("wrong_password", "wrong e-mail address or password").text(
        "Wrong e-mail address or password.",
        "Adresse e-mail ou mot de passe incorrect.",
    )
}

fn locked(retry: i64) -> ApiError {
    ApiError::rate_limited(retry).text(
        "Too many failed attempts: wait before trying again.",
        "Trop d'essais manqués : patientez avant de réessayer.",
    )
}

fn wrong_code() -> ApiError {
    ApiError::invalid("/code", "wrong, expired or used code").text(
        "This code is wrong, expired or already used.",
        "Ce code est faux, expiré ou déjà utilisé.",
    )
}

fn busy() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "too many logins at once: try again",
    )
}

/// A string member of a form (`None` when absent, null or empty; `422` when not a string).
fn field<'a>(b: &'a Map<String, Value>, k: &str) -> ApiResult<Option<&'a str>> {
    match b.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ApiError::invalid(&format!("/{k}"), "a string")),
    }
}

fn email_field(b: &Map<String, Value>) -> ApiResult<String> {
    field(b, "email")?.and_then(normalize_email).ok_or_else(|| {
        ApiError::invalid("/email", "an e-mail address (ASCII, local@domain)")
            .text("Type a valid e-mail address.", "Saisissez une adresse e-mail valide.")
    })
}

/// The password service of a server with `[auth.password]` on.
pub struct Passwords {
    pub cfg: PasswordConfig,
    secret: Vec<u8>,
    argon: Argon2<'static>,
    dummy: String,
    gate: Arc<Semaphore>,
    fail_ip: Buckets<IpAddr>,
    fail_account: Buckets<Vec<u8>>,
    sends: Buckets<Vec<u8>>,
    mailer: Option<Mailer>,
    verify_signup: bool,
    server: String,
    contact: String,
}

impl Passwords {
    /// Builds the service (starting the mailer inside the Tokio runtime); `None` when passwords are off.
    pub fn new(cfg: &Config, secret: &[u8]) -> Result<Option<Passwords>, String> {
        let Some(p) = cfg.auth.password.as_ref().filter(|p| p.enabled) else {
            return Ok(None);
        };
        let a = &p.argon2;
        let params = Params::new(a.memory_kib, a.iterations, a.parallelism, None)
            .map_err(|e| format!("auth.password.argon2: {e}"))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let dummy = argon
            .hash_password(crate::util::new_token("dummy").as_bytes())
            .map_err(|e| format!("auth.password: {e}"))?
            .to_string();
        let host = reqwest::Url::parse(&cfg.server.api_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .filter(|h| h.parse::<IpAddr>().is_err() && !h.starts_with('['))
            .unwrap_or_else(|| "localhost".into());
        let mailer = match &p.smtp {
            Some(s) => {
                let pass = match &s.username {
                    Some(_) => Some(crate::config::read_secret(
                        s.password_file.as_deref(),
                        s.password_env.as_deref(),
                        "auth.password.smtp password",
                    )?),
                    None => None,
                };
                Some(Mailer::start(s, pass, &host)?)
            }
            None => None,
        };
        let l = &p.lockout;
        Ok(Some(Passwords {
            verify_signup: p.smtp.as_ref().is_some_and(|s| s.verify_signup),
            cfg: p.clone(),
            secret: secret.to_vec(),
            argon,
            dummy,
            gate: Arc::new(Semaphore::new(p.max_concurrent_hashes.max(1))),
            fail_ip: Buckets::new(1.0 / l.ip_refill_s.max(1.0), l.ip_failures),
            fail_account: Buckets::new(1.0 / l.account_refill_s.max(1.0), l.account_failures),
            sends: Buckets::new(p.codes_per_hour / 3600.0, p.codes_per_hour),
            mailer,
            // In mail subjects: never a line break or another control character.
            server: cfg.server.name.chars().filter(|c| !c.is_control()).collect(),
            contact: cfg.server.operator.contact.clone(),
        }))
    }

    pub fn sweep(&self, now: Instant) {
        self.fail_ip.sweep(now);
        self.fail_account.sweep(now);
        self.sends.sweep(now);
    }

    fn email_hash(&self, email: &str) -> Vec<u8> {
        identity_hash(&self.secret, LOGIN, email)
    }

    /// Whether `provider` is one of the forms this server declares.
    pub fn handles(&self, provider: &str) -> bool {
        match provider {
            LOGIN | RESET => true,
            SIGNUP => self.cfg.signup,
            CHANGE => self.cfg.change_form,
            _ => false,
        }
    }

    async fn permit(&self) -> ApiResult<OwnedSemaphorePermit> {
        match tokio::time::timeout(HASH_WAIT, self.gate.clone().acquire_owned()).await {
            Ok(Ok(p)) => Ok(p),
            _ => Err(busy()),
        }
    }

    /// A new PHC string for `pw` (fresh random salt), computed on the blocking pool.
    async fn hash(&self, pw: &str) -> ApiResult<String> {
        let permit = self.permit().await?;
        let (a, pw) = (self.argon.clone(), pw.to_string());
        HASHES.fetch_add(1, Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            a.hash_password(pw.as_bytes()).map(|h| h.to_string())
        })
        .await
        .map_err(|_| ApiError::internal("password hashing failed"))?
        .map_err(|_| ApiError::internal("password hashing failed"))
    }

    /// Whether `pw` matches `phc`; with `None`, the dummy hash is checked instead (same cost) and the answer is no.
    async fn verify(&self, pw: &str, phc: Option<&str>) -> ApiResult<bool> {
        let permit = self.permit().await?;
        let (a, pw, dummy) = (self.argon.clone(), pw.to_string(), self.dummy.clone());
        let stored = phc.map(str::to_string);
        HASHES.fetch_add(1, Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let real = stored.is_some();
            let target = stored.unwrap_or(dummy.clone());
            let ok = match PasswordHash::new(&target) {
                Ok(h) if h.algorithm == ARGON2ID_IDENT => a.verify_password(pw.as_bytes(), &h).is_ok(),
                _ => {
                    // An unreadable stored hash: spend the same time, then refuse.
                    if let Ok(h) = PasswordHash::new(&dummy) {
                        let _ = a.verify_password(pw.as_bytes(), &h);
                    }
                    false
                }
            };
            ok && real
        })
        .await
        .map_err(|_| ApiError::internal("password check failed"))
    }

    /// The stored hash was made with other parameters than the configured ones.
    fn needs_rehash(&self, phc: &str) -> bool {
        let a = &self.cfg.argon2;
        match PasswordHash::new(phc) {
            Ok(h) => {
                h.algorithm != ARGON2ID_IDENT
                    || Params::try_from(&h)
                        .map(|p| (p.m_cost(), p.t_cost(), p.p_cost()) != (a.memory_kib, a.iterations, a.parallelism))
                        .unwrap_or(true)
            }
            Err(_) => true,
        }
    }

    /// A password member: `None` when absent; `422` above the longest accepted password.
    fn secret_field<'a>(&self, b: &'a Map<String, Value>, k: &str) -> ApiResult<Option<&'a str>> {
        let v = field(b, k)?;
        if v.is_some_and(|s| s.chars().count() > self.cfg.max_length || s.len() > 4 * self.cfg.max_length) {
            return Err(self.policy_error(&format!("/{k}")));
        }
        Ok(v)
    }

    fn policy_error(&self, field: &str) -> ApiError {
        let (min, max) = (self.cfg.min_length, self.cfg.max_length);
        ApiError::invalid(
            field,
            format!("{min} to {max} characters, not the e-mail address or the display name"),
        )
        .text(
            &format!("A password has {min} to {max} characters and is neither your e-mail address nor your name."),
            &format!("Un mot de passe a de {min} à {max} caractères et n'est ni votre adresse e-mail ni votre nom."),
        )
    }

    /// The rules of a new password: length, no control character, not the address (or its local part) nor the
    /// display name, case and surrounding spaces ignored.
    fn policy(&self, field: &str, pw: &str, email: Option<&str>, name: Option<&str>) -> ApiResult<()> {
        let n = pw.chars().count();
        let low = pw.trim().to_lowercase();
        let same = |x: &str| !x.is_empty() && low == x.trim().to_lowercase();
        let bad = n < self.cfg.min_length
            || n > self.cfg.max_length
            || pw.chars().any(char::is_control)
            || email.is_some_and(|e| same(e) || e.split('@').next().is_some_and(same))
            || name.is_some_and(same);
        if bad { Err(self.policy_error(field)) } else { Ok(()) }
    }

    /// Spends the failure tokens a password check may cost: the address's, and the account's unless `known`.
    fn guard(&self, ip: IpAddr, acct: &[u8], known: bool) -> ApiResult<()> {
        let now = Instant::now();
        self.fail_ip.take(&ip, now).map_err(locked)?;
        if !known && let Err(retry) = self.fail_account.take(&acct.to_vec(), now) {
            self.fail_ip.refund(&ip);
            return Err(locked(retry));
        }
        Ok(())
    }

    /// A check succeeded: the address gets its token back and the account's failures are forgotten.
    fn succeeded(&self, ip: IpAddr, acct: &[u8]) {
        self.fail_ip.refund(&ip);
        self.fail_account.forget(&acct.to_vec());
    }

    /// Checks an address and a password; `(user, address hash)` when right. Same answer and same cost for an address
    /// without an account. Upgrades the stored hash when the parameters changed.
    async fn check_credentials(
        &self,
        st: &AppState,
        ip: IpAddr,
        email: &str,
        pw: &str,
    ) -> ApiResult<(String, Vec<u8>)> {
        let h = self.email_hash(email);
        let (found, known) = {
            let core = st.lock();
            let f = lookup(&core, &h)?;
            let known = f.as_ref().is_some_and(|(u, _)| is_known(&core, u, ip));
            (f, known)
        };
        self.guard(ip, &h, known)?;
        let ok = self.verify(pw, found.as_ref().map(|(_, p)| p.as_str())).await?;
        let Some((user, phc)) = found.filter(|_| ok) else {
            return Err(wrong_credentials());
        };
        let rehash = if self.needs_rehash(&phc) {
            Some(self.hash(pw).await?)
        } else {
            None
        };
        let mut core = st.lock();
        if current_phc(&core, &user)?.as_deref() != Some(phc.as_str()) {
            // The password changed meanwhile.
            return Err(wrong_credentials());
        }
        if let Some(n) = rehash {
            core.db.execute(
                "UPDATE password_logins SET phc = ?1 WHERE user_id = ?2",
                params![n, user],
            )?;
        }
        self.succeeded(ip, &h);
        remember(&mut core, &user, ip, self.cfg.lockout.known_address_days);
        Ok((user, h))
    }

    fn mail(&self, to: &str, (subject, body): (String, String)) {
        if let Some(m) = &self.mailer {
            m.send(Outgoing {
                to: to.to_string(),
                subject,
                body,
            });
        }
    }

    fn code_sent(&self, reset: bool) -> ApiError {
        let m = self.cfg.email_code_ttl_s / 60;
        let e = ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "code_required",
            "a code was e-mailed to this address if it can receive one: send the form again with it in `code`",
        )
        .field("/code");
        if reset {
            e.text(
                &format!(
                    "If an account uses this address, a code was e-mailed to it (valid {m} minutes). Type it here with your new password."
                ),
                &format!(
                    "Si un compte utilise cette adresse, un code vient d'y être envoyé (valable {m} minutes). Saisissez-le ici avec votre nouveau mot de passe."
                ),
            )
        } else {
            e.text(
                &format!(
                    "A message was e-mailed to this address: type the code it contains (valid {m} minutes) and send the form again."
                ),
                &format!(
                    "Un message vient d'être envoyé à cette adresse : saisissez le code qu'il contient (valable {m} minutes) et renvoyez le formulaire."
                ),
            )
        }
    }

    fn codes_wait(retry: i64) -> ApiError {
        ApiError::rate_limited(retry).text(
            "Codes were sent to this address recently: wait before asking again.",
            "Des codes ont déjà été envoyés à cette adresse : patientez avant d'en redemander.",
        )
    }

    /// `POST /v1/auth/{provider}/login` of the four forms.
    pub async fn form(
        &self,
        st: &AppState,
        provider: &str,
        ip: IpAddr,
        lang: &'static str,
        body: &Map<String, Value>,
    ) -> ApiResult<Value> {
        match provider {
            LOGIN => {
                let email = email_field(body)?;
                let pw = self
                    .secret_field(body, "password")?
                    .ok_or_else(|| ApiError::invalid("/password", "the password is required"))?;
                let (user, _) = self.check_credentials(st, ip, &email, pw).await?;
                let mut core = st.lock();
                issue_session(&mut core, &user, false)
            }
            SIGNUP if self.cfg.signup => self.signup(st, ip, lang, body).await,
            RESET => self.reset(st, ip, lang, body).await,
            CHANGE if self.cfg.change_form => {
                let email = email_field(body)?;
                let pw = self
                    .secret_field(body, "password")?
                    .ok_or_else(|| ApiError::invalid("/password", "the current password is required"))?;
                let new = self
                    .secret_field(body, "new_password")?
                    .ok_or_else(|| self.policy_error("/new_password"))?;
                self.policy("/new_password", new, Some(&email), None)?;
                let (user, _) = self.check_credentials(st, ip, &email, pw).await?;
                let name = st.lock().display_name(&user);
                self.policy("/new_password", new, None, Some(&name))?;
                let phc = self.hash(new).await?;
                let s = {
                    let mut core = st.lock();
                    set_password(&core, &user, &phc)?;
                    revoke_sessions(&mut core, &user, None)?;
                    issue_session(&mut core, &user, false)?
                };
                self.mail(&email, mail::password_changed(lang, &self.server, &self.contact));
                Ok(s)
            }
            _ => Err(ApiError::not_found("no such direct login provider")),
        }
    }

    async fn signup(
        &self,
        st: &AppState,
        ip: IpAddr,
        lang: &'static str,
        body: &Map<String, Value>,
    ) -> ApiResult<Value> {
        let email = email_field(body)?;
        let name = field(body, "display_name")?
            .ok_or_else(|| ApiError::invalid("/display_name", "display_name is required"))?
            .to_string();
        check_display_name(&st.lock(), &name)?;
        let pw = self
            .secret_field(body, "password")?
            .ok_or_else(|| self.policy_error("/password"))?;
        self.policy("/password", pw, Some(&email), Some(&name))?;
        let h = self.email_hash(&email);
        if self.verify_signup {
            match field(body, "code")? {
                None => {
                    self.sends.take(&h, Instant::now()).map_err(Self::codes_wait)?;
                    let code = {
                        let mut core = st.lock();
                        let taken = find_identity(&core, LOGIN, &h)?.is_some();
                        let origin = if taken { "decoy" } else { "email" };
                        let c = store_code(&mut core, "signup", &h, None, self.cfg.email_code_ttl_s, origin)?;
                        (!taken).then_some(c)
                    };
                    let m = self.cfg.email_code_ttl_s / 60;
                    match code {
                        Some(c) => self.mail(&email, mail::signup_code(lang, &self.server, &show_code(&c), m)),
                        None => self.mail(&email, mail::already_registered(lang, &self.server)),
                    }
                    return Err(self.code_sent(false));
                }
                Some(code) => {
                    self.fail_ip.take(&ip, Instant::now()).map_err(locked)?;
                    let right = check_code(&mut st.lock(), "signup", &h, code, true, self.cfg.email_code_ttl_s)?;
                    if right.is_none() {
                        return Err(wrong_code());
                    }
                    self.fail_ip.refund(&ip);
                }
            }
        } else if find_identity(&st.lock(), LOGIN, &h)?.is_some() {
            return Err(account_exists());
        }
        let phc = self.hash(pw).await?;
        let mut core = st.lock();
        if find_identity(&core, LOGIN, &h)?.is_some() {
            return Err(account_exists());
        }
        crate::auth::charge_creation(st, ip)?;
        let now = core.now;
        let user = create_account(&mut core, LOGIN, &h, &name, |tx, user| {
            tx.execute(
                "INSERT INTO password_logins (user_id, phc, changed_at) VALUES (?1, ?2, ?3)",
                params![user, phc, now],
            )
            .map(|_| ())
        })?;
        remember(&mut core, &user, ip, self.cfg.lockout.known_address_days);
        issue_session(&mut core, &user, true)
    }

    async fn reset(
        &self,
        st: &AppState,
        ip: IpAddr,
        lang: &'static str,
        body: &Map<String, Value>,
    ) -> ApiResult<Value> {
        let email = email_field(body)?;
        let h = self.email_hash(&email);
        let Some(code) = field(body, "code")? else {
            if self.mailer.is_none() {
                return Err(
                    ApiError::invalid("/code", "a code from the server's operator is required").text(
                        &format!("Ask the server's operator ({}) for a reset code.", self.contact),
                        &format!("Demandez un code à l'opérateur du serveur ({}).", self.contact),
                    ),
                );
            }
            self.sends.take(&h, Instant::now()).map_err(Self::codes_wait)?;
            let code = {
                let mut core = st.lock();
                let user = find_identity(&core, LOGIN, &h)?;
                let origin = if user.is_some() { "email" } else { "decoy" };
                let c = store_code(
                    &mut core,
                    "reset",
                    &h,
                    user.as_deref(),
                    self.cfg.email_code_ttl_s,
                    origin,
                )?;
                user.is_some().then_some(c)
            };
            if let Some(c) = code {
                let m = self.cfg.email_code_ttl_s / 60;
                self.mail(&email, mail::reset_code(lang, &self.server, &show_code(&c), m));
            }
            return Err(self.code_sent(true));
        };
        let new = self
            .secret_field(body, "new_password")?
            .ok_or_else(|| self.policy_error("/new_password"))?;
        self.policy("/new_password", new, Some(&email), None)?;
        self.fail_ip.take(&ip, Instant::now()).map_err(locked)?;
        let (user, name) = {
            let mut core = st.lock();
            let ttl = self.cfg.email_code_ttl_s;
            let user = match check_code(&mut core, "reset", &h, code, false, ttl)? {
                Some(Some(u)) if find_identity(&core, LOGIN, &h)?.as_deref() == Some(u.as_str()) => u,
                _ => return Err(wrong_code()),
            };
            let name = core.display_name(&user);
            (user, name)
        };
        self.fail_ip.refund(&ip);
        self.policy("/new_password", new, None, Some(&name))?;
        let phc = self.hash(new).await?;
        let s = {
            let mut core = st.lock();
            let ttl = self.cfg.email_code_ttl_s;
            if check_code(&mut core, "reset", &h, code, true, ttl)?
                .flatten()
                .as_deref()
                != Some(user.as_str())
            {
                return Err(wrong_code());
            }
            set_password(&core, &user, &phc)?;
            revoke_sessions(&mut core, &user, None)?;
            self.succeeded(ip, &h);
            remember(&mut core, &user, ip, self.cfg.lockout.known_address_days);
            issue_session(&mut core, &user, false)?
        };
        self.mail(&email, mail::password_changed(lang, &self.server, &self.contact));
        Ok(s)
    }

    /// The forms of `GET /v1/meta` (`auth.providers`), in the order a launcher shows them.
    pub fn providers(&self, cfg: &Config) -> Vec<Value> {
        let (min, max) = (self.cfg.min_length, self.cfg.max_length);
        let contact = &cfg.server.operator.contact;
        let email = json!({"key": "email", "type": "string", "label": txt("E-mail address", "Adresse e-mail"),
            "required": true, "min_len": 3, "max_len": 254, "autocomplete": "email"});
        let current = |label: Text| {
            json!({"key": "password", "type": "string", "secret": true, "label": label, "required": true,
                   "min_len": 1, "max_len": max, "autocomplete": "current-password"})
        };
        let fresh = |key: &str, required: bool| {
            json!({"key": key, "type": "string", "secret": true,
                "label": if key == "password" { txt("Password", "Mot de passe") } else { txt("New password", "Nouveau mot de passe") },
                "required": required, "min_len": min, "max_len": max, "autocomplete": "new-password",
                "help": txt(&format!("At least {min} characters, neither your e-mail address nor your name."),
                            &format!("Au moins {min} caractères, ni votre adresse e-mail ni votre nom."))})
        };
        let code = |required: bool, help: Text| {
            json!({"key": "code", "type": "string", "label": txt("Code", "Code"), "required": required,
                   "min_len": CODE_LEN, "max_len": 16, "autocomplete": "one-time-code", "help": help})
        };
        let group = |id: &str, purpose: &str, label: Text, fields: Vec<Value>, warning: Option<Text>| {
            let mut p = json!({"id": id, "kind": "direct", "purpose": purpose, "group": LOGIN, "label": label,
                               "fields": fields});
            if let Some(w) = warning {
                p["warning"] = json!(w);
            }
            p
        };
        let email_m = self.cfg.email_code_ttl_s / 60;
        let operator_m = self.cfg.operator_code_ttl_s / 60;
        let mut v = vec![group(
            LOGIN,
            "login",
            txt("E-mail and password", "E-mail et mot de passe"),
            vec![email.clone(), current(txt("Password", "Mot de passe"))],
            None,
        )];
        if self.cfg.signup {
            let mut fields = vec![
                email.clone(),
                fresh("password", true),
                json!({"key": "display_name", "type": "string", "label": txt("Name", "Nom"), "required": true,
                       "min_len": cfg.limits.display_name_min, "max_len": cfg.limits.display_name_max,
                       "help": txt("Shown to other players; you can change it later.",
                                   "Visible des autres joueurs ; vous pourrez le changer.")}),
            ];
            let warning = if self.verify_signup {
                fields.push(code(
                    false,
                    txt(
                        "Leave it empty the first time: a code is e-mailed to you. Then type it and send again.",
                        "Laissez-le vide la première fois : un code vous est envoyé par e-mail. Saisissez-le puis renvoyez.",
                    ),
                ));
                None
            } else {
                Some(txt(
                    &format!(
                        "This server sends no e-mail: your address is only your login name, and if you forget your password only the operator ({contact}) can help."
                    ),
                    &format!(
                        "Ce serveur n'envoie pas d'e-mail : votre adresse ne sert qu'à vous connecter, et si vous oubliez votre mot de passe seul l'opérateur ({contact}) peut vous aider."
                    ),
                ))
            };
            v.push(group(
                SIGNUP,
                "signup",
                txt("Create an account", "Créer un compte"),
                fields,
                warning,
            ));
        }
        let (reset_fields, reset_warning) = if self.mailer.is_some() {
            (
                vec![
                    email.clone(),
                    code(
                        false,
                        txt(
                            "Leave it empty the first time: a code is e-mailed to you.",
                            "Laissez-le vide la première fois : un code vous est envoyé par e-mail.",
                        ),
                    ),
                    fresh("new_password", false),
                ],
                txt(
                    &format!(
                        "Send your address alone first: if an account uses it, a code is e-mailed to it (valid {email_m} minutes). Then type the code and a new password. Your other sessions end."
                    ),
                    &format!(
                        "Envoyez d'abord votre adresse seule : si un compte l'utilise, un code y est envoyé (valable {email_m} minutes). Saisissez ensuite le code et un nouveau mot de passe. Vos autres sessions se terminent."
                    ),
                ),
            )
        } else {
            (
                vec![
                    email.clone(),
                    code(
                        true,
                        txt(
                            &format!("Ask the server's operator ({contact}) for a code."),
                            &format!("Demandez un code à l'opérateur du serveur ({contact})."),
                        ),
                    ),
                    fresh("new_password", true),
                ],
                txt(
                    &format!(
                        "This server sends no e-mail: its operator ({contact}) gives reset codes, valid {operator_m} minutes. Your other sessions end."
                    ),
                    &format!(
                        "Ce serveur n'envoie pas d'e-mail : son opérateur ({contact}) donne les codes, valables {operator_m} minutes. Vos autres sessions se terminent."
                    ),
                ),
            )
        };
        v.push(group(
            RESET,
            "reset",
            txt("Forgotten or new password", "Mot de passe oublié ou à changer"),
            reset_fields,
            Some(reset_warning),
        ));
        if self.cfg.change_form {
            v.push(group(
                CHANGE,
                "change",
                txt("Change password", "Changer de mot de passe"),
                vec![
                    email,
                    current(txt("Current password", "Mot de passe actuel")),
                    fresh("new_password", true),
                ],
                Some(txt(
                    "Your sessions on other devices end.",
                    "Vos sessions sur les autres appareils se terminent.",
                )),
            ));
        }
        v
    }
}

fn account_exists() -> ApiError {
    ApiError::conflict("account_exists", "an account already uses this e-mail address").text(
        "An account already uses this e-mail address: log in, or use \"Forgotten or new password\".",
        "Un compte utilise déjà cette adresse : connectez-vous, ou utilisez « Mot de passe oublié ou à changer ».",
    )
}

/// `POST /v1/me/password` (1.1.5): `{"password": <current>, "new_password": ...}`; `204`, other sessions end.
pub async fn change_me(
    State(st): State<AppState>,
    a: Authed,
    ext: axum::http::Extensions,
    JsonBody(body): JsonBody<Map<String, Value>>,
) -> ApiResult<StatusCode> {
    let Some(p) = st.password.clone() else {
        return Err(ApiError::not_found("this server has no password logins"));
    };
    let ip = ip_of(&ext);
    let pw = p
        .secret_field(&body, "password")?
        .ok_or_else(|| ApiError::invalid("/password", "the current password is required"))?;
    let new = p
        .secret_field(&body, "new_password")?
        .ok_or_else(|| p.policy_error("/new_password"))?;
    let (h, phc, name, known) = {
        let core = st.lock();
        let row: Option<(Vec<u8>, String)> = core
            .db
            .query_row(
                "SELECT i.subject_hash, p.phc FROM identities i JOIN password_logins p ON p.user_id = i.user_id
                 WHERE i.user_id = ?1 AND i.provider = ?2",
                params![a.user, LOGIN],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((h, phc)) = row else {
            return Err(ApiError::not_found(
                "this account has no password login (a password cannot be added to another login)",
            ));
        };
        let known = is_known(&core, &a.user, ip);
        (h, phc, core.display_name(&a.user), known)
    };
    p.policy("/new_password", new, None, Some(&name))?;
    if normalize_email(new).is_some_and(|e| bool::from(p.email_hash(&e).ct_eq(&h))) {
        return Err(p.policy_error("/new_password"));
    }
    p.guard(ip, &h, known)?;
    if !p.verify(pw, Some(&phc)).await? {
        return Err(
            ApiError::forbidden("wrong_password", "the current password is wrong").text(
                "The current password is wrong.",
                "Le mot de passe actuel est incorrect.",
            ),
        );
    }
    p.succeeded(ip, &h);
    let fresh = p.hash(new).await?;
    let mut core = st.lock();
    if current_phc(&core, &a.user)?.as_deref() != Some(phc.as_str()) {
        return Err(ApiError::conflict("conflict", "the password changed meanwhile"));
    }
    set_password(&core, &a.user, &fresh)?;
    revoke_sessions(&mut core, &a.user, Some(&a.session))?;
    Ok(StatusCode::NO_CONTENT)
}

/// The Accept-Language of a request, for mails.
pub fn lang_of(h: &HeaderMap) -> &'static str {
    mail::lang(h.get(axum::http::header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()))
}

/// `admin password-reset <user>`: a reset code the operator gives the player (shown once, stored hashed).
pub fn operator_code(core: &mut Core, user: &str) -> Result<(String, i64), String> {
    let p = core
        .cfg
        .auth
        .password
        .clone()
        .filter(|p| p.enabled)
        .ok_or("auth.password is not enabled in the configuration")?;
    let h: Vec<u8> = core
        .db
        .query_row(
            "SELECT subject_hash FROM identities WHERE user_id = ?1 AND provider = ?2",
            params![user, LOGIN],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("this account has no e-mail and password login")?;
    let code = store_code(core, "reset", &h, Some(user), p.operator_code_ttl_s, "operator").map_err(|e| e.message)?;
    Ok((show_code(&code), core.now + p.operator_code_ttl_s))
}

/// The `password` member of the Me object (1.1.5): when the account has a password.
pub fn me_member(core: &Core, user: &str) -> Option<Value> {
    core.cfg.auth.password.as_ref().filter(|p| p.enabled)?;
    let changed: i64 = core
        .db
        .query_row(
            "SELECT changed_at FROM password_logins WHERE user_id = ?1",
            [user],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()?;
    Some(json!({"changed_at": rfc3339(changed)}))
}

/// What the export says about the account's password login and pending codes.
pub fn export_member(core: &Core, user: &str) -> Value {
    let changed: Option<i64> = core
        .db
        .query_row(
            "SELECT changed_at FROM password_logins WHERE user_id = ?1",
            [user],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let codes: Vec<Value> = core
        .db
        .prepare("SELECT purpose, origin, created_at, expires_at FROM password_codes WHERE user_id = ?1")
        .and_then(|mut s| {
            s.query_map([user], |r| {
                Ok(
                    json!({"purpose": r.get::<_, String>(0)?, "origin": r.get::<_, String>(1)?,
                          "created_at": rfc3339(r.get(2)?), "expires_at": rfc3339(r.get(3)?)}),
                )
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    match changed {
        Some(t) => json!({
            "password_changed_at": rfc3339(t),
            "pending_codes": codes,
            "note": "the server stores your e-mail address only as a keyed hash (it cannot read it back) and your password only as a salted Argon2id hash; neither is in this export",
        }),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalized_or_refused() {
        assert_eq!(
            normalize_email("  Camille.D+kal@Example.ORG "),
            Some("camille.d+kal@example.org".into())
        );
        for bad in [
            "",
            "camille",
            "@example.org",
            "camille@",
            "camille@localhost",
            "camille@example",
            "camille@1.2.3.4",
            "cam ille@example.org",
            "camille@exa mple.org",
            "camille@-label.example.org",
            ".camille@example.org",
            "cam..ille@example.org",
            "\"camille\"@example.org",
            "camille@example.org\r\nBcc: x@example.org",
            "camillé@example.org",
            "camille@[127.0.0.1]",
        ] {
            assert_eq!(normalize_email(bad), None, "{bad:?}");
        }
        assert_eq!(normalize_email(&format!("{}@example.org", "a".repeat(65))), None);
    }

    #[test]
    fn codes_read_as_typed() {
        let c = crate::util::new_code(CODE_LEN);
        assert_eq!(normalize_code(&show_code(&c)), Some(c.clone()));
        assert_eq!(normalize_code(&show_code(&c).to_lowercase()), Some(c.clone()));
        assert_eq!(normalize_code("ABCDE OIL12"), Some("ABCDE01112".into()));
        assert_eq!(normalize_code("ABCDE-0111"), None);
        assert_eq!(normalize_code("ABCDE-0111U"), None, "U is not Crockford");
        let s = b"secret";
        assert_ne!(code_hash(s, "reset", b"a", "X"), code_hash(s, "signup", b"a", "X"));
        assert_ne!(code_hash(s, "reset", b"a", "X"), code_hash(s, "reset", b"b", "X"));
    }

    #[test]
    fn argon2_minimums() {
        let c = |m, t, p| Argon2Config {
            memory_kib: m,
            iterations: t,
            parallelism: p,
        };
        assert!(argon2_strong(&Argon2Config::default()));
        assert!(argon2_strong(&c(47_104, 1, 1)) && argon2_strong(&c(7168, 5, 1)));
        assert!(!argon2_strong(&c(19_456, 1, 1)) && !argon2_strong(&c(4096, 10, 1)));
        assert!(argon2_bounds(&c(19_456, 2, 1)));
        assert!(!argon2_bounds(&c(19_456, 0, 1)) && !argon2_bounds(&c(8, 1, 2)) && !argon2_bounds(&c(4_000_000, 1, 1)));
    }
}
