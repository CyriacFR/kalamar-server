//! Discovery document and `GET /v1/meta` (`server-card.md`, `api.md` "Meta").

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::util::rfc3339;

/// `GET /.well-known/kalamar-server.json`.
pub async fn discovery(State(st): State<AppState>) -> Response {
    let c = &st.cfg();
    let s = &c.server;
    let mut doc = json!({
        "kalamar_server": 1,
        "name": s.name,
        "api": s.api_url,
        "relay": c.default_relay(),
        "protocol": {"versions": kalamar_protocol::PROTOCOL_VERSIONS},
        "features": c.features.declared(),
        "operator": {"name": s.operator.name, "contact": s.operator.contact},
        "rules_url": s.rules_url,
        "privacy_url": s.privacy_url,
    });
    if !s.description.is_empty() {
        doc["description"] = json!(s.description);
    }
    if let Some(r) = &s.region {
        doc["region"] = json!(r);
    }
    if !s.languages.is_empty() {
        doc["languages"] = json!(s.languages);
    }
    let mut resp = Json(doc).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    resp
}

fn policy_json(p: &crate::config::PolicyEntry) -> Value {
    json!({
        "id": p.id, "name": p.name, "summary": p.summary,
        "quorum": p.quorum, "silence": p.silence, "disagreement": p.disagreement,
        "rated_requires_both_teams": p.rated_requires_both_teams, "abort": p.abort,
        "disconnect": p.disconnect, "no_show": p.no_show, "loadout_check": p.loadout_check,
        "decider": p.decider, "review_deadline_s": p.review_deadline_s,
        "deadlines": {"accept_s": p.accept_s, "launch_s": p.launch_s, "result_s": p.result_s, "grace_s": p.grace_s},
        // 1.1.7: what the spectators' (witnesses') reports are used for.
        "witness": p.witness, "witness_from": p.witness_from,
        // 1.1.8: what the reports' counters weigh (declared on every policy: an absent member means `strict`).
        "stats": p.stats,
        // 1.1.11: what a voluntary departure in play does to a match without a valid result.
        "leave": p.leave,
    })
}

/// `GET /v1/meta`.
pub async fn meta(State(st): State<AppState>) -> Json<Value> {
    let c = &st.cfg();
    let s = &c.server;
    let f = &c.features;
    let revision = st.lock().live.meta_revision;
    let catalog = |v: &[crate::config::CatalogEntry]| -> Vec<Value> {
        v.iter().map(|e| json!({"id": e.id, "label": e.label})).collect()
    };
    let mut m = json!({
        "name": s.name,
        "description": s.description,
        "protocol": {"versions": kalamar_protocol::PROTOCOL_VERSIONS},
        "software": {"name": "kalamar-service", "version": env!("CARGO_PKG_VERSION")},
        "time": rfc3339(st.clock.now()),
        "features": f.declared(),
        "relay": c.default_relay(),
        // 1.1.9: every relay a match may be given, for launchers that measure their latency to each.
        "relays": crate::relays::declaration(c),
        "revision": revision,
        "operator": {"name": s.operator.name, "contact": s.operator.contact},
        "privacy_url": s.privacy_url,
        "game_contract": {"id": kalamar_protocol::CONTRACT_ID, "versions": [kalamar_protocol::CONTRACT_VERSION]},
        "limits": {
            "body_bytes": c.limits.body_bytes, "result_bytes": c.limits.result_bytes, "status_bytes": c.limits.status_bytes,
            "display_name": {"min_len": c.limits.display_name_min, "max_len": c.limits.display_name_max},
            "friends": c.limits.friends_max, "party_size": c.limits.party_size,
        },
        "auth": {"providers": crate::auth::providers_json(&st)},
        "rules": {
            "url": s.rules_url,
            "summary": c.rules.summary,
            "sections": c.rules.sections.iter().map(|x| json!({"id": x.id, "title": x.title, "body": x.body})).collect::<Vec<_>>(),
        },
        "result_policies": c.result_policies.iter().map(policy_json).collect::<Vec<_>>(),
        "catalog": {"modes": catalog(&c.catalog.modes), "stages": catalog(&c.catalog.stages)},
        // 1.1.10: a player whose terminal report is in is free of a match that waits for the others' reports.
        "matches": {"free_after_report": true},
    });
    if f.rooms {
        m["rooms"] = crate::api::rooms::declaration(c);
    }
    if f.queues {
        let p = &c.penalties;
        m["queues"] = json!({"count": c.queues.len(), "penalties": {
            "summary": p.summary, "counted": p.counted, "window_s": p.window_s, "cooldowns_s": p.cooldowns_s}});
    }
    let referee = c
        .result_policies
        .iter()
        .any(|p| p.decider != kalamar_protocol::policy::Decider::Auto);
    if !c.webhooks.is_empty() || !c.operator_tokens.is_empty() || referee {
        m["operator_api"] =
            json!({"webhooks": !c.webhooks.is_empty(), "bot_api": !c.operator_tokens.is_empty(), "referee": referee});
    }
    if f.moderation {
        m["moderation"] = json!({"report_categories": catalog(&c.moderation.report_categories)});
    }
    // 1.1.7: the stands (`spectators.md`): what holds for every place of this server.
    if f.spectators {
        m["spectators"] = json!({
            "version": crate::api::account::SPECTATE_VERSION,
            "summary": c.spectators.summary,
            "opt_out": c.spectators.opt_out,
            "referees": !c.spectators.referees.is_empty(),
            "late": false,
            "verified_modes_only": c.spectators.verified_modes_only,
            "wait_s": c.spectators.wait_s,
        });
    }
    Json(m)
}
