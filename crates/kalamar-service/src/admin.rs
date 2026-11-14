//! The operator's command line: accounts, bans, reports, seasons, export and erasure. It works on the database
//! directly (SQLite in WAL mode allows it while the service runs); the running service applies bans and erasures
//! within 30 seconds (its ticker removes such players from the live state).

use std::sync::Arc;

use clap::Subcommand;
use rusqlite::{OptionalExtension, params};
use serde_json::json;

use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::state::Core;
use crate::util::{parse_duration, parse_rfc3339, rfc3339};

#[derive(Subcommand, Debug)]
pub enum AdminCmd {
    /// List accounts (newest first).
    Users {
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Create an account and print a session (access and refresh tokens), for test accounts or the conformance
    /// suite's `--tokens`.
    UserCreate {
        #[arg(long)]
        name: String,
    },
    /// Ban an account: `--for 7d`, `--until <RFC 3339>` or permanently.
    Ban {
        user: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "for")]
        duration: Option<String>,
        #[arg(long)]
        until: Option<String>,
    },
    Unban {
        user: String,
    },
    /// List moderation reports (open ones unless `--all`).
    Reports {
        #[arg(long)]
        all: bool,
    },
    ResolveReport {
        id: String,
        #[arg(long)]
        note: String,
    },
    Seasons,
    /// End the current season and start the next one; ratings carry over with the configured soft reset and every
    /// player plays placement again.
    SeasonRoll {
        #[arg(long)]
        name: Option<String>,
    },
    /// Print (or write) everything held about an account.
    Export {
        user: String,
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Erase an account (cannot be undone).
    Erase {
        user: String,
        #[arg(long)]
        yes: bool,
    },
    /// Make a one-time code for an e-mail and password account to choose a new password (form "Forgotten or new
    /// password"); shown once, valid `auth.password.operator_code_ttl_s`. Give it to the player privately.
    PasswordReset {
        user: String,
    },
    /// Make the running service read this configuration file again, without a restart: sessions and matches go on.
    /// An invalid file changes nothing; what only a restart can change is listed.
    Reload {
        /// Seconds to wait for the running service's answer.
        #[arg(long, default_value_t = 10)]
        wait: u64,
    },
    /// Show each relay as the running service sees it: reachable or not, bound or open, and the load a bound relay
    /// announces (sessions, rooms, the caps, each worker's load, the rates; protocol 1.1.13), whether it has room.
    Relays {
        /// Seconds to wait for the running service's answer.
        #[arg(long, default_value_t = 10)]
        wait: u64,
    },
    /// Show a match with its reports and decision.
    Match {
        id: String,
    },
    /// Make a token for the operator API (bot, statistics site, referee); prints the token once and the lines to
    /// add to the configuration.
    OperatorToken {
        #[arg(long)]
        name: String,
        /// Comma-separated: read, results, moderation, activities.
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
    },
}

pub fn run(cfg: Config, cmd: AdminCmd) -> Result<(), String> {
    // Never migrates: a database still served by an older service is left alone (`db::open_current`).
    let db = crate::db::open_current(&cfg.server.database)?;
    let secret = match std::fs::read_to_string(&cfg.server.secret_file) {
        Ok(s) => crate::util::unhex(s.trim()).ok_or("bad secret file")?,
        Err(_) => {
            return Err(format!(
                "{}: start the service once to create the secret",
                cfg.server.secret_file.display()
            ));
        }
    };
    let now = SystemClock.now();
    let mut core = Core::new(Arc::new(cfg), db, secret, now);
    let e = |x: rusqlite::Error| x.to_string();
    match cmd {
        AdminCmd::Reload { wait } => {
            let missing = core.cfg.check_secrets();
            if !missing.is_empty() {
                return Err(missing.join("\n"));
            }
            let answer = crate::reload::request(&core.db, now, std::time::Duration::from_secs(wait.clamp(1, 120)))?;
            println!("{answer}");
        }
        AdminCmd::Relays { wait } => {
            let answer = crate::reload::request_kind(
                &core.db,
                "relays",
                now,
                std::time::Duration::from_secs(wait.clamp(1, 120)),
            )?;
            print!("{answer}");
        }
        AdminCmd::Users { search, limit } => {
            let pattern = format!("%{}%", search.unwrap_or_default());
            let mut s = core
                .db
                .prepare("SELECT id, display_name, created_at, banned_until FROM users WHERE display_name LIKE ?1 OR id = ?2 ORDER BY created_at DESC LIMIT ?3")
                .map_err(e)?;
            let rows = s
                .query_map(params![pattern, pattern.trim_matches('%'), limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                })
                .map_err(e)?;
            for (id, name, at, ban) in rows.flatten() {
                let ban = match ban {
                    Some(u) if u > now => {
                        if u == i64::MAX {
                            " [banned]".to_string()
                        } else {
                            format!(" [banned until {}]", rfc3339(u))
                        }
                    }
                    _ => String::new(),
                };
                println!("{id}  {name}  {}{ban}", rfc3339(at));
            }
        }
        AdminCmd::UserCreate { name } => {
            let subject = crate::util::new_id("admin");
            let (user, _) = crate::auth::login_identity(&mut core, "admin", &subject, &name).map_err(|x| x.message)?;
            let s = crate::auth::issue_session(&mut core, &user, true).map_err(|x| x.message)?;
            crate::db::audit(&core.db, now, "user-create", Some(&user), None).map_err(e)?;
            println!("{}", serde_json::to_string_pretty(&json!({"user_id": user, "access_token": s["access_token"], "refresh_token": s["refresh_token"], "access_expires_at": s["access_expires_at"]})).unwrap_or_default());
        }
        AdminCmd::Ban {
            user,
            reason,
            duration,
            until,
        } => {
            let until = match (duration, until) {
                (Some(d), _) => now + parse_duration(&d).ok_or("--for: like 30m, 12h, 7d")?,
                (None, Some(u)) => parse_rfc3339(&u).ok_or("--until: RFC 3339")?,
                (None, None) => i64::MAX,
            };
            let n = core
                .db
                .execute(
                    "UPDATE users SET banned_until = ?1, ban_reason = ?2 WHERE id = ?3",
                    params![until, reason, user],
                )
                .map_err(e)?;
            if n == 0 {
                return Err("no such user".into());
            }
            crate::db::audit(&core.db, now, "ban", Some(&user), Some(&reason)).map_err(e)?;
            println!(
                "banned {user} {}",
                if until == i64::MAX {
                    "permanently".to_string()
                } else {
                    format!("until {}", rfc3339(until))
                }
            );
        }
        AdminCmd::Unban { user } => {
            let n = core
                .db
                .execute(
                    "UPDATE users SET banned_until = NULL, ban_reason = NULL WHERE id = ?1",
                    [&user],
                )
                .map_err(e)?;
            if n == 0 {
                return Err("no such user".into());
            }
            crate::db::audit(&core.db, now, "unban", Some(&user), None).map_err(e)?;
            println!("unbanned {user}");
        }
        AdminCmd::Reports { all } => {
            let sql = if all {
                "SELECT id, reporter, target, match_id, category, comment, created_at, status, resolution FROM reports ORDER BY created_at DESC"
            } else {
                "SELECT id, reporter, target, match_id, category, comment, created_at, status, resolution FROM reports WHERE status = 'open' ORDER BY created_at"
            };
            let mut s = core.db.prepare(sql).map_err(e)?;
            let rows = s
                .query_map([], |r| {
                    Ok(json!({"id": r.get::<_, String>(0)?, "reporter": r.get::<_, Option<String>>(1)?, "target": r.get::<_, Option<String>>(2)?,
                              "match_id": r.get::<_, Option<String>>(3)?, "category": r.get::<_, String>(4)?, "comment": r.get::<_, String>(5)?,
                              "created_at": rfc3339(r.get(6)?), "status": r.get::<_, String>(7)?, "resolution": r.get::<_, Option<String>>(8)?}))
                })
                .map_err(e)?;
            for r in rows.flatten() {
                println!("{r}");
            }
        }
        AdminCmd::ResolveReport { id, note } => {
            let n = core
                .db
                .execute(
                    "UPDATE reports SET status = 'resolved', resolution = ?1, resolved_at = ?2 WHERE id = ?3",
                    params![note, now, id],
                )
                .map_err(e)?;
            if n == 0 {
                return Err("no such report".into());
            }
            crate::db::audit(&core.db, now, "resolve-report", Some(&id), Some(&note)).map_err(e)?;
            println!("resolved {id}");
        }
        AdminCmd::Seasons => {
            crate::db::current_season(&core.db, now).map_err(e)?;
            let mut s = core
                .db
                .prepare("SELECT id, name, starts_at, ends_at FROM seasons ORDER BY number")
                .map_err(e)?;
            let rows = s
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                })
                .map_err(e)?;
            for (id, name, a, b) in rows.flatten() {
                println!(
                    "{id}  {name}  {} .. {}",
                    rfc3339(a),
                    b.map(rfc3339).unwrap_or_else(|| "current".into())
                );
            }
        }
        AdminCmd::SeasonRoll { name } => {
            let (old, _, _) = crate::db::current_season(&core.db, now).map_err(e)?;
            let p = core.cfg.rating.clone();
            let tx = core.db.unchecked_transaction().map_err(e)?;
            tx.execute("UPDATE seasons SET ends_at = ?1 WHERE id = ?2", params![now, old])
                .map_err(e)?;
            let number: i64 = tx
                .query_row("SELECT MAX(number) + 1 FROM seasons", [], |r| r.get(0))
                .map_err(e)?;
            let id = format!("s{number}");
            let name = name.unwrap_or_else(|| format!("Season {number}"));
            tx.execute(
                "INSERT INTO seasons (id, number, name, starts_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, number, name, now],
            )
            .map_err(e)?;
            let carried = tx
                .execute(
                    "INSERT INTO ratings (user_id, ladder, season_id, rating, deviation, volatility, games, wins, losses)
                     SELECT user_id, ladder, ?1, ?2 + (rating - ?2) * ?3, MAX(deviation, ?4), volatility, 0, 0, 0
                     FROM ratings WHERE season_id = ?5",
                    params![id, p.initial_rating, p.season_keep, p.season_deviation_floor, old],
                )
                .map_err(e)?;
            tx.commit().map_err(e)?;
            crate::db::audit(&core.db, now, "season-roll", Some(&id), None).map_err(e)?;
            println!("season {old} ended; {id} ({name}) started; {carried} ratings carried over");
        }
        AdminCmd::Export { user, out } => {
            let doc = crate::api::account::export_user(&core, &user).map_err(|x| x.message)?;
            if doc["account"].is_null() {
                return Err("no such user".into());
            }
            let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
            match out {
                Some(p) => std::fs::write(&p, text).map_err(|x| format!("{}: {x}", p.display()))?,
                None => println!("{text}"),
            }
            crate::db::audit(&core.db, now, "export", Some(&user), None).map_err(e)?;
        }
        AdminCmd::Erase { user, yes } => {
            if !yes {
                return Err("erasing cannot be undone: add --yes".into());
            }
            crate::api::account::erase_user(&mut core, &user).map_err(|x| x.message)?;
            println!("erased {user}");
        }
        AdminCmd::PasswordReset { user } => {
            if !core.user_exists(&user) {
                return Err("no such user".into());
            }
            let (code, until) = crate::auth::password::operator_code(&mut core, &user)?;
            crate::db::audit(&core.db, now, "password-reset", Some(&user), None).map_err(e)?;
            println!(
                "reset code for {user} (shown once): {code}
valid until {}. Give it to the player privately (not in a public channel); they type their e-mail address, this
code and a new password in the form \"Forgotten or new password\". Their other sessions end. A new code replaces
this one.",
                rfc3339(until)
            );
        }
        AdminCmd::OperatorToken { name, scopes } => {
            let token = crate::util::new_token("kop1");
            println!(
                "token (shown once): {token}

add to the configuration and restart:
{}",
                crate::operator::token_line(&name, &token, &scopes)
            );
        }
        AdminCmd::Match { id } => {
            let m = crate::matches::load(&core, &id)
                .map_err(|x| x.message)?
                .ok_or("no such match")?;
            let seats: Vec<serde_json::Value> = m
                .seats
                .iter()
                .map(|s| json!({"user_id": s.user_id, "member_name": s.member_name, "team": s.team.as_str(), "role": s.role.as_str(),
                                "accept": s.accept, "launched_at": s.launched_at.map(rfc3339), "status": s.status_state,
                                "saw_in_match": s.saw_in_match, "report": s.report}))
                .collect();
            let reason: Option<String> = core
                .db
                .query_row("SELECT cancel_reason FROM matches WHERE id = ?1", [&id], |r| r.get(0))
                .optional()
                .map_err(e)?
                .flatten();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"id": m.id, "state": m.state.as_str(), "rules": m.rules,
                "seats": seats, "outcome": m.outcome, "cancel_reason": reason}))
                .unwrap_or_default()
            );
        }
    }
    Ok(())
}
