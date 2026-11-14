//! Reloading the configuration while the service runs (`kalamar-service admin reload`, or SIGHUP on Unix).
//!
//! The new file is read, normalized and validated exactly like at start; an invalid one changes nothing. Sessions,
//! event streams, rooms, parties and running matches go on: a match keeps the result policy it was created with,
//! a series too. What only a restart can change (the listening address, the API path, the database, the logins, the
//! rate limits, the webhooks, the relay this service runs and, while that waits, the `[[relays]]`) keeps its running
//! value and is named in the report. Relay secrets are read again (the embedded relay's stays the one it started
//! with). Tickets of queues that no longer exist are canceled, and every connected launcher gets `meta.changed`
//! (with the new `revision`, protocol 1.1.9) so that it reads `GET /v1/meta` again.

use std::sync::Arc;

use rusqlite::params;
use serde_json::json;

use crate::config::Config;
use crate::state::AppState;

/// What a reload did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// `meta.revision` after the reload.
    pub revision: u64,
    /// Parts of the file that differ from the running configuration and wait for a restart.
    pub restart_needed: Vec<String>,
    /// Searches canceled because their queue is gone.
    pub cancelled_tickets: usize,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut s = format!("configuration reloaded (revision {})", self.revision);
        if self.cancelled_tickets > 0 {
            s.push_str(&format!(
                "; {} search(es) cancelled: their queue is gone",
                self.cancelled_tickets
            ));
        }
        if !self.restart_needed.is_empty() {
            s.push_str(&format!(
                "; NOT applied, restart the service for: {}",
                self.restart_needed.join(", ")
            ));
        }
        s
    }
}

/// `meta.revision` after a change of the offer at `now` (seconds): the clock in milliseconds, or one more than
/// `previous` when that is larger (several changes within a second, a clock set back). It is therefore larger after
/// a restart than before it, as long as the server's clock moved forward in between; launchers only compare it.
pub fn revision_after(previous: u64, now: i64) -> u64 {
    previous.saturating_add(1).max((now.max(0) as u64).saturating_mul(1000))
}

/// Puts `new` in force. `Err`: why it was refused (nothing changed).
pub fn apply(st: &AppState, mut new: Config) -> Result<Report, String> {
    new.normalize();
    let errors = new.validate();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let running = st.cfg();
    let mut restart_needed: Vec<String> = new
        .keep_restart_only(&running)
        .into_iter()
        .map(str::to_string)
        .collect();
    // The running logins and limits with the new offering must still make a valid whole.
    let errors = new.validate();
    if !errors.is_empty() {
        return Err(format!(
            "with the parts that need a restart ({}) kept as they run:\n{}",
            restart_needed.join(", "),
            errors.join("\n")
        ));
    }
    // Relay secrets are read here, outside the lock; the entries that name the embedded relay get the very secret
    // it runs with.
    let running_keys = st.lock().relay_keys.clone();
    let keys = crate::relays::load_keys(&new, running_keys.embedded.as_ref()).map_err(|e| e.join("\n"))?;
    if let (Some(e), Some(f)) = (&running_keys.embedded, &new.embedded_relay.secret_file)
        && kalamar_relay::roomauth::RoomSecret::from_file(f).map_or(true, |s| &s != e)
    {
        restart_needed.push(
            "embedded_relay.secret_file (the file changed or cannot be read: the relay this service runs, and the \
             [[relays]] entries that name it, keep the secret they started with)"
                .into(),
        );
    }
    let new = Arc::new(new);
    let mut core = st.lock();
    // A tournament in progress keeps its shape: its bracket was built for it.
    for (id, a) in &core.live.activities {
        if a.state != "running" {
            continue;
        }
        let before = running.activities.iter().find(|x| &x.id == id);
        let after = new
            .activities
            .iter()
            .find(|x| &x.id == id)
            .filter(|_| new.features.activities);
        if let Some(b) = before
            && after.is_none_or(|n| n.kind != b.kind || n.size != b.size)
        {
            return Err(format!(
                "activities.{id} is running: its kind and size cannot change, and it cannot be removed, until it ends"
            ));
        }
    }
    core.cfg = new.clone();
    core.relay_keys = Arc::new(keys);
    *st.current_cfg.write().unwrap_or_else(|p| p.into_inner()) = new.clone();
    // Searches in queues that are gone.
    let orphans: Vec<String> = core
        .live
        .tickets
        .values()
        .filter(|t| t.match_id.is_none() && (!new.features.queues || new.queue(&t.queue_id).is_none()))
        .filter_map(|t| t.members.first().cloned())
        .collect();
    for u in &orphans {
        crate::api::queues::cancel_ticket_of(&mut core, u, "queue_closed");
    }
    core.live.population = None;
    core.live.meta_revision = revision_after(core.live.meta_revision, core.now);
    let revision = core.live.meta_revision;
    let users: Vec<String> = core.live.user_streams.keys().cloned().collect();
    core.push_many(&users, "meta.changed", &json!({"revision": revision}));
    let report = Report {
        revision,
        restart_needed,
        cancelled_tickets: orphans.len(),
    };
    let now = core.now;
    if let Err(e) = crate::db::audit(&core.db, now, "config-reload", None, Some(&report.summary())) {
        tracing::error!(error = %e, "audit");
    }
    Ok(report)
}

/// Reads the service's configuration file again and applies it.
pub fn from_file(st: &AppState) -> Result<Report, String> {
    let path = st
        .config_path
        .as_ref()
        .ok_or("this service was not started from a configuration file")?;
    let text = std::fs::read_to_string(path.as_path()).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    cfg.normalize();
    let missing = cfg.check_secrets();
    if !missing.is_empty() {
        return Err(missing.join("\n"));
    }
    apply(st, cfg)
}

/// Reloads from the file and logs the outcome (SIGHUP, the command line's requests).
pub fn from_file_logged(st: &AppState) -> (bool, String) {
    match from_file(st) {
        Ok(r) => {
            let s = r.summary();
            tracing::info!(revision = r.revision, restart_needed = %r.restart_needed.join(","), "configuration reloaded");
            (true, s)
        }
        Err(e) => {
            tracing::error!(error = %e, "configuration NOT reloaded: the running one stays in force");
            (false, e)
        }
    }
}

/// Serves the requests `admin reload` left in the database (called by the ticker every second; one cheap query).
/// A request is served once: when its answer cannot be written (a locked or read-only database), the answer is kept
/// in memory and written at a later tick, without reloading again.
pub fn poll(st: &AppState) {
    let (fresh, waiting): (Vec<i64>, bool) = {
        let core = st.lock();
        let Ok(mut q) = core
            .db
            .prepare_cached("SELECT id FROM control WHERE kind = 'reload' AND done_at IS NULL")
        else {
            return;
        };
        let pending: Vec<i64> = q
            .query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        let served = &core.live.reloads_served;
        (
            pending
                .into_iter()
                .filter(|id| !served.iter().any(|(s, _, _)| s == id))
                .collect(),
            !served.is_empty(),
        )
    };
    if fresh.is_empty() && !waiting {
        return;
    }
    let answer = (!fresh.is_empty()).then(|| from_file_logged(st));
    let mut core = st.lock();
    if let Some((ok, detail)) = answer {
        for id in fresh {
            core.live.reloads_served.push((id, ok, detail.clone()));
        }
    }
    let now = core.now;
    let served = std::mem::take(&mut core.live.reloads_served);
    let mut left = Vec::new();
    for (id, ok, detail) in served {
        if let Err(e) = core.db.execute(
            "UPDATE control SET done_at = ?1, ok = ?2, detail = ?3 WHERE id = ?4",
            params![now, ok as i64, detail, id],
        ) {
            tracing::warn!(error = %e, request = id, "the answer to `admin reload` cannot be written yet");
            left.push((id, ok, detail));
        }
    }
    core.live.reloads_served = left;
}

/// The command line's side: leaves a request and waits for the running service's answer.
pub fn request(db: &rusqlite::Connection, now: i64, wait: std::time::Duration) -> Result<String, String> {
    request_kind(db, "reload", now, wait)
}

/// [`request`] of any kind: `reload`, or `relays` (`admin relays`, served by `relays::serve_admin`).
pub fn request_kind(
    db: &rusqlite::Connection,
    kind: &str,
    now: i64,
    wait: std::time::Duration,
) -> Result<String, String> {
    let e = |x: rusqlite::Error| x.to_string();
    db.execute(
        "DELETE FROM control WHERE done_at IS NOT NULL AND done_at < ?1",
        [now - 86_400],
    )
    .map_err(e)?;
    db.execute(
        "INSERT INTO control (kind, requested_at) VALUES (?1, ?2)",
        rusqlite::params![kind, now],
    )
    .map_err(e)?;
    let id = db.last_insert_rowid();
    let until = std::time::Instant::now() + wait;
    loop {
        let row: Option<(i64, String)> = db
            .query_row(
                "SELECT ok, COALESCE(detail, '') FROM control WHERE id = ?1 AND done_at IS NOT NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            Some((1, detail)) => return Ok(detail),
            Some((_, detail)) => return Err(format!("the service refused the configuration:\n{detail}")),
            None if std::time::Instant::now() >= until => {
                let _ = db.execute("DELETE FROM control WHERE id = ?1 AND done_at IS NULL", [id]);
                return Err(
                    "no running service answered: is it running, with this configuration's database? (nothing changed)"
                        .into(),
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_revision_grows_within_a_run_and_from_one_start_to_the_next() {
        let t = 1_800_000_000;
        let start = revision_after(0, t);
        assert_eq!(start, 1_800_000_000_000);
        let r1 = revision_after(start, t);
        let r2 = revision_after(r1, t);
        assert!(start < r1 && r1 < r2, "several reloads within one second");
        // A restart a few seconds later starts above every revision of the previous run.
        assert!(revision_after(0, t + 5) > r2);
        // A clock set back does not make it smaller.
        assert_eq!(revision_after(r2, t - 3600), r2 + 1);
        assert!(start <= (1u64 << 53), "a JSON number launchers read exactly");
    }
}
