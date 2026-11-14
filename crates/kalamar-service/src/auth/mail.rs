//! Outgoing e-mail for password accounts (`[auth.password.smtp]`): one-time codes and notices, sent by a background
//! task so that a request never waits for (and its timing never depends on) the SMTP relay.
//!
//! TLS is required (`implicit` on 465 or `starttls` on 587, certificates checked against the system's roots); `none`
//! exists only for a test relay on 127.0.0.1 of a non-production server, without credentials. Logs never carry an
//! address, a code or the relay's answer text.

use std::time::Duration;

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tokio::sync::mpsc;

use crate::config::SmtpConfig;

/// Mails waiting to be sent; more are dropped (and logged).
const QUEUE: usize = 256;

/// One message to send.
pub struct Outgoing {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone)]
pub struct Mailer {
    tx: mpsc::Sender<Outgoing>,
}

/// Whether a configured sender parses (`Name <address>` or `address`).
pub fn sender_ok(from: &str) -> bool {
    from.parse::<Mailbox>().is_ok()
}

/// The relay's TLS mode, refusing anything else.
pub fn tls_ok(cfg: &SmtpConfig, production: bool) -> Result<(), String> {
    match cfg.tls.as_str() {
        "implicit" | "starttls" => Ok(()),
        "none" if !production && matches!(cfg.host.as_str(), "127.0.0.1" | "::1" | "localhost") => {
            if cfg.username.is_some() {
                Err("auth.password.smtp: tls = \"none\" sends no credentials (remove username)".into())
            } else {
                Ok(())
            }
        }
        "none" => Err(
            "auth.password.smtp.tls = \"none\" is only for a test relay on 127.0.0.1 when production = false".into(),
        ),
        other => Err(format!("auth.password.smtp.tls: `{other}` (starttls or implicit)")),
    }
}

impl Mailer {
    /// Builds the transport and starts the sending task (inside a Tokio runtime).
    pub fn start(cfg: &SmtpConfig, password: Option<String>, helo: &str) -> Result<Mailer, String> {
        crate::auth::oauth::install_crypto();
        let from: Mailbox = cfg
            .from
            .parse()
            .map_err(|_| "auth.password.smtp.from: not an address".to_string())?;
        let builder = match cfg.tls.as_str() {
            "implicit" => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host).map_err(|e| e.to_string())?,
            "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host).map_err(|e| e.to_string())?,
            _ => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host),
        };
        let mut builder = builder
            .port(cfg.port())
            .timeout(Some(Duration::from_secs(cfg.timeout_s)))
            .hello_name(ClientId::Domain(helo.to_string()));
        if let (Some(user), Some(pass)) = (&cfg.username, password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass));
        }
        let transport = builder.build();
        let domain = helo.to_string();
        let total = Duration::from_secs(cfg.timeout_s.saturating_mul(4).max(5));
        let (tx, mut rx) = mpsc::channel::<Outgoing>(QUEUE);
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let Ok(to) = m.to.parse::<Mailbox>() else {
                    tracing::warn!("mail not sent: the address does not parse");
                    continue;
                };
                let id = format!("<{}@{}>", crate::util::new_id("m"), domain);
                let msg = match Message::builder()
                    .from(from.clone())
                    .to(to)
                    .subject(m.subject)
                    .message_id(Some(id))
                    .header(ContentType::TEXT_PLAIN)
                    .body(m.body)
                {
                    Ok(msg) => msg,
                    Err(_) => {
                        tracing::warn!("mail not sent: it could not be built");
                        continue;
                    }
                };
                match tokio::time::timeout(total, transport.send(msg)).await {
                    Ok(Ok(_)) => tracing::info!("mail sent"),
                    Ok(Err(e)) => tracing::warn!(
                        permanent = e.is_permanent(),
                        transient = e.is_transient(),
                        status = ?e.status().map(|c| c.to_string()),
                        "mail not sent: the relay refused or failed"
                    ),
                    Err(_) => tracing::warn!("mail not sent: the relay timed out"),
                }
            }
        });
        Ok(Mailer { tx })
    }

    /// Queues a message; never waits.
    pub fn send(&self, m: Outgoing) {
        if self.tx.try_send(m).is_err() {
            tracing::warn!("mail dropped: the queue is full");
        }
    }
}

/// The player's language for mails: French when `Accept-Language` starts with `fr`, else English.
pub fn lang(accept_language: Option<&str>) -> &'static str {
    match accept_language.map(|s| s.trim_start().to_ascii_lowercase()) {
        Some(s) if s.starts_with("fr") => "fr",
        _ => "en",
    }
}

/// The texts of the mails: `(subject, body)`.
pub fn reset_code(lang: &str, server: &str, code: &str, minutes: i64) -> (String, String) {
    if lang == "fr" {
        (
            format!("{server} : votre code pour changer de mot de passe"),
            format!(
                "Bonjour,\n\nVoici le code pour choisir un nouveau mot de passe sur {server} : {code}\n\nIl est valable \
                 {minutes} minutes et ne sert qu'une fois. Saisissez-le dans le formulaire « Mot de passe oublié ou à \
                 changer » du lanceur, avec votre nouveau mot de passe.\n\nSi vous n'avez rien demandé, ignorez ce \
                 message : votre mot de passe ne change pas.\n"
            ),
        )
    } else {
        (
            format!("{server}: your code to set a new password"),
            format!(
                "Hello,\n\nHere is the code to choose a new password on {server}: {code}\n\nIt is valid for {minutes} \
                 minutes and works once. Type it in the launcher's \"Forgotten or new password\" form, with your new \
                 password.\n\nIf you did not ask for it, ignore this message: your password does not change.\n"
            ),
        )
    }
}

pub fn signup_code(lang: &str, server: &str, code: &str, minutes: i64) -> (String, String) {
    if lang == "fr" {
        (
            format!("{server} : votre code pour créer un compte"),
            format!(
                "Bonjour,\n\nVoici le code pour créer votre compte sur {server} : {code}\n\nIl est valable {minutes} \
                 minutes. Saisissez-le dans le formulaire « Créer un compte » du lanceur.\n\nSi vous n'avez rien \
                 demandé, ignorez ce message : aucun compte n'est créé sans ce code.\n"
            ),
        )
    } else {
        (
            format!("{server}: your code to create an account"),
            format!(
                "Hello,\n\nHere is the code to create your account on {server}: {code}\n\nIt is valid for {minutes} \
                 minutes. Type it in the launcher's \"Create an account\" form.\n\nIf you did not ask for it, ignore \
                 this message: no account is created without this code.\n"
            ),
        )
    }
}

pub fn already_registered(lang: &str, server: &str) -> (String, String) {
    if lang == "fr" {
        (
            format!("{server} : vous avez déjà un compte"),
            format!(
                "Bonjour,\n\nQuelqu'un (vous, peut-être) a voulu créer un compte sur {server} avec cette adresse, qui a \
                 déjà un compte. Connectez-vous avec, ou utilisez « Mot de passe oublié ou à changer » si vous ne \
                 vous souvenez plus du mot de passe.\n\nSi ce n'était pas vous, ignorez ce message.\n"
            ),
        )
    } else {
        (
            format!("{server}: you already have an account"),
            format!(
                "Hello,\n\nSomeone (maybe you) tried to create an account on {server} with this address, which already \
                 has one. Log in with it, or use \"Forgotten or new password\" if you do not remember the \
                 password.\n\nIf it was not you, ignore this message.\n"
            ),
        )
    }
}

pub fn password_changed(lang: &str, server: &str, contact: &str) -> (String, String) {
    if lang == "fr" {
        (
            format!("{server} : votre mot de passe a changé"),
            format!(
                "Bonjour,\n\nLe mot de passe de votre compte sur {server} vient d'être changé, et vos autres sessions \
                 ont été fermées.\n\nSi ce n'était pas vous, utilisez tout de suite « Mot de passe oublié ou à \
                 changer » et prévenez l'opérateur du serveur : {contact}\n"
            ),
        )
    } else {
        (
            format!("{server}: your password was changed"),
            format!(
                "Hello,\n\nThe password of your account on {server} was just changed, and your other sessions were \
                 closed.\n\nIf it was not you, use \"Forgotten or new password\" at once and tell the server's \
                 operator: {contact}\n"
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smtp(host: &str, tls: &str) -> SmtpConfig {
        SmtpConfig {
            host: host.into(),
            port: None,
            tls: tls.into(),
            username: None,
            password_file: None,
            password_env: None,
            from: "Kalamar <noreply@play.example.org>".into(),
            verify_signup: true,
            timeout_s: 15,
        }
    }

    #[test]
    fn tls_is_required_but_on_a_local_test_relay() {
        assert!(tls_ok(&smtp("smtp.example.org", "starttls"), true).is_ok());
        assert!(tls_ok(&smtp("smtp.example.org", "implicit"), true).is_ok());
        assert!(tls_ok(&smtp("smtp.example.org", "none"), false).is_err());
        assert!(tls_ok(&smtp("127.0.0.1", "none"), true).is_err());
        assert!(tls_ok(&smtp("127.0.0.1", "none"), false).is_ok());
        let mut s = smtp("127.0.0.1", "none");
        s.username = Some("u".into());
        assert!(tls_ok(&s, false).is_err(), "no credentials in clear");
        assert!(tls_ok(&smtp("smtp.example.org", "ssl"), true).is_err());
        assert_eq!(smtp("x", "implicit").port(), 465);
        assert_eq!(smtp("x", "starttls").port(), 587);
        assert!(sender_ok("Kalamar <noreply@play.example.org>") && !sender_ok("not an address"));
        assert_eq!(lang(Some("fr, en;q=0.5")), "fr");
        assert_eq!(lang(Some("de")), "en");
        assert_eq!(lang(None), "en");
    }
}
