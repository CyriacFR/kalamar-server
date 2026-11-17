//! The host of a match whose host is not the room owner (a queue's, a room's with `rooms.host = "random"`), from a
//! hidden score of how each player's game hosted its last matches (`users.host_score`, migration `0009_host_score`).
//!
//! The host's game runs every step of the room before the start (its settings, "ready", the check of the players, the
//! team screen) while every other player waits for it, and a host that fails takes the match down with it. So:
//! - **the score** is [`DEFAULT`] for everyone at first, from [`MIN`] to [`MAX`], and moves only after a match the
//!   player hosted: when every player's game has started the match ([`started`]), up [`FAST`] when that came within
//!   [`fast_s`] of the last player's launch, down [`SLOW`] past [`slow_s`]; down [`FAILED`] when the host's game
//!   crashed or the player closed it or left, before or during the match, and when a match is voided before every
//!   player reached it with the host as the cause ([`finished`]);
//! - **the draw** ([`candidates`]): the better half of the match's players by score (ties at the cut included, so a
//!   match of new players is a draw among all), of them those at [`DEFAULT`] or above (when none is, the better half
//!   as it is), and one of those at random. So the best player is not always the host (a 2v2 draws between its two
//!   best), and in a 4v4 where only three players are reliable the draw is among those three.
//!
//! The score is never shown and gives nothing in a match's result; a player who fails on purpose only hosts less.

use crate::state::{Core, LiveMatch};
use crate::util::random_index;
use kalamar_protocol::Role;
use rusqlite::params;
use serde_json::Value;

pub const DEFAULT: i64 = 100;
pub const MIN: i64 = 0;
pub const MAX: i64 = 200;
/// Score changes after a hosted match.
pub const FAST: i64 = 5;
pub const SLOW: i64 = -5;
pub const FAILED: i64 = -25;

/// Seconds from the last player's launch to every player's start that count as fast, and beyond which it is slow.
/// shortcut: measured with two players only (28 to 31 s at 60 frames a second, Kalamar 2026-11), the slope per player
/// is an estimate; calibrate from real matches of 8.
pub fn fast_s(players: usize) -> i64 {
    35 + 2 * players as i64
}
pub fn slow_s(players: usize) -> i64 {
    60 + 4 * players as i64
}

/// The host's own errors that say its game failed to host (the engine's and the launcher's codes).
const HOST_ERRORS: &[&str] = &[
    "boot-timeout",
    "list-timeout",
    "create-failed",
    "rules-not-applied",
    "teams-not-set",
    "network-unavailable",
    "save-unavailable",
    "launcher-failed",
];
/// The host's own errors that say its game went away: it crashed (`launcher-crashed`) or the player closed it or left
/// the match (`left-by-player`, `decision::LEFT_BY_PLAYER`), before or during the match.
const HOST_GONE: &[&str] = &["launcher-crashed", kalamar_protocol::decision::LEFT_BY_PLAYER];
/// A guest's errors that point at the host.
const GUEST_ERRORS_ABOUT_HOST: &[&str] = &["room-not-found", "join-failed", "host-left"];

pub fn score(core: &Core, user: &str) -> i64 {
    core.db
        .query_row("SELECT host_score FROM users WHERE id = ?1", [user], |r| r.get(0))
        .unwrap_or(DEFAULT)
}

fn add(core: &Core, user: &str, delta: i64) {
    if delta != 0 {
        let _ = core.db.execute(
            "UPDATE users SET host_score = MAX(?3, MIN(?4, host_score + ?2)) WHERE id = ?1",
            params![user, delta, MIN, MAX],
        );
    }
}

/// The players (by index) the host is drawn from.
pub fn candidates(scores: &[i64]) -> Vec<usize> {
    if scores.is_empty() {
        return Vec::new();
    }
    let mut sorted = scores.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let cut = sorted[scores.len().div_ceil(2) - 1];
    let half: Vec<usize> = (0..scores.len()).filter(|&i| scores[i] >= cut).collect();
    let good: Vec<usize> = half.iter().copied().filter(|&i| scores[i] >= DEFAULT).collect();
    if good.is_empty() { half } else { good }
}

/// The index in `users` of the match's host (0 when `users` is empty).
pub fn pick(core: &Core, users: &[&str]) -> usize {
    let scores: Vec<i64> = users.iter().map(|u| score(core, u)).collect();
    let c = candidates(&scores);
    if c.is_empty() { 0 } else { c[random_index(c.len())] }
}

fn host_user(m: &LiveMatch) -> Option<&str> {
    m.seats
        .iter()
        .find(|s| s.role == Role::Host)
        .and_then(|s| s.user_id.as_deref())
}

/// Every player's game has just started the match (`STARTED_STATES`), at `now`.
pub fn started(core: &Core, m: &LiveMatch, now: i64) {
    let Some(host) = host_user(m) else { return };
    let Some(last_launch) = m.seats.iter().filter_map(|s| s.launched_at).max() else {
        return;
    };
    let took = now - last_launch;
    let n = m.seats.len();
    add(
        core,
        host,
        if took <= fast_s(n) {
            FAST
        } else if took > slow_s(n) {
            SLOW
        } else {
            0
        },
    );
}

/// The match ended; `counted`: its result counts. Lowers the host's score when its own launcher reported that its game
/// crashed or that the player closed it or left, at any time, whatever the decision; or, for a void match that not
/// every player reached, when the host did not launch, reported a hosting error, or was named by a guest's error.
pub fn finished(core: &Core, m: &LiveMatch, counted: bool) {
    let Some(host) = host_user(m) else { return };
    let error = |s: &crate::state::Seat| {
        s.report
            .as_ref()
            .and_then(|r| r.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let host_seat = m.seats.iter().find(|s| s.role == Role::Host);
    if host_seat
        .and_then(error)
        .is_some_and(|e| HOST_GONE.contains(&e.as_str()))
    {
        add(core, host, FAILED);
        return;
    }
    if counted || m.seats.iter().all(|s| s.saw_in_match) {
        return;
    }
    let host_failed = host_seat.is_some_and(|s| {
        let others_launched = m.seats.iter().any(|o| o.role != Role::Host && o.launched_at.is_some());
        (s.launched_at.is_none() && s.report.is_none() && others_launched)
            || error(s).is_some_and(|e| HOST_ERRORS.contains(&e.as_str()))
    });
    let named_by_guest = m
        .seats
        .iter()
        .filter(|s| s.role != Role::Host)
        .any(|s| error(s).is_some_and(|e| GUEST_ERRORS_ABOUT_HOST.contains(&e.as_str())));
    if host_failed || named_by_guest {
        add(core, host, FAILED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_players_are_a_draw_among_all() {
        assert_eq!(candidates(&[100; 4]), vec![0, 1, 2, 3]);
        assert!(candidates(&[]).is_empty());
    }

    #[test]
    fn the_better_half_draws_and_the_best_does_not_always_host() {
        // 2v2: the two best, never the third or fourth
        assert_eq!(candidates(&[130, 90, 140, 100]), vec![0, 2]);
        // 1v1: the better one
        assert_eq!(candidates(&[105, 120]), vec![1]);
    }

    #[test]
    fn only_reliable_players_among_the_better_half() {
        // 4v4: three reliable players, five below the default: a draw among the three, not four
        assert_eq!(candidates(&[120, 40, 110, 60, 105, 30, 95, 70]), vec![0, 2, 4]);
        // nobody reliable: the better half as it is
        assert_eq!(candidates(&[40, 60, 20, 80]), vec![1, 3]);
    }

    #[test]
    fn the_thresholds_grow_with_the_players() {
        assert!(fast_s(2) < slow_s(2) && fast_s(8) > fast_s(2) && slow_s(8) > slow_s(2));
    }
}
