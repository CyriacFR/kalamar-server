# Kalamar community protocol 1.1 — series

A **series** chains several matches between the same participants: best of 3, best of 5, first to N, a fixed number
of games, a rematch. Area id: `series`. Any origin can create one (a room, a queue, an activity).

## 1. Series object

`GET /v1/series/{series_id}` — *auth* (participants; others only an ended series, same object, when `stats` is
declared; else `404 not_found`).

```json
{
  "id": "sr_7k2...",
  "origin": {"kind": "activity", "activity_id": "spring-cup"},
  "format": {"kind": "best_of", "games": 3},
  "state": "running",
  "sides": [
    {"team": "alpha", "label": {"en": "Camille"}, "users": [{"id": "u_a", "display_name": "Camille"}], "wins": 1},
    {"team": "bravo", "label": {"en": "Lou"}, "users": [{"id": "u_b", "display_name": "Lou"}], "wins": 0}
  ],
  "games": [
    {"index": 1, "match_id": "m_1", "state": "completed", "winner": "alpha"},
    {"index": 2, "match_id": "m_2", "state": "ready", "winner": null}
  ],
  "winner": null,
  "created_at": "...", "ended_at": null
}
```

| Member | Meaning |
|---|---|
| `format.kind` | `kind` is always present; `best_of` (`games`: odd number; the first side to win more than half wins), `first_to` (`wins`), `fixed` (`games` played, most wins wins, draws possible). Unknown kind: show `games` and `sides[].wins` |
| `state` | `running`, `completed`, `void`. Unknown: `running` |
| `sides` | the two sides, with their team in the **first** game, labels, players and wins. Sides keep their identity across games even if the server swaps colors (`team` of a game is in each match) |
| `games` | the log of every game played, in order, void ones included; `index` is the position in this log (a replayed game gets the next index: each attempt is its own match with its own reports) |
| `winner` | `alpha` or `bravo` side (by `sides[].team`), or `null` |

Matches of a series carry `series: {"id", "index", "format"}` in the match object (additive member). Events:
`series.updated` (the series), plus the usual `match.*` events for each game.

1.1.6: the games of a series may be drafted ([draft.md](draft.md)): before every game (the previous game's winner
and loser known to the format, stages already played out), or once before the first game, whose picks in order are
the stages of the games. The series then MAY carry `draft`: `{"preset", "stages": [<the order of a once-per-series
draft>], "played": [<the stages of the decided games, in order>]}`.

## 2. Playing a series without restarting the game

With match contract **version 2** ([match-contract-2.md](match-contract-2.md#6-rounds-in-one-session)), a game may
stay open between the games of a series — a declared capability, used only when every participant's engine
declares it faster than a relaunch (`rounds_faster`) and the
server keeps the same participants, host and relay room. The next game's match object then carries `game.continue_from`: the id of the previous match. A launcher
whose game is still running that previous match (waiting for a command after its result) **sends the next round to
the running game** instead of starting a new process; otherwise it launches normally. Either way it reports the new
match as usual (`launch`, statuses, result). A launcher of contract version 1 always launches anew: series work the
same, only slower.

1.1.7: a series MAY carry `spectators`: `{"seats", "who", "users": [User]}`, the places in its stands and who sits there.
A place is taken for the series (`POST /v1/series/{series_id}/spectate`) and holds for every next game until it is left;
a change in the stands makes the next game a new game session (no `continue_from`) ([spectators.md](spectators.md#32-queues-activities-series)).

## 3. Rules for servers

- Every game is decided by the series' result policy like any match; the series only counts decided games.
- The server creates the next game when the previous one is decided, until the format is settled; it SHOULD give
  players a short break (the next game's `deadlines.launch` is the pace).
- After **every** decided game the server sends `series.updated`, and `activity.updated` when the series belongs to
  an activity, so views show the running score (1.1.2).
- A series whose game is void MAY replay it, as a new game with the next index, up to a limit the server states in
  its rules, then void the series. Only decided games count towards the format.
- Rematches: a server MAY offer a rematch action (an activity action, or a room's start again) that creates a new
  series or a single match with the same participants.
