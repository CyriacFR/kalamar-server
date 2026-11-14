//! Badges, titles and server-designed profiles (protocol 1.1, `presentation.md`). Badges are the community's own
//! (configured in `[[badges]]`), earned by rules evaluated after matches, series and tournaments.
//!
//! Rules (`[[badges]] rule`): `matches:<n>` (completed matches), `wins:<n>` (won matches), `series_wins:<n>` (won
//! series), `tournament:<activity id>` (won that tournament), `tier:<ladder>:<tier id>` (reached that tier on that
//! ladder this season: the shown rating after placement is in that tier or a higher one). Titles (`[[titles]]`)
//! unlock with a badge (or are open to everyone). A badge is earned once (`user_badges`); a new one sends the player
//! `me.updated`. A tournament rule may name an activity that is no longer configured (the badge stays shown).
//!
//! Endpoints: `GET /v1/users/{user_id}/badges`, `GET /v1/titles` (area `badges`) and `GET /v1/users/{user_id}/profile`
//! (area `profiles`, a view of `activities.md` components). Badges and profiles are visible like statistics: to
//! everyone when the player's `public_profile` is on, else to the player and their friends.

use axum::Json;
use axum::extract::{Path, State};
use kalamar_protocol::decision::Decision;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::config::{BadgeEntry, Config, Text, TitleEntry};
use crate::error::{ApiError, ApiResult};
use crate::rating::{Rating, tier_index};
use crate::series::Series;
use crate::state::{AppState, Core, LiveMatch};
use crate::util::{is_id, rfc3339};

/// Icon names a launcher draws (`activities.md`, section 5).
pub const ICONS: &[&str] = &[
    "trophy", "swords", "shield", "star", "crown", "flag", "clock", "calendar", "users", "user", "target", "fire",
    "bolt", "medal", "ribbon", "map", "dice", "lock", "check", "info", "warning", "chat", "chart", "heart",
];

/// Most badges and titles (a view's list holds at most 500 items).
const MAX_ENTRIES: usize = 500;

/// A parsed `[[badges]] rule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule<'a> {
    Matches(i64),
    Wins(i64),
    SeriesWins(i64),
    Tournament(&'a str),
    Tier(&'a str, &'a str),
}

impl<'a> Rule<'a> {
    fn parse(s: &'a str) -> Option<Rule<'a>> {
        let (kind, arg) = s.split_once(':')?;
        let count = || {
            arg.parse::<i64>()
                .ok()
                .filter(|n| (1..=1_000_000).contains(n) && arg.bytes().all(|b| b.is_ascii_digit()))
        };
        Some(match kind {
            "matches" => Rule::Matches(count()?),
            "wins" => Rule::Wins(count()?),
            "series_wins" => Rule::SeriesWins(count()?),
            "tournament" if is_id(arg) => Rule::Tournament(arg),
            "tier" => match arg.split_once(':') {
                Some((ladder, tier)) if is_id(ladder) && is_id(tier) => Rule::Tier(ladder, tier),
                _ => return None,
            },
            _ => return None,
        })
    }
}

const MATCHES_SQL: &str = "SELECT COUNT(*) FROM match_players p JOIN matches m ON m.id = p.match_id
                           WHERE p.user_id = ?1 AND m.state = 'completed'";
const WINS_SQL: &str = "SELECT COUNT(*) FROM match_players WHERE user_id = ?1 AND result = 'win'";

fn count(core: &Core, sql: &str, user: &str) -> i64 {
    core.db.query_row(sql, [user], |r| r.get(0)).unwrap_or(0)
}

/// Series `user` won (completed series whose winning side holds them).
fn series_wins(core: &Core, user: &str) -> i64 {
    let rows: Vec<String> = core
        .db
        .prepare("SELECT data FROM series WHERE state = 'completed' AND instr(data, ?1) > 0")
        .and_then(|mut s| s.query_map([user], |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    rows.iter()
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter(|v| {
            v["winner"].as_u64().is_some_and(|w| {
                v["sides"][w as usize]
                    .as_array()
                    .is_some_and(|side| side.iter().any(|u| u.as_str() == Some(user)))
            })
        })
        .count() as i64
}

/// `user`'s rating on a ladder and season, if they played it.
fn rating_of(core: &Core, user: &str, ladder: &str, season: &str) -> Option<Rating> {
    core.db
        .query_row(
            "SELECT rating, deviation, volatility, games FROM ratings WHERE user_id = ?1 AND ladder = ?2 AND season_id = ?3",
            params![user, ladder, season],
            |r| {
                Ok(Rating {
                    rating: r.get(0)?,
                    deviation: r.get(1)?,
                    volatility: r.get(2)?,
                    games: r.get(3)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
}

/// Whether `user`, placed on `ladder` this season, shows a rating in tier `tier` or a higher one.
fn reached(core: &Core, user: &str, ladder: &str, season: &str, tier: &str) -> bool {
    let p = &core.cfg.rating;
    let Some(want) = p.tiers.iter().position(|t| t.id == tier) else {
        return false;
    };
    rating_of(core, user, ladder, season)
        .is_some_and(|r| r.games >= p.placement_games && tier_index(p, r.shown(p)) >= want)
}

fn has(core: &Core, user: &str, badge: &str) -> bool {
    core.db
        .query_row(
            "SELECT 1 FROM user_badges WHERE user_id = ?1 AND badge_id = ?2",
            params![user, badge],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// Awards `user` every configured badge whose rule `earned` accepts; one `me.updated` when any is new.
fn award(core: &mut Core, user: &str, earned: impl Fn(&Core, Rule<'_>) -> bool) {
    let cfg = core.cfg.clone();
    let mut new = false;
    for b in &cfg.badges {
        let Some(rule) = Rule::parse(&b.rule) else { continue };
        if !earned(&*core, rule) {
            continue;
        }
        match core.db.execute(
            "INSERT OR IGNORE INTO user_badges (user_id, badge_id, earned_at) VALUES (?1, ?2, ?3)",
            params![user, b.id, core.now],
        ) {
            Ok(n) => new |= n > 0,
            Err(e) => tracing::error!(badge = %b.id, error = %e, "badge not saved"),
        }
    }
    if new {
        let me = crate::api::account::me_json(core, user);
        core.push(user, "me.updated", me);
    }
}

/// Count and tier badges of the players of a decided match.
fn after_match(core: &mut Core, users: &[String]) {
    if core.cfg.badges.is_empty() {
        return;
    }
    let season = crate::db::current_season(&core.db, core.now).ok().map(|s| s.0);
    for user in users {
        let (matches, wins) = (count(core, MATCHES_SQL, user), count(core, WINS_SQL, user));
        award(core, user, |core, rule| match rule {
            Rule::Matches(n) => matches >= n,
            Rule::Wins(n) => wins >= n,
            Rule::Tier(ladder, tier) => season.as_deref().is_some_and(|s| reached(core, user, ladder, s, tier)),
            _ => false,
        });
    }
}

/// Series badges of the winners of a series.
fn after_series(core: &mut Core, winners: &[String]) {
    for user in winners {
        let won = series_wins(core, user);
        award(core, user, |_, rule| matches!(rule, Rule::SeriesWins(n) if won >= n));
    }
}

/// After a decided match: awards count-based badges to its players.
pub fn on_match_end(core: &mut Core, m: &LiveMatch, d: &Decision) {
    if !d.counted {
        return;
    }
    let users: Vec<String> = m.seats.iter().filter_map(|s| s.user_id.clone()).collect();
    after_match(core, &users);
}

/// After a series: awards series badges.
pub fn on_series_end(core: &mut Core, s: &Series) {
    if let Some(side) = s.winner.filter(|_| s.state == "completed").and_then(|w| s.sides.get(w)) {
        after_series(core, side);
    }
}

/// A tournament's champion.
pub fn on_tournament_win(core: &mut Core, activity: &str, user: &str) {
    award(
        core,
        user,
        |_, rule| matches!(rule, Rule::Tournament(a) if a == activity),
    );
}

/// `featured_badge` and `title` of the Me object (ids), when `badges` is declared. Ids no longer configured read as
/// none.
pub fn me_members(core: &Core, user: &str) -> (Option<String>, Option<String>) {
    let (featured, title): (Option<String>, Option<String>) = core
        .db
        .query_row("SELECT featured_badge, title FROM users WHERE id = ?1", [user], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    (
        featured.filter(|b| core.cfg.badges.iter().any(|x| &x.id == b)),
        title.filter(|t| core.cfg.titles.iter().any(|x| &x.id == t)),
    )
}

fn unlocked(core: &Core, user: &str, t: &TitleEntry) -> bool {
    t.badge.as_ref().is_none_or(|b| has(core, user, b))
}

/// `PATCH /v1/me` with `featured_badge` / `title` (`Value::Null` clears). Both are checked before either is stored.
pub fn patch_me(core: &mut Core, user: &str, featured: Option<&Value>, title: Option<&Value>) -> ApiResult<()> {
    let id = |v: &Value, field: &str| match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(ApiError::invalid(field, "an id or null")),
    };
    let featured = featured.map(|v| id(v, "/featured_badge")).transpose()?;
    let title = title.map(|v| id(v, "/title")).transpose()?;
    if let Some(Some(b)) = &featured
        && !(core.cfg.badges.iter().any(|x| &x.id == b) && has(core, user, b))
    {
        return Err(ApiError::invalid("/featured_badge", "not a badge you earned"));
    }
    if let Some(Some(t)) = &title {
        let entry = core
            .cfg
            .titles
            .iter()
            .find(|x| &x.id == t)
            .ok_or_else(|| ApiError::invalid("/title", "no such title"))?;
        if !unlocked(core, user, entry) {
            return Err(ApiError::invalid("/title", "this title is locked"));
        }
    }
    if let Some(f) = featured {
        core.db
            .execute("UPDATE users SET featured_badge = ?1 WHERE id = ?2", params![f, user])?;
    }
    if let Some(t) = title {
        core.db
            .execute("UPDATE users SET title = ?1 WHERE id = ?2", params![t, user])?;
    }
    Ok(())
}

/// Configuration problems of badges and titles.
pub fn validate(c: &Config, e: &mut Vec<String>) {
    let long = |t: &Text, max: usize| t.values().any(|s| s.chars().count() > max);
    let mut ids = std::collections::HashSet::new();
    if c.badges.len() > MAX_ENTRIES || c.titles.len() > MAX_ENTRIES {
        e.push(format!("badges, titles: at most {MAX_ENTRIES} of each"));
    }
    for b in &c.badges {
        let n = format!("badges.{}", b.id);
        if !ids.insert(b.id.as_str()) || !is_id(&b.id) {
            e.push(format!("{n}: duplicate or bad id"));
        }
        if b.name.is_empty() || long(&b.name, 120) || long(&b.description, 2000) {
            e.push(format!(
                "{n}: a name (at most 120 characters), a description of at most 2000"
            ));
        }
        if !ICONS.contains(&b.icon.as_str()) {
            e.push(format!("{n}.icon: one of {}", ICONS.join(", ")));
        }
        if b.color
            .as_ref()
            .is_some_and(|x| x.len() != 7 || !x.starts_with('#') || !x[1..].bytes().all(|d| d.is_ascii_hexdigit()))
        {
            e.push(format!("{n}.color: #RRGGBB"));
        }
        match Rule::parse(&b.rule) {
            None => e.push(format!(
                "{n}.rule: `{}` (matches:<n>, wins:<n>, series_wins:<n>, tournament:<activity id> or tier:<ladder>:<tier id>)",
                b.rule
            )),
            Some(Rule::Tier(ladder, tier)) => {
                if c.ladder(ladder).is_none() {
                    e.push(format!("{n}.rule: unknown ladder `{ladder}`"));
                }
                if !c.rating.tiers.iter().any(|t| t.id == tier) {
                    e.push(format!("{n}.rule: unknown tier `{tier}`"));
                }
            }
            Some(_) => {}
        }
    }
    let mut tids = std::collections::HashSet::new();
    for t in &c.titles {
        let n = format!("titles.{}", t.id);
        if !tids.insert(t.id.as_str()) || !is_id(&t.id) {
            e.push(format!("{n}: duplicate or bad id"));
        }
        if t.text.is_empty() || long(&t.text, 120) {
            e.push(format!("{n}.text: 1 to 120 characters"));
        }
        if let Some(b) = &t.badge
            && !ids.contains(b.as_str())
        {
            e.push(format!("{n}.badge: unknown badge `{b}`"));
        }
    }
}

// ---------------------------------------------------------------- views

/// `t` in `lang`, else English, else any language, else `fallback`.
fn local(t: &Text, lang: &str, fallback: &str) -> String {
    t.get(lang)
        .or_else(|| t.get("en"))
        .or_else(|| t.values().next())
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// The configured badges `user` earned, newest first, with when.
fn earned<'a>(core: &'a Core, user: &str) -> Vec<(&'a BadgeEntry, i64)> {
    let rows: Vec<(String, i64)> = core
        .db
        .prepare("SELECT badge_id, earned_at FROM user_badges WHERE user_id = ?1 ORDER BY earned_at DESC, badge_id")
        .and_then(|mut s| {
            s.query_map([user], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(id, at)| core.cfg.badges.iter().find(|b| b.id == id).map(|b| (b, at)))
        .collect()
}

/// The Badge object (`presentation.md`, section 1).
fn badge_json(core: &Core, b: &BadgeEntry, earned_at: i64) -> Value {
    let holders: i64 = core
        .db
        .query_row("SELECT COUNT(*) FROM user_badges WHERE badge_id = ?1", [&b.id], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let rarity = if holders == 1 {
        "1 player".to_string()
    } else {
        format!("{holders} players")
    };
    json!({"id": b.id, "name": b.name, "description": b.description, "icon": b.icon, "color": b.color,
           "rarity": {"en": rarity}, "earned_at": rfc3339(earned_at)})
}

fn title_text(core: &Core, id: &str) -> Option<Value> {
    core.cfg.titles.iter().find(|t| t.id == id).map(|t| json!(t.text))
}

/// Whether `viewer` may see `user`'s badges and profile (like statistics: `public_profile`, else self and friends).
fn visible(core: &Core, viewer: &str, user: &str) -> ApiResult<()> {
    let public: Option<i64> = core
        .db
        .query_row("SELECT public_profile FROM users WHERE id = ?1", [user], |r| r.get(0))
        .optional()?;
    match public {
        None => Err(ApiError::not_found("no such user")),
        Some(0) if viewer != user && !core.are_friends(viewer, user) => Err(ApiError::forbidden(
            "private_profile",
            "this player's profile is private",
        )),
        _ => Ok(()),
    }
}

fn origin_label(core: &Core, kind: &str, id: &str) -> Value {
    match kind {
        "room" => json!({"en": "Private room", "fr": "Salon privé"}),
        "queue" => core
            .cfg
            .queue(id)
            .map(|q| json!(q.name))
            .unwrap_or_else(|| json!({"en": id})),
        _ => core
            .cfg
            .activities
            .iter()
            .find(|a| a.id == id)
            .map(|a| json!(a.title))
            .unwrap_or_else(|| json!({"en": id})),
    }
}

/// The profile view of `user`: a player card, statistics, ratings, badges and recent matches.
fn profile_view(core: &Core, user: &str) -> ApiResult<Value> {
    let badges_on = core.cfg.features.badges;
    let mine = if badges_on { earned(core, user) } else { Vec::new() };
    let mut card = json!({"type": "user", "id": "player", "user": core.user_json(user)});
    if badges_on {
        card["badges"] = mine
            .iter()
            .map(|(b, _)| json!({"id": b.id, "icon": b.icon, "color": b.color}))
            .collect();
        if let Some(t) = me_members(core, user).1.and_then(|t| title_text(core, &t)) {
            card["subtitle"] = t;
        }
    }
    let mut sections = vec![card];
    let (matches, wins) = (count(core, MATCHES_SQL, user), count(core, WINS_SQL, user));
    let rate = if matches > 0 {
        format!("{}%", (wins * 100 + matches / 2) / matches)
    } else {
        "-".into()
    };
    sections.push(json!({"type": "fields", "id": "stats", "items": [
        {"label": {"en": "Matches", "fr": "Matchs"}, "value": matches},
        {"label": {"en": "Wins", "fr": "Victoires"}, "value": wins},
        {"label": {"en": "Win rate", "fr": "Taux de victoire"}, "value": rate},
    ]}));
    if core.cfg.features.ratings {
        let (season, season_name, _) = crate::db::current_season(&core.db, core.now)?;
        let p = &core.cfg.rating;
        let rows: Vec<Value> = core
            .cfg
            .ladders
            .iter()
            .filter_map(|l| {
                let r = rating_of(core, user, &l.id, &season)?;
                let placed = r.games >= p.placement_games;
                let tier = if placed {
                    local(&p.tiers[tier_index(p, r.shown(p))].name, "en", "")
                } else {
                    format!("Placement ({} left)", p.placement_games - r.games)
                };
                Some(json!({"cells": {"ladder": local(&l.name, "en", &l.id),
                                      "rating": placed.then(|| r.shown(p).round() as i64),
                                      "tier": tier, "games": r.games}}))
            })
            .collect();
        if !rows.is_empty() {
            sections.push(
                json!({"type": "table", "id": "ratings", "caption": {"en": season_name}, "columns": [
                {"key": "ladder", "label": {"en": "Ladder", "fr": "Classement"}, "type": "text"},
                {"key": "rating", "label": {"en": "Rating", "fr": "Cote"}, "type": "number"},
                {"key": "tier", "label": {"en": "Tier", "fr": "Rang"}, "type": "text"},
                {"key": "games", "label": {"en": "Matches", "fr": "Matchs"}, "type": "number"},
            ], "rows": rows}),
            );
        }
    }
    if badges_on {
        let items: Vec<Value> = mine
            .iter()
            .map(|(b, _)| json!({"title": b.name, "subtitle": b.description, "icon": b.icon}))
            .collect();
        sections.push(
            json!({"type": "group", "id": "badges", "title": {"en": "Badges", "fr": "Badges"}, "children": [
            {"type": "list", "items": items, "empty": {"en": "No badges yet.", "fr": "Aucun badge pour l'instant."}}]}),
        );
    }
    let mut st = core.db.prepare(
        "SELECT m.origin_kind, m.origin_id, m.ended_at, p.result FROM match_players p JOIN matches m ON m.id = p.match_id
         WHERE p.user_id = ?1 AND m.state IN ('completed', 'void') ORDER BY m.ended_at DESC, m.id DESC LIMIT 5",
    )?;
    let recent: Vec<Value> = st
        .query_map([user], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .flatten()
        .map(|(kind, origin, ended, result)| {
            let title = match result.as_deref() {
                Some("win") => json!({"en": "Win", "fr": "Victoire"}),
                Some("loss") => json!({"en": "Loss", "fr": "Défaite"}),
                _ => json!({"en": "No result", "fr": "Sans résultat"}),
            };
            json!({"title": title, "subtitle": origin_label(core, &kind, &origin), "icon": "swords",
                   "trailing": ended.map(|t| rfc3339(t)[..10].to_string()).unwrap_or_default()})
        })
        .collect();
    sections.push(
        json!({"type": "group", "id": "recent", "title": {"en": "Recent matches", "fr": "Matchs récents"}, "children": [
        {"type": "list", "items": recent, "empty": {"en": "No matches yet.", "fr": "Aucun match pour l'instant."}}]}),
    );
    Ok(json!({"id": user, "title": {"en": core.display_name(user)}, "sections": sections}))
}

// ---------------------------------------------------------------- endpoints

/// `GET /v1/users/{user_id}/badges`.
pub async fn user_badges(State(st): State<AppState>, a: Authed, Path(user): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    if !core.cfg.features.badges {
        return Err(ApiError::feature_disabled());
    }
    visible(&core, &a.user, &user)?;
    let items: Vec<Value> = earned(&core, &user)
        .into_iter()
        .map(|(b, at)| badge_json(&core, b, at))
        .collect();
    let (featured, title) = me_members(&core, &user);
    Ok(Json(json!({"items": items, "featured_badge": featured,
                   "title": title.and_then(|t| title_text(&core, &t))})))
}

/// `GET /v1/titles`: every title, unlocked or not for the caller, with how to unlock it.
pub async fn titles(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    if !core.cfg.features.badges {
        return Err(ApiError::feature_disabled());
    }
    let items: Vec<Value> = core
        .cfg
        .titles
        .iter()
        .map(|t| {
            let hint = match t
                .badge
                .as_ref()
                .and_then(|b| core.cfg.badges.iter().find(|x| &x.id == b))
            {
                Some(b) => json!({"en": format!("Earn the badge \"{}\".", local(&b.name, "en", &b.id)),
                                  "fr": format!("Obtenez le badge « {} ».", local(&b.name, "fr", &b.id))}),
                None => json!({"en": "Available to everyone.", "fr": "Disponible pour tous."}),
            };
            json!({"id": t.id, "text": t.text, "unlocked": unlocked(&core, &a.user, t), "hint": hint})
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

/// `GET /v1/users/{user_id}/profile`.
pub async fn profile(State(st): State<AppState>, a: Authed, Path(user): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    if !core.cfg.features.profiles {
        return Err(ApiError::feature_disabled());
    }
    visible(&core, &a.user, &user)?;
    profile_view(&core, &user).map(Json)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path as FsPath;
    use std::sync::Arc;

    use axum::http::StatusCode;

    use super::*;
    use crate::state::Stream;

    const T0: i64 = 1_800_000_000;

    const CONFIG: &str = r##"
[server]
name = "Test"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
badges = true
profiles = true
[auth.dev]
enabled = true
[[relays]]
host = "127.0.0.1"
[[ladders]]
id = "duel"
name = { en = "Duel" }
[[queues]]
id = "duel"
name = { en = "Duel" }
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{ mode = "turf" }]
result_policy = "standard"
[[badges]]
id = "first-match"
name = { en = "First match" }
rule = "matches:1"
[[badges]]
id = "two-wins"
name = { en = "Two wins", fr = "Deux victoires" }
icon = "star"
color = "#D4AF37"
rule = "wins:2"
[[badges]]
id = "series"
name = { en = "Series winner" }
rule = "series_wins:1"
[[badges]]
id = "cup"
name = { en = "Cup champion" }
icon = "crown"
rule = "tournament:cup"
[[badges]]
id = "gold"
name = { en = "Gold" }
rule = "tier:duel:gold"
[[badges]]
id = "master"
name = { en = "Master" }
rule = "tier:duel:master"
[[titles]]
id = "newcomer"
text = { en = "Newcomer" }
[[titles]]
id = "winner"
text = { en = "Winner" }
badge = "two-wins"
"##;

    fn cfg(extra: &str) -> Config {
        let mut c: Config = toml::from_str(&format!("{CONFIG}{extra}")).unwrap();
        c.normalize();
        c
    }

    fn core() -> Core {
        let db = crate::db::open(FsPath::new(":memory:")).unwrap();
        let core = Core::new(Arc::new(cfg("")), db, vec![0; 32], T0);
        for (i, u) in ["u1", "u2", "u3"].iter().enumerate() {
            core.db
                .execute(
                    "INSERT INTO users (id, display_name, friend_code, created_at) VALUES (?1, ?1, ?2, ?3)",
                    params![u, format!("code{i}"), T0],
                )
                .unwrap();
        }
        core
    }

    /// A completed (or void) 1 v 1 match `id` between `a` (alpha) and `b`, `a` winning when counted.
    fn played(core: &Core, id: &str, a: &str, b: &str, state: &str, ended: i64) {
        core.db
            .execute(
                "INSERT INTO matches (id, origin_kind, origin_id, state, rules, game, created_at, ended_at)
                 VALUES (?1, 'queue', 'duel', ?2, '{}', '{}', ?3, ?3)",
                params![id, state, ended],
            )
            .unwrap();
        let counted = state == "completed";
        for (slot, (u, team, result)) in [(a, "alpha", "win"), (b, "bravo", "loss")].into_iter().enumerate() {
            core.db
                .execute(
                    "INSERT INTO match_players (match_id, slot, user_id, display_name, member_name, team, role, accept, result)
                     VALUES (?1, ?2, ?3, ?3, ?3, ?4, 'guest', 'accepted', ?5)",
                    params![id, slot as i64, u, team, if counted { result } else { "none" }],
                )
                .unwrap();
        }
    }

    fn badges_of(core: &Core, user: &str) -> Vec<String> {
        let mut v: Vec<String> = earned(core, user).into_iter().map(|(b, _)| b.id.clone()).collect();
        v.sort();
        v
    }

    /// An event stream without a connection, to read what was pushed.
    fn listen(core: &mut Core, user: &str) {
        core.live.streams.insert(
            "st1".into(),
            Stream {
                id: "st1".into(),
                user_id: user.into(),
                session_id: "s1".into(),
                seq: 0,
                buffer: VecDeque::new(),
                tx: None,
                conn: 0,
                opened_at: T0,
                closed_at: None,
            },
        );
        core.live.user_streams.insert(user.into(), vec!["st1".into()]);
    }

    fn pushed(core: &Core) -> Vec<Value> {
        core.live.streams["st1"]
            .buffer
            .iter()
            .map(|(_, f)| serde_json::from_str(f).unwrap())
            .collect()
    }

    #[test]
    fn rules_parse() {
        assert_eq!(Rule::parse("matches:10"), Some(Rule::Matches(10)));
        assert_eq!(Rule::parse("wins:1"), Some(Rule::Wins(1)));
        assert_eq!(Rule::parse("series_wins:3"), Some(Rule::SeriesWins(3)));
        assert_eq!(
            Rule::parse("tournament:spring-cup"),
            Some(Rule::Tournament("spring-cup"))
        );
        assert_eq!(Rule::parse("tier:duel:gold"), Some(Rule::Tier("duel", "gold")));
        for bad in [
            "",
            "matches",
            "matches:",
            "matches:0",
            "matches:-1",
            "matches:+3",
            "wins:x",
            "tournament:",
            "tournament:a b",
            "tier:duel",
            "tier::gold",
            "tier:duel:",
            "streak:3",
            "Matches:1",
        ] {
            assert_eq!(Rule::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn configuration_is_checked() {
        assert!(cfg("").validate().is_empty(), "{:?}", cfg("").validate());
        let c = cfg(r##"
[[badges]]
id = "first-match"
name = { en = "Again" }
rule = "matches:2"
[[badges]]
id = "odd"
name = { en = "Odd" }
icon = "https://example.invalid/x.png"
color = "gold"
rule = "streak:3"
[[badges]]
id = "nowhere"
name = { en = "Nowhere" }
rule = "tier:squad:legend"
[[titles]]
id = "ghost"
text = { en = "Ghost" }
badge = "missing"
[[titles]]
id = "newcomer"
text = {}
"##);
        let errors = c.validate().join("\n");
        for want in [
            "badges.first-match: duplicate or bad id",
            "badges.odd.icon",
            "badges.odd.color",
            "badges.odd.rule: `streak:3`",
            "unknown ladder `squad`",
            "unknown tier `legend`",
            "titles.ghost.badge: unknown badge `missing`",
            "titles.newcomer: duplicate or bad id",
            "titles.newcomer.text",
        ] {
            assert!(errors.contains(want), "missing `{want}` in:\n{errors}");
        }
    }

    #[test]
    fn matches_award_count_badges_once() {
        let mut core = core();
        listen(&mut core, "u1");
        played(&core, "m1", "u1", "u2", "completed", T0);
        after_match(&mut core, &["u1".into(), "u2".into()]);
        assert_eq!(badges_of(&core, "u1"), ["first-match"]);
        assert_eq!(badges_of(&core, "u2"), ["first-match"]);
        let events = pushed(&core);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "me.updated");
        assert_eq!(events[0]["data"]["id"], "u1");
        // A void match does not count; a second win does.
        played(&core, "m2", "u1", "u2", "void", T0 + 10);
        core.now = T0 + 20;
        after_match(&mut core, &["u1".into(), "u2".into()]);
        assert_eq!(badges_of(&core, "u1"), ["first-match"]);
        played(&core, "m3", "u1", "u2", "completed", T0 + 30);
        core.now = T0 + 40;
        after_match(&mut core, &["u1".into(), "u2".into()]);
        assert_eq!(badges_of(&core, "u1"), ["first-match", "two-wins"]);
        assert_eq!(badges_of(&core, "u2"), ["first-match"]);
        assert_eq!(pushed(&core).len(), 2, "one event per new award only");
        // Earned once: the first date stays.
        let at: i64 = core
            .db
            .query_row(
                "SELECT earned_at FROM user_badges WHERE user_id = 'u1' AND badge_id = 'first-match'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(at, T0);
    }

    #[test]
    fn series_tournaments_and_tiers() {
        let mut core = core();
        let series = |id: &str, state: &str, winner: Value| {
            json!({"id": id, "sides": [["u1"], ["u2"]], "winner": winner, "state": state}).to_string()
        };
        for (id, state, winner) in [
            ("s1", "void", json!(null)),
            ("s2", "completed", json!(1)),
            ("s3", "running", json!(0)),
        ] {
            core.db
                .execute(
                    "INSERT INTO series (id, origin_kind, origin_id, state, data, created_at) VALUES (?1, 'activity', 'x', ?2, ?3, 0)",
                    params![id, state, series(id, state, winner)],
                )
                .unwrap();
        }
        assert_eq!((series_wins(&core, "u1"), series_wins(&core, "u2")), (0, 1));
        after_series(&mut core, &["u1".into()]);
        after_series(&mut core, &["u2".into()]);
        assert!(badges_of(&core, "u1").is_empty());
        assert_eq!(badges_of(&core, "u2"), ["series"]);

        on_tournament_win(&mut core, "other", "u1");
        assert!(badges_of(&core, "u1").is_empty());
        on_tournament_win(&mut core, "cup", "u1");
        assert_eq!(badges_of(&core, "u1"), ["cup"]);

        // Shown value 1900 - 2 x 50 = 1800: diamond, so gold (and lower) is reached, master is not.
        let season = crate::db::current_season(&core.db, core.now).unwrap().0;
        for (u, games) in [("u2", 10), ("u3", 2)] {
            core.db
                .execute(
                    "INSERT INTO ratings (user_id, ladder, season_id, rating, deviation, volatility, games, wins, losses)
                     VALUES (?1, 'duel', ?2, 1900.0, 50.0, 0.06, ?3, 0, 0)",
                    params![u, season, games],
                )
                .unwrap();
        }
        after_match(&mut core, &["u2".into(), "u3".into()]);
        assert_eq!(badges_of(&core, "u2"), ["gold", "series"]);
        assert!(badges_of(&core, "u3").is_empty(), "still in placement");
    }

    #[test]
    fn featured_badge_and_title() {
        let mut core = core();
        played(&core, "m1", "u1", "u2", "completed", T0);
        played(&core, "m2", "u1", "u2", "completed", T0 + 1);
        after_match(&mut core, &["u1".into(), "u2".into()]);
        let err = |r: ApiResult<()>| r.unwrap_err().field.unwrap();
        assert_eq!(
            err(patch_me(&mut core, "u2", Some(&json!("two-wins")), None)),
            "/featured_badge"
        );
        assert_eq!(
            err(patch_me(&mut core, "u2", Some(&json!("nope")), None)),
            "/featured_badge"
        );
        assert_eq!(err(patch_me(&mut core, "u2", Some(&json!(5)), None)), "/featured_badge");
        assert_eq!(err(patch_me(&mut core, "u2", None, Some(&json!("winner")))), "/title");
        assert_eq!(err(patch_me(&mut core, "u2", None, Some(&json!("nope")))), "/title");
        // A refused title leaves the badge unchanged too.
        assert!(patch_me(&mut core, "u2", Some(&json!("first-match")), Some(&json!("winner"))).is_err());
        assert_eq!(me_members(&core, "u2"), (None, None));
        patch_me(&mut core, "u2", Some(&json!("first-match")), Some(&json!("newcomer"))).unwrap();
        assert_eq!(
            me_members(&core, "u2"),
            (Some("first-match".into()), Some("newcomer".into()))
        );
        patch_me(&mut core, "u1", Some(&json!("two-wins")), Some(&json!("winner"))).unwrap();
        assert_eq!(
            me_members(&core, "u1"),
            (Some("two-wins".into()), Some("winner".into()))
        );
        patch_me(&mut core, "u1", Some(&Value::Null), None).unwrap();
        assert_eq!(me_members(&core, "u1"), (None, Some("winner".into())));
        patch_me(&mut core, "u1", None, Some(&Value::Null)).unwrap();
        assert_eq!(me_members(&core, "u1"), (None, None));
    }

    #[test]
    fn profile_sections() {
        let mut core = core();
        played(&core, "m1", "u1", "u2", "completed", T0);
        played(&core, "m2", "u1", "u2", "void", T0 + 5);
        after_match(&mut core, &["u1".into(), "u2".into()]);
        patch_me(&mut core, "u1", None, Some(&json!("newcomer"))).unwrap();
        let season = crate::db::current_season(&core.db, core.now).unwrap().0;
        core.db
            .execute(
                "INSERT INTO ratings (user_id, ladder, season_id, rating, deviation, volatility, games, wins, losses)
                 VALUES ('u1', 'duel', ?1, 1500.0, 100.0, 0.06, 3, 2, 1)",
                [&season],
            )
            .unwrap();
        let v = profile_view(&core, "u1").unwrap();
        assert_eq!(v["title"]["en"], "u1");
        let types: Vec<&str> = v["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["user", "fields", "table", "group", "group"]);
        let s = &v["sections"];
        assert_eq!(s[0]["subtitle"]["en"], "Newcomer");
        assert_eq!(
            s[0]["badges"],
            json!([{"id": "first-match", "icon": "trophy", "color": null}])
        );
        assert_eq!(s[1]["items"][0]["value"], 1);
        assert_eq!(s[1]["items"][1]["value"], 1);
        assert_eq!(s[1]["items"][2]["value"], "100%");
        let row = &s[2]["rows"][0]["cells"];
        assert_eq!((row["ladder"].as_str(), row["rating"].is_null()), (Some("Duel"), true));
        assert_eq!(row["tier"], "Placement (2 left)");
        assert_eq!(s[3]["children"][0]["items"][0]["title"]["en"], "First match");
        let recent = s[4]["children"][0]["items"].as_array().unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["title"]["en"], "No result");
        assert_eq!(recent[1]["title"]["en"], "Win");
        assert_eq!(recent[1]["subtitle"]["en"], "Duel");
        assert_eq!(recent[1]["trailing"], &rfc3339(T0)[..10]);
    }

    #[tokio::test]
    async fn endpoints_check_areas_and_privacy() {
        let clock = Arc::new(crate::clock::ManualClock::new(T0));
        let st = crate::app::build_state(cfg(""), clock.clone()).unwrap();
        {
            let mut core = st.lock();
            for (i, u) in ["u1", "u2", "u3"].iter().enumerate() {
                core.db
                    .execute(
                        "INSERT INTO users (id, display_name, friend_code, created_at) VALUES (?1, ?1, ?2, ?3)",
                        params![u, format!("code{i}"), T0],
                    )
                    .unwrap();
            }
            on_tournament_win(&mut core, "cup", "u1");
            patch_me(&mut core, "u1", Some(&json!("cup")), Some(&json!("newcomer"))).unwrap();
            core.db
                .execute("UPDATE users SET public_profile = 0 WHERE id = 'u1'", [])
                .unwrap();
            core.db
                .execute(
                    "INSERT INTO friendships (user_a, user_b, created_at) VALUES ('u1', 'u2', 0)",
                    [],
                )
                .unwrap();
        }
        let who = |u: &str| Authed {
            user: u.into(),
            session: "s".into(),
        };
        let badges = |viewer: &str, user: &str| user_badges(State(st.clone()), who(viewer), Path(user.into()));
        let v = badges("u1", "u1").await.unwrap().0;
        assert_eq!(v["items"][0]["id"], "cup");
        assert_eq!(v["items"][0]["rarity"]["en"], "1 player");
        assert_eq!(v["items"][0]["earned_at"], rfc3339(T0));
        assert_eq!(
            (v["featured_badge"].as_str(), &v["title"]),
            (Some("cup"), &json!({"en": "Newcomer"}))
        );
        assert!(badges("u2", "u1").await.is_ok(), "a friend sees a private profile");
        let e = badges("u3", "u1").await.unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::FORBIDDEN, "private_profile"));
        let e = badges("u3", "nobody").await.unwrap_err();
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        let e = profile(State(st.clone()), who("u3"), Path("u1".into()))
            .await
            .unwrap_err();
        assert_eq!(e.code, "private_profile");
        let v = profile(State(st.clone()), who("u2"), Path("u1".into()))
            .await
            .unwrap()
            .0;
        assert_eq!(v["sections"][0]["badges"][0]["icon"], "crown");
        let v = titles(State(st.clone()), who("u1")).await.unwrap().0;
        assert_eq!(
            v["items"][0],
            json!({"id": "newcomer", "text": {"en": "Newcomer"}, "unlocked": true,
                                         "hint": {"en": "Available to everyone.", "fr": "Disponible pour tous."}})
        );
        assert_eq!(v["items"][1]["unlocked"], false);
        assert_eq!(v["items"][1]["hint"]["fr"], "Obtenez le badge « Deux victoires ».");

        let mut off = cfg("");
        off.features.badges = false;
        off.features.profiles = false;
        let st = crate::app::build_state(off, clock).unwrap();
        let e = titles(State(st.clone()), who("u1")).await.unwrap_err();
        assert_eq!(e.code, "feature_disabled");
        let e = user_badges(State(st.clone()), who("u1"), Path("u1".into()))
            .await
            .unwrap_err();
        assert_eq!(e.code, "feature_disabled");
        let e = profile(State(st), who("u1"), Path("u1".into())).await.unwrap_err();
        assert_eq!(e.code, "feature_disabled");
    }
}
