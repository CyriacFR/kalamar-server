//! Ratings, ladders, seasons, leaderboards, statistics and history (`api.md`, "Ratings" and "Statistics and
//! history"). The reference rating system (Glicko-2, placement, tiers) is presented as opaque data: value, display,
//! tier, progress, provisional status, last change.

use axum::Json;
use axum::extract::{Path, Query, State};
use kalamar_protocol::Team;
use kalamar_protocol::decision::Decision;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::rating::{Rating, tier_index, update_team_match};
use crate::state::{AppState, Core, LiveMatch};
use crate::util::rfc3339;

fn need(area: bool) -> ApiResult<()> {
    if area {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

#[derive(Debug, Clone)]
struct Row {
    r: Rating,
    wins: i64,
    losses: i64,
    last_delta: Option<f64>,
    last_match: Option<String>,
    last_at: Option<i64>,
}

fn load_row(core: &Core, user: &str, ladder: &str, season: &str) -> Option<Row> {
    core.db
        .query_row(
            "SELECT rating, deviation, volatility, games, wins, losses, last_delta, last_match, last_at FROM ratings
             WHERE user_id = ?1 AND ladder = ?2 AND season_id = ?3",
            params![user, ladder, season],
            |r| {
                Ok(Row {
                    r: Rating {
                        rating: r.get(0)?,
                        deviation: r.get(1)?,
                        volatility: r.get(2)?,
                        games: r.get(3)?,
                    },
                    wins: r.get(4)?,
                    losses: r.get(5)?,
                    last_delta: r.get(6)?,
                    last_match: r.get(7)?,
                    last_at: r.get(8)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
}

fn tier_json(core: &Core, idx: usize) -> Value {
    let t = &core.cfg.rating.tiers[idx];
    json!({"id": t.id, "name": t.name, "color": t.color, "order": idx})
}

/// `(display, tier)` of a rating: `None` during placement.
fn shown(core: &Core, r: &Rating) -> (Option<i64>, Option<Value>) {
    if r.games < core.cfg.rating.placement_games {
        return (None, None);
    }
    let v = r.shown(&core.cfg.rating);
    (
        Some(v.round() as i64),
        Some(tier_json(core, tier_index(&core.cfg.rating, v))),
    )
}

/// The rating view of `user` on a ladder and season.
fn view(core: &Core, ladder: &str, season: &str, row: &Row) -> Value {
    let p = &core.cfg.rating;
    let (value, tier) = shown(core, &row.r);
    let remaining = (p.placement_games - row.r.games).max(0);
    let progress = value.and_then(|v| {
        let i = tier_index(p, v as f64);
        let next = p.tiers.get(i + 1)?;
        let base = if i == 0 { next.min - 300.0 } else { p.tiers[i].min };
        let span = (next.min - base).max(1.0);
        let done = (v as f64 - base).clamp(0.0, span);
        let name = next.name.get("en").cloned().unwrap_or_else(|| next.id.clone());
        Some(json!({"value": done.round() as i64, "max": span.round() as i64,
                    "label": {"en": format!("{} / {} to {}", done.round() as i64, span.round() as i64, name)}}))
    });
    json!({
        "ladder": ladder,
        "season": season,
        "value": value,
        "display": value.map(|v| v.to_string()),
        "tier": tier,
        "progress": progress,
        "provisional": (remaining > 0).then(|| json!({"remaining": remaining,
            "label": {"en": format!("{remaining} placement matches left")}})),
        "last_change": row.last_at.map(|at| json!({
            "delta": row.last_delta.map(|d| d.round() as i64),
            "match_id": row.last_match,
            "at": rfc3339(at),
            "reason": {"en": if row.last_delta.unwrap_or(0.0) >= 0.0 { "Win" } else { "Loss" }},
        })),
        "fields": [
            {"key": "games", "label": {"en": "Matches"}, "value": row.r.games},
            {"key": "wins", "label": {"en": "Wins"}, "value": row.wins},
            {"key": "losses", "label": {"en": "Losses"}, "value": row.losses},
        ],
        "x-reference": {"rating": (row.r.rating * 10.0).round() / 10.0, "deviation": (row.r.deviation * 10.0).round() / 10.0},
    })
}

/// Rates a decided match; returns the rating changes of the outcome.
pub fn apply_match(core: &mut Core, m: &LiveMatch, d: &Decision) -> ApiResult<Vec<Value>> {
    let (Some(ladder), Some(winner)) = (m.rules.ladder.clone(), d.winner) else {
        return Ok(Vec::new());
    };
    let season = match &m.rules.season {
        Some(s) => s.clone(),
        None => crate::db::current_season(&core.db, core.now)?.0,
    };
    let initial = Rating::initial(&core.cfg.rating);
    let rows: Vec<Option<Row>> = m
        .seats
        .iter()
        .map(|s| {
            s.user_id.as_ref().map(|u| {
                load_row(core, u, &ladder, &season).unwrap_or(Row {
                    r: initial,
                    wins: 0,
                    losses: 0,
                    last_delta: None,
                    last_match: None,
                    last_at: None,
                })
            })
        })
        .collect();
    let side = |t: Team| -> Vec<usize> {
        m.seats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.team == t)
            .map(|(i, _)| i)
            .collect()
    };
    let (ia, ib) = (side(Team::Alpha), side(Team::Bravo));
    let ratings = |ix: &[usize]| -> Vec<Rating> {
        ix.iter()
            .map(|&i| rows[i].as_ref().map(|r| r.r).unwrap_or(initial))
            .collect()
    };
    let score_a = if winner == Team::Alpha { 1.0 } else { 0.0 };
    let (na, nb) = update_team_match(&ratings(&ia), &ratings(&ib), score_a, &core.cfg.rating);
    let mut changes = Vec::new();
    for (ix, new) in [(ia, na), (ib, nb)] {
        for (k, &i) in ix.iter().enumerate() {
            let (Some(user), Some(old)) = (m.seats[i].user_id.clone(), rows[i].clone()) else {
                continue;
            };
            let won = m.seats[i].team == winner;
            let after = new[k];
            let p = &core.cfg.rating;
            let delta = after.shown(p) - old.r.shown(p);
            core.db.execute(
                "INSERT INTO ratings (user_id, ladder, season_id, rating, deviation, volatility, games, wins, losses, last_delta, last_match, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (user_id, ladder, season_id) DO UPDATE SET rating = ?4, deviation = ?5, volatility = ?6,
                   games = ?7, wins = ?8, losses = ?9, last_delta = ?10, last_match = ?11, last_at = ?12",
                params![user, ladder, season, after.rating, after.deviation, after.volatility, after.games,
                        old.wins + won as i64, old.losses + (!won) as i64, delta, m.id, core.now],
            )?;
            let (bd, bt) = shown(core, &old.r);
            let (ad, at) = shown(core, &after);
            changes.push(json!({
                "user_id": user, "ladder": ladder, "season": season, "delta": delta.round() as i64,
                "before": {"display": bd.map(|v| v.to_string()), "tier": bt},
                "after": {"display": ad.map(|v| v.to_string()), "tier": at},
                "reason": {"en": if won { "Win" } else { "Loss" }},
            }));
        }
    }
    Ok(changes)
}

/// `rating.changed` to every player whose rating changed.
pub fn notify_changes(core: &mut Core, changes: &[Value]) {
    for c in changes {
        let (Some(user), Some(ladder), Some(season)) =
            (c["user_id"].as_str(), c["ladder"].as_str(), c["season"].as_str())
        else {
            continue;
        };
        let ratings: Vec<Value> = load_row(core, user, ladder, season)
            .map(|r| vec![view(core, ladder, season, &r)])
            .unwrap_or_default();
        let user = user.to_string();
        core.push(&user, "rating.changed", json!({"changes": [c], "ratings": ratings}));
    }
}

fn season_json(core: &Core, id: &str) -> Value {
    core.db
        .query_row(
            "SELECT name, starts_at, ends_at FROM seasons WHERE id = ?1",
            [id],
            |r| {
                Ok(
                    json!({"id": id, "name": {"en": r.get::<_, String>(0)?}, "starts_at": rfc3339(r.get(1)?),
                      "ends_at": r.get::<_, Option<i64>>(2)?.map(rfc3339)}),
                )
            },
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(Value::Null)
}

/// `GET /v1/ladders`.
pub async fn ladders(State(st): State<AppState>, _a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.ratings)?;
    let season = crate::db::current_season(&core.db, core.now)?.0;
    let tiers: Vec<Value> = (0..core.cfg.rating.tiers.len()).map(|i| tier_json(&core, i)).collect();
    let items: Vec<Value> = core
        .cfg
        .ladders
        .iter()
        .map(|l| {
            let queues: Vec<&str> = core
                .cfg
                .queues
                .iter()
                .filter(|q| q.ladder.as_deref() == Some(&l.id))
                .map(|q| q.id.as_str())
                .collect();
            json!({"id": l.id, "name": l.name, "description": l.description, "queues": queues,
                   "season": season_json(&core, &season), "tiers": tiers,
                   "leaderboard": l.leaderboard && core.cfg.features.leaderboards})
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

/// `GET /v1/seasons`.
pub async fn seasons(State(st): State<AppState>, _a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.ratings)?;
    crate::db::current_season(&core.db, core.now)?;
    let mut s = core
        .db
        .prepare("SELECT id, name, starts_at, ends_at FROM seasons ORDER BY number DESC")?;
    let items: Vec<Value> = s
        .query_map([], |r| {
            let ends: Option<i64> = r.get(3)?;
            Ok(json!({"id": r.get::<_, String>(0)?, "name": {"en": r.get::<_, String>(1)?}, "starts_at": rfc3339(r.get(2)?),
                      "ends_at": ends.map(rfc3339), "current": ends.is_none()}))
        })?
        .flatten()
        .collect();
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
pub struct SeasonQuery {
    season: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /v1/users/{user_id}/ratings`.
pub async fn user_ratings(
    State(st): State<AppState>,
    _a: Authed,
    Path(user): Path<String>,
    Query(q): Query<SeasonQuery>,
) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.ratings)?;
    if !core.user_exists(&user) {
        return Err(ApiError::not_found("no such user"));
    }
    let season = match q.season {
        Some(s) => s,
        None => crate::db::current_season(&core.db, core.now)?.0,
    };
    let items: Vec<Value> = core
        .cfg
        .ladders
        .iter()
        .filter_map(|l| load_row(&core, &user, &l.id, &season).map(|r| view(&core, &l.id, &season, &r)))
        .collect();
    Ok(Json(json!({"items": items})))
}

fn page(q: &SeasonQuery) -> ApiResult<(usize, usize)> {
    let limit = match &q.limit {
        None => 20,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| ApiError::invalid("/limit", "1 to 100"))?,
    };
    let offset = match &q.cursor {
        None => 0,
        Some(c) => c
            .strip_prefix("o")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n <= 1_000_000)
            .ok_or_else(|| ApiError::bad_request("bad_cursor", "unknown cursor"))?,
    };
    Ok((offset, limit))
}

/// `GET /v1/ladders/{ladder_id}/leaderboard`.
pub async fn leaderboard(
    State(st): State<AppState>,
    a: Authed,
    Path(ladder): Path<String>,
    Query(q): Query<SeasonQuery>,
) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.leaderboards)?;
    if core.cfg.ladder(&ladder).is_none_or(|l| !l.leaderboard) {
        return Err(ApiError::not_found("no such ladder"));
    }
    let season = match &q.season {
        Some(s) => s.clone(),
        None => crate::db::current_season(&core.db, core.now)?.0,
    };
    let (offset, limit) = page(&q)?;
    let p = &core.cfg.rating;
    let sql = "SELECT r.user_id, r.rating, r.deviation, r.volatility, r.games, r.wins, r.losses FROM ratings r JOIN users u ON u.id = r.user_id
               WHERE r.ladder = ?1 AND r.season_id = ?2 AND r.games >= ?3 AND u.show_on_leaderboards = 1
               ORDER BY r.rating - ?4 * r.deviation DESC, r.user_id LIMIT ?5 OFFSET ?6";
    let mut s = core.db.prepare(sql)?;
    let rows: Vec<(String, Rating, i64)> = s
        .query_map(
            params![
                ladder,
                season,
                p.placement_games,
                p.display_deviations,
                (limit + 1) as i64,
                offset as i64
            ],
            |r| {
                Ok((
                    r.get(0)?,
                    Rating {
                        rating: r.get(1)?,
                        deviation: r.get(2)?,
                        volatility: r.get(3)?,
                        games: r.get(4)?,
                    },
                    r.get(5)?,
                ))
            },
        )?
        .flatten()
        .collect();
    let more = rows.len() > limit;
    let cell = |rank: usize, user: &str, r: &Rating, wins: i64| {
        let (value, tier) = shown(&core, r);
        json!({"user_id": user, "cells": {"rank": rank, "player": core.user_json(user), "tier": tier,
               "rating": value, "games": r.games, "wins": wins}})
    };
    let items: Vec<Value> = rows
        .iter()
        .take(limit)
        .enumerate()
        .map(|(i, (u, r, w))| cell(offset + i + 1, u, r, *w))
        .collect();
    // The caller's own row.
    let you = load_row(&core, &a.user, &ladder, &season).filter(|r| r.r.games >= p.placement_games).map(|row| {
        let shown_v = row.r.shown(p);
        let better: i64 = core
            .db
            .query_row(
                "SELECT COUNT(*) FROM ratings r JOIN users u ON u.id = r.user_id WHERE r.ladder = ?1 AND r.season_id = ?2
                 AND r.games >= ?3 AND u.show_on_leaderboards = 1 AND r.rating - ?4 * r.deviation > ?5",
                params![ladder, season, p.placement_games, p.display_deviations, shown_v],
                |r| r.get(0),
            )
            .unwrap_or(0);
        cell(better as usize + 1, &a.user, &row.r, row.wins)
    });
    Ok(Json(json!({
        "ladder": ladder,
        "season": season,
        "columns": [
            {"key": "rank", "label": {"en": "#"}, "type": "rank"},
            {"key": "player", "label": {"en": "Player", "fr": "Joueur"}, "type": "player"},
            {"key": "tier", "label": {"en": "Tier", "fr": "Rang"}, "type": "tier"},
            {"key": "rating", "label": {"en": "Rating", "fr": "Cote"}, "type": "number"},
            {"key": "games", "label": {"en": "Matches", "fr": "Matchs"}, "type": "number"},
            {"key": "wins", "label": {"en": "Wins", "fr": "Victoires"}, "type": "number"},
        ],
        "items": items,
        "next_cursor": more.then(|| format!("o{}", offset + limit)),
        "you": you,
    })))
}

/// Whether `viewer` may see `user`'s statistics and history.
fn visible(core: &Core, viewer: &str, user: &str) -> ApiResult<()> {
    let public: Option<i64> = core
        .db
        .query_row("SELECT public_profile FROM users WHERE id = ?1", [user], |r| r.get(0))
        .optional()?;
    match public {
        None => Err(ApiError::not_found("no such user")),
        Some(0) if viewer != user && !core.are_friends(viewer, user) => Err(ApiError::forbidden(
            "private_profile",
            "this player's statistics are private",
        )),
        _ => Ok(()),
    }
}

/// `GET /v1/users/{user_id}/stats`.
pub async fn stats(State(st): State<AppState>, a: Authed, Path(user): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.stats)?;
    visible(&core, &a.user, &user)?;
    let mut s = core.db.prepare(
        "SELECT m.origin_kind, m.origin_id, p.result, p.stats FROM match_players p JOIN matches m ON m.id = p.match_id
         WHERE p.user_id = ?1 AND m.state = 'completed' ORDER BY m.ended_at DESC LIMIT 10000",
    )?;
    let rows: Vec<(String, String, Option<String>, Option<String>)> = s
        .query_map([&user], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .flatten()
        .collect();
    #[derive(Default)]
    struct Acc {
        matches: i64,
        wins: i64,
        losses: i64,
        sums: [i64; 5],
    }
    let mut groups: Vec<(String, Value, Acc)> = vec![(
        "all".into(),
        json!({"en": "All matches", "fr": "Tous les matchs"}),
        Acc::default(),
    )];
    for (kind, origin, result, line) in &rows {
        let key = if kind == "room" {
            "rooms".to_string()
        } else {
            format!("queue:{origin}")
        };
        if !groups.iter().any(|g| g.0 == key) {
            let label = if kind == "room" {
                json!({"en": "Private rooms", "fr": "Salons privés"})
            } else {
                core.cfg
                    .queue(origin)
                    .map(|q| json!(q.name))
                    .unwrap_or_else(|| json!({"en": origin}))
            };
            groups.push((key.clone(), label, Acc::default()));
        }
        let line: Value = line
            .as_deref()
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or(Value::Null);
        for g in groups.iter_mut().filter(|g| g.0 == "all" || g.0 == key) {
            g.2.matches += 1;
            g.2.wins += (result.as_deref() == Some("win")) as i64;
            g.2.losses += (result.as_deref() == Some("loss")) as i64;
            for (k, f) in ["paint", "kills", "assists", "deaths", "specials"].iter().enumerate() {
                g.2.sums[k] += line[f].as_i64().unwrap_or(0);
            }
        }
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(key, label, a)| {
            let rate = if a.matches > 0 { format!("{}%", (a.wins * 100 + a.matches / 2) / a.matches) } else { "-".into() };
            let avg = |x: i64| if a.matches > 0 { json!(((x as f64 / a.matches as f64) * 10.0).round() / 10.0) } else { json!(0) };
            json!({"key": key, "label": label, "fields": [
                {"key": "matches", "label": {"en": "Matches", "fr": "Matchs"}, "value": a.matches},
                {"key": "wins", "label": {"en": "Wins", "fr": "Victoires"}, "value": a.wins},
                {"key": "losses", "label": {"en": "Losses", "fr": "Défaites"}, "value": a.losses},
                {"key": "win_rate", "label": {"en": "Win rate", "fr": "Taux de victoire"}, "value": rate},
                {"key": "paint_avg", "label": {"en": "Paint per match", "fr": "Encrage par match"}, "value": avg(a.sums[0])},
                {"key": "kills_avg", "label": {"en": "Eliminations per match", "fr": "Éliminations par match"}, "value": avg(a.sums[1])},
                {"key": "assists_avg", "label": {"en": "Assists per match", "fr": "Aides par match"}, "value": avg(a.sums[2])},
                {"key": "deaths_avg", "label": {"en": "Deaths per match", "fr": "Morts par match"}, "value": avg(a.sums[3])},
                {"key": "specials_avg", "label": {"en": "Specials per match", "fr": "Spéciales par match"}, "value": avg(a.sums[4])},
            ]})
        })
        .collect();
    Ok(Json(json!({"user_id": user, "groups": groups})))
}

/// `GET /v1/users/{user_id}/matches`.
pub async fn history(
    State(st): State<AppState>,
    a: Authed,
    Path(user): Path<String>,
    Query(q): Query<SeasonQuery>,
) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(core.cfg.features.stats)?;
    visible(&core, &a.user, &user)?;
    let (offset, limit) = page(&q)?;
    let mut s = core.db.prepare(
        "SELECT m.id, m.ended_at, m.state, m.origin_kind, m.origin_id, m.rules, p.team, p.result, p.stats, p.rating_delta, m.outcome
         FROM match_players p JOIN matches m ON m.id = p.match_id
         WHERE p.user_id = ?1 AND m.state IN ('completed', 'void') ORDER BY m.ended_at DESC, m.id DESC LIMIT ?2 OFFSET ?3",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<i64>,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<String>,
    )> = s
        .query_map(params![user, (limit + 1) as i64, offset as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?
        .flatten()
        .collect();
    let more = rows.len() > limit;
    let items: Vec<Value> = rows
        .into_iter()
        .take(limit)
        .map(|(id, ended, state, kind, origin, rules, team, result, stats, delta, outcome)| {
            let rules: Value = serde_json::from_str(&rules).unwrap_or(Value::Null);
            let outcome: Value = outcome.and_then(|o| serde_json::from_str(&o).ok()).unwrap_or(Value::Null);
            let mut o = json!({"kind": kind});
            o[if kind == "room" { "room_id" } else { "queue_id" }] = json!(origin);
            let mut item = json!({
                "match_id": id, "ended_at": ended.map(rfc3339), "state": state, "origin": o,
                "rules": {"teams": rules["teams"], "mode": rules["mode"], "stage": rules["stage"], "rated": rules["rated"], "ladder": rules["ladder"]},
                "team": team, "result": result.unwrap_or_else(|| "none".into()),
                "stage": outcome["record"]["stage_scene"],
                "stats": stats.and_then(|s| serde_json::from_str::<Value>(&s).ok()).map(|mut v| {
                    if let Some(m) = v.as_object_mut() { m.remove("name"); m.remove("team"); }
                    v
                }),
                "rating_delta": delta.map(|d| d.round() as i64),
            });
            // 1.1.11: a match won or lost by forfeit says why (`outcome.forfeit`).
            if outcome["forfeit"].is_object() {
                item["forfeit"] = outcome["forfeit"].clone();
            }
            item
        })
        .collect();
    Ok(Json(
        json!({"items": items, "next_cursor": more.then(|| format!("o{}", offset + limit))}),
    ))
}
