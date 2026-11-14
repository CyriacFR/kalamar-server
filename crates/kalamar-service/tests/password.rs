//! E-mail and password accounts (`[auth.password]`): the forms as launchers see them, sign-up, login, wrong
//! passwords and unknown addresses answering alike, the policy, the change form and `POST /v1/me/password`, resets
//! with an operator code and with e-mailed codes (a fake SMTP relay on 127.0.0.1), sign-up verification, sessions
//! ended by a reset, progressive locks per account and per address that never stop the owner's address, export and
//! erasure, and the configuration checks.

mod common;

use std::time::Duration;

use common::{BASE, Server, fake_smtp, start};
use serde_json::{Value, json};

const OWNER: &str = "203.0.113.10";
const OTHER: &str = "203.0.113.20";

fn with_password(extra: &str) -> toml::Table {
    let mut doc: toml::Table = BASE.parse().unwrap();
    let p: toml::Table = format!("[auth.password]\nenabled = true\n{extra}").parse().unwrap();
    doc["auth"]
        .as_table_mut()
        .unwrap()
        .insert("password".into(), p["auth"]["password"].clone());
    doc
}

fn provider<'a>(meta: &'a Value, id: &str) -> Option<&'a Value> {
    meta["auth"]["providers"].as_array()?.iter().find(|p| p["id"] == id)
}

fn keys(p: &Value) -> Vec<(String, bool, bool)> {
    p["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["key"].as_str().unwrap().to_string(),
                f["required"].as_bool().unwrap(),
                f["secret"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

async fn signup(s: &Server, ip: &str, email: &str, pw: &str, name: &str) -> common::Resp {
    s.post(
        ip,
        "/v1/auth/password-signup/login",
        &json!({"email": email, "password": pw, "display_name": name}),
    )
    .await
}

async fn login(s: &Server, ip: &str, email: &str, pw: &str) -> common::Resp {
    s.post(ip, "/v1/auth/password/login", &json!({"email": email, "password": pw}))
        .await
}

fn token(r: &common::Resp) -> String {
    r.body["access_token"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", r.body))
        .to_string()
}

#[tokio::test]
async fn accounts_without_outgoing_mail() {
    let s = start(with_password("")).await;
    let meta = s.meta().await;

    // The forms, drawn by any launcher from their fields (types the 1.1 launcher knows: string, secret).
    let l = provider(&meta, "password").expect("login form");
    assert_eq!(
        (l["kind"].as_str(), l["purpose"].as_str(), l["group"].as_str()),
        (Some("direct"), Some("login"), Some("password"))
    );
    assert_eq!(
        keys(l),
        [("email".into(), true, false), ("password".into(), true, true)]
    );
    let su = provider(&meta, "password-signup").expect("sign-up form");
    assert_eq!(
        keys(su),
        [
            ("email".into(), true, false),
            ("password".into(), true, true),
            ("display_name".into(), true, false)
        ]
    );
    assert!(
        su["warning"]["fr"].as_str().unwrap().contains("n'envoie pas d'e-mail"),
        "the form says it"
    );
    let re = provider(&meta, "password-reset").expect("reset form");
    assert_eq!(
        keys(re),
        [
            ("email".into(), true, false),
            ("code".into(), true, false),
            ("new_password".into(), true, true)
        ]
    );
    assert!(
        re["fields"][1]["help"]["en"]
            .as_str()
            .unwrap()
            .contains("mailto:op@example.invalid")
    );
    assert!(provider(&meta, "password-change").is_some());
    for p in meta["auth"]["providers"].as_array().unwrap() {
        for f in p["fields"].as_array().into_iter().flatten() {
            assert_eq!(f["type"], "string", "only types every launcher draws");
        }
    }

    // Sign-up: a session, a password, the address nowhere.
    let r = signup(
        &s,
        OWNER,
        "  Camille.D@Example.org ",
        "correct horse battery",
        "Camille",
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["created"], true);
    assert_eq!(r.body["me"]["logins"], json!(["password"]));
    assert!(r.body["me"]["password"]["changed_at"].is_string());
    assert!(!r.body.to_string().to_lowercase().contains("example.org"));
    let first = token(&r);
    let me_id = r.body["me"]["id"].as_str().unwrap().to_string();
    // The same address again: refused (without e-mail verification, a sign-up cannot hide that).
    let r = signup(&s, OTHER, "camille.d@example.org", "another long one", "Cam").await;
    assert_eq!((r.status, r.code()), (409, Some("account_exists")));
    assert!(r.body["error"]["text"]["fr"].is_string());

    // The policy.
    for (pw, name) in [
        ("short", "Bob"),
        ("bob@example.org", "Bob"),
        ("BOB@example.org ", "Bob"),
        ("bob", "Bob"),
        ("robertino1", "Robertino1"),
        (&"x".repeat(129), "Bob"),
    ] {
        let r = signup(&s, OTHER, "bob@example.org", pw, name).await;
        assert_eq!(
            (r.status, r.code()),
            (422, Some("validation_failed")),
            "{pw}: {}",
            r.body
        );
        assert!(r.body["error"]["field"] == "/password" || r.body["error"]["field"] == "/display_name");
    }
    for bad in [
        "bob",
        "bob@localhost",
        "bob@exa mple.org",
        "bob@example.org\r\nBcc: x@example.org",
    ] {
        let r = signup(&s, OTHER, bad, "a long enough password", "Bob").await;
        assert_eq!(
            (r.status, r.body["error"]["field"].as_str()),
            (422, Some("/email")),
            "{bad}"
        );
    }

    // Login: the address compared without case; a wrong password and an unknown address answer alike.
    let r = login(&s, OWNER, "CAMILLE.D@example.org", "correct horse battery").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["created"], false);
    assert_eq!(r.body["me"]["id"], me_id.as_str());
    let wrong = login(&s, OTHER, "camille.d@example.org", "wrong password!").await;
    let unknown = login(&s, OTHER, "nobody@example.org", "wrong password!").await;
    assert_eq!((wrong.status, wrong.code()), (403, Some("wrong_password")));
    assert_eq!(wrong.body, unknown.body, "no way to tell an account from none");
    assert_eq!(wrong.status, unknown.status);

    // Another player sees no address; the export says how it is kept and holds no hash.
    let guest = token(
        &s.post(OTHER, "/v1/auth/guest/login", &json!({"display_name": "Gus"}))
            .await,
    );
    let r = s
        .call(OTHER, "GET", &format!("/v1/users/{me_id}"), Some(&guest), None)
        .await;
    assert_eq!(r.status, 200);
    assert!(!r.body.to_string().contains("example.org") && r.body.get("password").is_none());
    let r = s.call(OWNER, "GET", "/v1/me/export", Some(&first), None).await;
    let export = r.body.to_string();
    assert!(r.body["password_login"]["password_changed_at"].is_string());
    assert!(
        !export.contains("example.org") && !export.contains("argon2"),
        "{export}"
    );

    // POST /v1/me/password: the current password is checked, the other sessions end, this one stays.
    let second = token(&login(&s, OWNER, "camille.d@example.org", "correct horse battery").await);
    let ticket = |t: &str| {
        let t = t.to_string();
        let s = &s;
        async move {
            s.call(OWNER, "POST", "/v1/events/ticket", Some(&t), None).await.body["ticket"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let (kept_ticket, ended_ticket) = (ticket(&first).await, ticket(&second).await);
    let r = s
        .call(
            OWNER,
            "POST",
            "/v1/me/password",
            Some(&first),
            Some(&json!({"password": "nope nope nope", "new_password": "a brand new secret"})),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_password")));
    let r = s
        .call(
            OWNER,
            "POST",
            "/v1/me/password",
            Some(&first),
            Some(&json!({"password": "correct horse battery", "new_password": "camille.d@example.org"})),
        )
        .await;
    assert_eq!(
        (r.status, r.body["error"]["field"].as_str()),
        (422, Some("/new_password")),
        "not the address"
    );
    let r = s
        .call(
            OWNER,
            "POST",
            "/v1/me/password",
            Some(&first),
            Some(&json!({"password": "correct horse battery", "new_password": "a brand new secret"})),
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(
        s.call(OWNER, "GET", "/v1/me", Some(&first), None).await.status,
        200,
        "this session stays"
    );
    assert_eq!(
        s.call(OWNER, "GET", "/v1/me", Some(&second), None).await.status,
        401,
        "the other one ends"
    );
    // Its event ticket too (without a WebSocket upgrade, a valid ticket gets 400, an unknown one 401).
    let r = s
        .call(OWNER, "GET", &format!("/v1/events?ticket={ended_ticket}"), None, None)
        .await;
    assert_eq!(r.status, 401, "{}", r.body);
    let r = s
        .call(OWNER, "GET", &format!("/v1/events?ticket={kept_ticket}"), None, None)
        .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(
        login(&s, OWNER, "camille.d@example.org", "correct horse battery")
            .await
            .status,
        403
    );
    assert_eq!(
        login(&s, OWNER, "camille.d@example.org", "a brand new secret")
            .await
            .status,
        200
    );
    // A guest account has no password to change (no linking).
    let r = s
        .call(
            OTHER,
            "POST",
            "/v1/me/password",
            Some(&guest),
            Some(&json!({"password": "x", "new_password": "a brand new secret"})),
        )
        .await;
    assert_eq!(r.status, 404);

    // The change form (logged out): every session ends, a new one is given.
    let r = s
        .post(
            OWNER,
            "/v1/auth/password-change/login",
            &json!({"email": "camille.d@example.org", "password": "a brand new secret", "new_password": "third time lucky"}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(s.call(OWNER, "GET", "/v1/me", Some(&first), None).await.status, 401);
    let after_change = token(&r);

    // Reset with an operator code (no outgoing mail): the code is single use; every session ends.
    let r = s
        .post(
            OWNER,
            "/v1/auth/password-reset/login",
            &json!({"email": "camille.d@example.org", "new_password": "forgotten again!"}),
        )
        .await;
    assert_eq!((r.status, r.body["error"]["field"].as_str()), (422, Some("/code")));
    assert!(r.body["error"]["text"]["en"].as_str().unwrap().contains("operator"));
    let (code, _) = {
        let mut core = s.running.state.lock();
        kalamar_service::auth::password::operator_code(&mut core, &me_id).unwrap()
    };
    let reset = |code: &str, pw: &str| json!({"email": "camille.d@example.org", "code": code, "new_password": pw});
    let r = s
        .post(
            OTHER,
            "/v1/auth/password-reset/login",
            &reset("AAAAA-AAAAA", "forgotten again!"),
        )
        .await;
    assert_eq!((r.status, r.body["error"]["field"].as_str()), (422, Some("/code")));
    let r = s
        .post(
            OTHER,
            "/v1/auth/password-reset/login",
            &reset(&code.to_lowercase(), "forgotten again!"),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["me"]["id"], me_id.as_str());
    assert_eq!(
        s.call(OWNER, "GET", "/v1/me", Some(&after_change), None).await.status,
        401,
        "sessions end"
    );
    let r = s
        .post(OTHER, "/v1/auth/password-reset/login", &reset(&code, "and once more!!"))
        .await;
    assert_eq!((r.status, r.code()), (422, Some("validation_failed")), "single use");
    assert_eq!(
        login(&s, OWNER, "camille.d@example.org", "forgotten again!")
            .await
            .status,
        200
    );
    // Five wrong tries destroy a code.
    let (code, _) = {
        let mut core = s.running.state.lock();
        kalamar_service::auth::password::operator_code(&mut core, &me_id).unwrap()
    };
    for _ in 0..5 {
        s.post(
            OTHER,
            "/v1/auth/password-reset/login",
            &reset("BBBBB-BBBBB", "forgotten again?"),
        )
        .await;
    }
    let r = s
        .post(
            OTHER,
            "/v1/auth/password-reset/login",
            &reset(&code, "forgotten again?"),
        )
        .await;
    assert_eq!(r.status, 422, "the code died after five wrong tries");
    // Codes are stored hashed.
    let (code, _) = {
        let mut core = s.running.state.lock();
        kalamar_service::auth::password::operator_code(&mut core, &me_id).unwrap()
    };
    {
        let core = s.running.state.lock();
        let n: i64 = core
            .db
            .query_row(
                "SELECT COUNT(*) FROM password_codes WHERE instr(hex(code_hash), hex(?1)) > 0",
                [code.replace('-', "")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let phc: String = core
            .db
            .query_row("SELECT phc FROM password_logins", [], |r| r.get(0))
            .unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
    }

    // Erasure removes the password and the codes; the address can sign up again.
    let t = token(&login(&s, OWNER, "camille.d@example.org", "forgotten again!").await);
    assert_eq!(
        s.call(OWNER, "DELETE", "/v1/me?confirm=erase", Some(&t), None)
            .await
            .status,
        204
    );
    {
        let core = s.running.state.lock();
        for table in ["password_logins", "password_codes"] {
            let n: i64 = core
                .db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
    }
    assert_eq!(
        login(&s, OWNER, "camille.d@example.org", "forgotten again!")
            .await
            .status,
        403
    );
    assert_eq!(
        signup(&s, OWNER, "camille.d@example.org", "a fresh start!!", "Camille")
            .await
            .status,
        200
    );
    s.running.stop();
}

#[tokio::test]
async fn accounts_with_e_mailed_codes() {
    let (port, inbox) = fake_smtp().await;
    let s = start(with_password(&format!(
        "codes_per_hour = 6\n[auth.password.smtp]\nhost = \"127.0.0.1\"\nport = {port}\ntls = \"none\"\nfrom = \"Kalamar test <noreply@play.example.org>\"\n"
    )))
    .await;
    let meta = s.meta().await;
    let su = provider(&meta, "password-signup").unwrap();
    assert_eq!(
        keys(su).last().unwrap(),
        &("code".to_string(), false, false),
        "verification: an optional code"
    );
    let re = provider(&meta, "password-reset").unwrap();
    assert_eq!(
        keys(re),
        [
            ("email".into(), true, false),
            ("code".into(), false, false),
            ("new_password".into(), false, true)
        ]
    );

    // Sign-up, first pass: a code is e-mailed, the answer asks for it.
    let form = |code: Option<&str>| {
        let mut f = json!({"email": "dana@example.org", "password": "a good long password", "display_name": "Dana"});
        if let Some(c) = code {
            f["code"] = json!(c);
        }
        f
    };
    let r = s.post(OWNER, "/v1/auth/password-signup/login", &form(None)).await;
    assert_eq!(
        (r.status, r.code(), r.body["error"]["field"].as_str()),
        (422, Some("code_required"), Some("/code"))
    );
    let first_answer = r.body.clone();
    let m = inbox.wait(1).await;
    assert_eq!(m.rcpt, ["dana@example.org"]);
    assert!(
        m.data.contains("Subject: Logins: your code to create an account"),
        "{}",
        m.data
    );
    assert!(m.data.contains("noreply@play.example.org"), "{}", m.data);
    let code = common::code_in(&m);
    let r = s
        .post(OWNER, "/v1/auth/password-signup/login", &form(Some("ZZZZZ-ZZZZZ")))
        .await;
    assert_eq!((r.status, r.body["error"]["field"].as_str()), (422, Some("/code")));
    let r = s
        .post(OWNER, "/v1/auth/password-signup/login", &form(Some(&code)))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["created"], true);
    let old = token(&r);
    // Signing up again with the address: the same answer, and a mail telling the owner (no code).
    let r = s.post(OTHER, "/v1/auth/password-signup/login", &form(None)).await;
    assert_eq!(r.body, first_answer, "a registered address answers like a new one");
    let m = inbox.wait(2).await;
    assert!(
        m.data.contains("already has one") && m.rcpt == ["dana@example.org"],
        "{}",
        m.data
    );

    // Reset: an unknown address gets the same answer and no mail; a known one gets a code.
    let ask = |email: &str| json!({"email": email});
    let unknown = s
        .post(OTHER, "/v1/auth/password-reset/login", &ask("nobody@example.org"))
        .await;
    inbox.stays(2).await;
    {
        // The taken sign-up and the unknown address wrote a decoy where a real code would be: the same work.
        let core = s.running.state.lock();
        let decoys: i64 = core
            .db
            .query_row(
                "SELECT COUNT(*) FROM password_codes WHERE origin = 'decoy' AND user_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(decoys, 2);
    }
    let known = s
        .call_lang(
            OTHER,
            "POST",
            "/v1/auth/password-reset/login",
            None,
            Some(&ask("Dana@Example.org")),
            Some("en"),
        )
        .await;
    assert_eq!((known.status, known.code()), (422, Some("code_required")));
    assert_eq!(known.body, unknown.body, "no way to tell an account from none");
    let m = inbox.wait(3).await;
    assert!(m.data.contains("Subject: Logins: your code to set a new password"));
    let code = common::code_in(&m);
    let r = s
        .post(
            OTHER,
            "/v1/auth/password-reset/login",
            &json!({"email": "dana@example.org", "code": code, "new_password": "a new good password"}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        s.call(OWNER, "GET", "/v1/me", Some(&old), None).await.status,
        401,
        "the reset ended old sessions"
    );
    let m = inbox.wait(4).await;
    assert!(m.data.contains("your password was changed"), "{}", m.data);
    assert_eq!(
        login(&s, OWNER, "dana@example.org", "a new good password").await.status,
        200
    );

    // A French player gets French mail.
    let r = s
        .call_lang(
            OTHER,
            "POST",
            "/v1/auth/password-reset/login",
            None,
            Some(&ask("dana@example.org")),
            Some("fr-FR, en;q=0.5"),
        )
        .await;
    assert_eq!(r.code(), Some("code_required"));
    let m = inbox.wait(5).await;
    assert!(
        m.data
            .contains("Subject: Logins : votre code pour changer de mot de passe"),
        "{}",
        m.data
    );

    // At most `codes_per_hour` (6 here) per address, known or not: the next is refused alike.
    for (email, before) in [("dana@example.org", 4), ("nobody@example.org", 1)] {
        let mut ok = 0;
        loop {
            let r = s.post(OTHER, "/v1/auth/password-reset/login", &ask(email)).await;
            if r.status == 429 {
                break;
            }
            assert_eq!(r.code(), Some("code_required"), "{}", r.body);
            ok += 1;
            assert!(ok < 20);
        }
        assert_eq!(before + ok, 6, "{email}");
    }
    s.running.stop();
}

#[tokio::test]
async fn locks_are_progressive_and_spare_the_owner() {
    let s = start(with_password(
        "[auth.password.lockout]\naccount_failures = 3\naccount_refill_s = 2\nip_failures = 6\nip_refill_s = 3600\n",
    ))
    .await;
    assert_eq!(
        signup(&s, OWNER, "eve@example.org", "the owner's password", "Eve")
            .await
            .status,
        200
    );
    // Three failures from an attacker's address, then the account is locked for every other address...
    for _ in 0..3 {
        assert_eq!(
            login(&s, "198.51.100.1", "eve@example.org", "guess guess guess")
                .await
                .status,
            403
        );
    }
    let r = login(&s, "198.51.100.2", "eve@example.org", "guess guess guess").await;
    assert_eq!((r.status, r.code()), (429, Some("rate_limited")), "{}", r.body);
    assert!(r.body["error"]["retry_after_s"].as_i64().is_some_and(|s| s >= 1));
    // ... and an unknown address locks the same way (the lock tells nothing).
    for _ in 0..3 {
        assert_eq!(
            login(&s, "198.51.100.3", "ghost@example.org", "guess guess guess")
                .await
                .status,
            403
        );
    }
    assert_eq!(
        login(&s, "198.51.100.4", "ghost@example.org", "guess guess guess")
            .await
            .status,
        429
    );
    // The owner, from where they signed up, still logs in.
    let r = login(&s, OWNER, "eve@example.org", "the owner's password").await;
    assert_eq!(r.status, 200, "the lock never stops the owner's address: {}", r.body);
    // The lock lifts by itself (one more try every two seconds here).
    for _ in 0..3 {
        login(&s, "198.51.100.5", "eve@example.org", "guess guess guess").await;
    }
    assert_eq!(
        login(&s, "198.51.100.6", "eve@example.org", "x x x x x x x x")
            .await
            .status,
        429
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(
        login(&s, "198.51.100.6", "eve@example.org", "x x x x x x x x")
            .await
            .status,
        403
    );
    // One address that sprays many accounts is locked for all of them.
    let spray = "198.51.100.9";
    for i in 0..6 {
        assert_eq!(
            login(&s, spray, &format!("user{i}@example.org"), "Summer2026!!")
                .await
                .status,
            403
        );
    }
    assert_eq!(login(&s, spray, "user9@example.org", "Summer2026!!").await.status, 429);
    s.running.stop();
}

#[tokio::test]
async fn malformed_password_requests_never_answer_5xx() {
    let s = start(with_password("")).await;
    let t = token(&signup(&s, OWNER, "fuzz@example.org", "a fuzzing password", "Fuzz").await);
    let long = "é".repeat(5000);
    let bodies = [
        json!(null),
        json!([]),
        json!({"email": 5, "password": true}),
        json!({"email": "fuzz@example.org", "password": null}),
        json!({"email": "fuzz@example.org", "password": long}),
        json!({"email": long, "password": "x"}),
        json!({"email": "fuzz@example.org", "code": ["x"], "new_password": {}}),
        json!({"email": "\u{0000}@example.org", "password": "\u{0000}"}),
        json!({"email": "fuzz@example.org", "password": "a fuzzing password", "new_password": "\u{0007}bell bell bell"}),
    ];
    for path in [
        "/v1/auth/password/login",
        "/v1/auth/password-signup/login",
        "/v1/auth/password-reset/login",
        "/v1/auth/password-change/login",
        "/v1/me/password",
    ] {
        for b in &bodies {
            let r = s.call(OTHER, "POST", path, Some(&t), Some(b)).await;
            assert!(r.status < 500, "{path} {b} -> {} {}", r.status, r.body);
            if r.status >= 400 {
                assert!(r.body["error"]["code"].is_string(), "{path}: {}", r.body);
            }
        }
    }
    s.running.stop();
}

#[test]
fn configuration_checks() {
    let errors = |doc: toml::Table, production: bool| {
        let mut cfg = common::config(doc, 8080);
        if production {
            cfg.server.production = true;
            cfg.server.api_url = "https://play.example.org/api".into();
            cfg.features.test_clock = false;
        }
        cfg.normalize();
        (cfg.validate(), cfg.warnings())
    };
    let (e, w) = errors(with_password(""), true);
    assert!(e.is_empty(), "{e:?}");
    assert!(w.iter().any(|w| w.contains("admin password-reset")), "{w:?}");
    let (e, w) = errors(with_password(""), false);
    assert!(
        e.is_empty() && w.iter().any(|w| w.contains("production = true")),
        "{w:?}"
    );
    // The protocol forbids requiring an e-mail address: a password-only server is refused.
    let mut only = with_password("");
    only["auth"]["guest"]
        .as_table_mut()
        .unwrap()
        .insert("enabled".into(), false.into());
    assert!(
        errors(only, true)
            .0
            .iter()
            .any(|e| e.contains("forbids requiring an e-mail"))
    );
    for (extra, why) in [
        ("min_length = 4", "min_length"),
        ("[auth.password.argon2]\nmemory_kib = 4096\niterations = 1", "OWASP"),
        (
            "[auth.password.smtp]\nhost = \"smtp.example.org\"\ntls = \"none\"\nfrom = \"a@example.org\"",
            "tls",
        ),
        (
            "[auth.password.smtp]\nhost = \"smtp.example.org\"\nfrom = \"not an address\"",
            "from",
        ),
        (
            "[auth.password.smtp]\nhost = \"smtp.example.org\"\nfrom = \"a@example.org\"\nusername = \"kalamar\"",
            "password_file",
        ),
    ] {
        let (e, _) = errors(with_password(extra), true);
        assert!(e.iter().any(|m| m.contains(why)), "{why}: {e:?}");
    }
    // Weaker hashing is allowed for tests, with a warning.
    let (e, w) = errors(
        with_password("[auth.password.argon2]\nmemory_kib = 4096\niterations = 1"),
        false,
    );
    assert!(e.is_empty() && w.iter().any(|w| w.contains("OWASP")), "{e:?} {w:?}");
    // A secret file that is missing is reported by check-config (never its content).
    let missing = with_password(
        "[auth.password.smtp]\nhost = \"smtp.example.org\"\nfrom = \"a@example.org\"\nusername = \"k\"\npassword_file = \"/nonexistent/kalamar-smtp\"",
    );
    let cfg = common::config(missing, 8080);
    assert!(cfg.check_secrets().iter().any(|m| m.contains("smtp")));
    let present = with_password(&format!(
        "[auth.password.smtp]\nhost = \"smtp.example.org\"\nfrom = \"a@example.org\"\nusername = \"k\"\npassword_file = '{}'",
        common::secret_file("relay-password").display()
    ));
    assert!(common::config(present, 8080).check_secrets().is_empty());
}
