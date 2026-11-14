//! An address without an account costs the server the same Argon2 work as a wrong password, so neither the answer
//! nor its time tells them apart (its own test binary: the hash counter is process-wide).

mod common;

use std::sync::atomic::Ordering;
use std::time::Instant;

use common::{BASE, start};
use kalamar_service::auth::password::HASHES;
use serde_json::json;

#[tokio::test]
async fn unknown_addresses_cost_a_full_password_check() {
    let mut doc: toml::Table = BASE.parse().unwrap();
    let p: toml::Table = "[auth.password]\nenabled = true".parse().unwrap();
    doc["auth"]
        .as_table_mut()
        .unwrap()
        .insert("password".into(), p["auth"]["password"].clone());
    let s = start(doc).await;
    let r = s
        .post(
            "203.0.113.1",
            "/v1/auth/password-signup/login",
            &json!({"email": "ines@example.org", "password": "a password of hers", "display_name": "Ines"}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let mut timings = Vec::new();
    for (email, what) in [
        ("ines@example.org", "a wrong password"),
        ("nobody@example.org", "an unknown address"),
        ("ines@example.org", "a wrong password"),
        ("nobody@example.org", "an unknown address"),
    ] {
        let before = HASHES.load(Ordering::Relaxed);
        let t = Instant::now();
        let r = s
            .post(
                "203.0.113.2",
                "/v1/auth/password/login",
                &json!({"email": email, "password": "not her password"}),
            )
            .await;
        timings.push(t.elapsed());
        assert_eq!((r.status, r.code()), (403, Some("wrong_password")));
        assert_eq!(HASHES.load(Ordering::Relaxed) - before, 1, "{what}: one Argon2 check");
    }
    // Informative: both kinds take the time of one Argon2id check (not compared strictly: machines vary).
    println!("timings: {timings:?}");
    s.running.stop();
}
