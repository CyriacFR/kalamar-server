# Kalamar community protocol 1.1 — spectators ("the stands")

A match may have **spectators**: members of the game session seated in the stands instead of a team. They watch the
match from inside the game (its own spectator view), are shown to every player, and their game writes a result file
too, which the server reads as a **witness report**, never as a player's. Area id: `spectators` (in `features`);
launcher capability: `launcher.spectate` and the engine's `engine.spectator_seats` in `PUT /v1/me/game`. Everything here
is additive: a 1.1.x launcher that knows none of it still plays every match it is given, and is never seated in the
stands ([section 9](#9-launchers-and-games-without-the-capability)).

The server decides who may watch which match ([section 3](#3-where-places-come-from)); the launcher renders the places
and the spectator's screens from data. What the game can do is fixed below.

## 1. What the game can do (and cannot)

From the engine's own documentation and its capability list (`--list-match-options`, member `spectators`):

| Fact | Consequence for the protocol |
|---|---|
| A room has **2 places** for spectators, in every mode a private room offers; the team letter is `S` | at most 2 spectators per match (`MAX_SPECTATORS`) |
| `players=` counts **everyone** in the session: at most 8 players and 2 spectators, 10 in all | `game.players` = players + spectators |
| The game confirms the teams only with **two players at least and one in each team**; spectators do not count | a team's minimum and `min_players` count players only |
| Teams by name (relay mode): the host finds a spectator by its member name; once every player is in it waits `spectators_wait` seconds (0 to 600, default 60) for missing spectators, then starts without them | the rules string carries `spectators_wait`; a spectator who does not come never blocks a match |
| A spectator who arrives **after the match started** is left in the game's lobby until the end, then fails with `host-left` | places close when the game block is final; a spectator never launches once a player's stage loads (`409 match_started`) |
| A spectator's result file says `"outcome": "spectated"` and `"seat": "spectator"`, has no player entry with `self: true` and no `loadout`; its record is the players' (the same shared fields) | it is a witness report ([section 6](#6-witness-reports-and-the-decision)) |
| A spectator's game crash or departure changes nothing in the players' record (no `disconnected` entry) | its errors never void a match |
| The host may sit in the stands (verified once) | a room may let its owner host from the stands (`host`) |
| Verified: 1 and 2 spectators with 2 players, rounds (`next`) with a spectator | the engine declares `verified_modes`; an operator may restrict places to them |
| An **older engine** refuses the letter `S` and the key `spectators_wait` (`bad-settings`) | every participant's game must declare the capability, players included ([section 2](#2-declaring-the-capability)) |

The game cannot: let a spectator in after the start, delay what a spectator sees, hide a spectator from the players
(the pre-match screen lists them), or seat more than two.

## 2. Declaring the capability

A launcher that runs the spectator seat declares it in `PUT /v1/me/game` (1.1.7 members):

```json
{
  "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
  "engine": {
    "spectators": true,
    "spectator_seats": {"max": 2, "modes": ["<mode id>", "<mode id>"], "verified_modes": ["<mode id>"], "host": true},
    "rule_keys": ["players", "teams", "mode", "stage", "results_hold", "spectators_wait", "..."]
  },
  "launcher": {"draft": [1], "spectate": [1]}
}
```

| Member | Meaning |
|---|---|
| `engine.spectator_seats` | from the engine's `spectators` member: `max` (places), `modes` (the modes a room can have spectators in; absent: every mode), `verified_modes` (run with spectators), `host` (a host in the stands was run) |
| `launcher.spectate` | the versions of the spectator seat the launcher runs: it starts the game in the stands, imposes no loadout, uploads the `spectated` file, shows the places and the spectators to its player |
| `engine.spectators` | the 1.1 boolean (the engine's `teams.spectators`): informative. **It never seats anyone**: a 1.1.6 launcher relays it from a recent engine without knowing what to do with a spectator seat |

A server gives a place only when **every** participant of the match — each player and each spectator — declares
`engine.spectator_seats` (with `max` at least the match's spectators, the match's mode in `modes`, and with
`verified_modes_only` in `verified_modes`), `launcher.spectate` containing 1, and contract 2 (the rules go to every
participant). Reason: an older engine as a *player* stops with `bad-settings` on the rules' letter `S`; an older
launcher as a player would show its player no spectators. Otherwise the server refuses with `409 incompatible_game`
and `details.players[].missing`, the 1.1 items plus:

| Item | Meaning |
|---|---|
| `launcher:spectate` | the launcher does not run the spectator seat |
| `spectators` | the engine declares no places (`engine.spectator_seats` missing, or `max` 0) |
| `spectators:<n>` | the engine has fewer than `n` places |
| `spectator_mode:<mode>` | the engine cannot have spectators in that mode (or did not verify it, when the operator asks for verified modes) |
| `spectator_host` | the engine was not run with a host in the stands |
| `contract:2` | as in 1.1 |

The refusal comes **before** a place is given (taking it, joining or moving into the stands) and again when a room
starts, never as a match bound to fail.

## 3. Where places come from

`GET /v1/meta` declares the area and what holds for every place:

```json
"spectators": {"version": 1, "summary": {"en": "..."}, "opt_out": true, "referees": true, "late": false,
               "verified_modes_only": false, "wait_s": 60}
```

`late` is always `false` (the game cannot seat a spectator after the start); `opt_out`: players may refuse to be
watched ([section 7](#7-privacy-and-abuse)); `referees`: the operator has referee accounts.

### 3.1 Private rooms

The room declaration offers places as **ordinary fields** (a launcher that does not know the stands still draws the
form): `spectators` (type `integer`, `min` 0, `max` 1 or 2: places in the stands; 0 closes them) and, when the operator
offers a choice, `spectators_who` (type `enum`): who may sit — `anyone` (any member), `invited` (members who came by an
invitation), `friends` (the owner's friends), `referees` (the operator's). The declaration also carries
`"spectators": {"max", "who", "host", "wait_s", "late", "opt_out", "summary"}` for the launcher's stands area.

- The room object: a member in the stands has `"team": "spectator"`; `options.spectators`, `options.spectators_who`;
  `"spectators": {"seats", "taken", "who", "host"}`.
- `POST /v1/rooms/join` takes `"seat": "spectator"` (1.1.7, optional) to join in the stands: only a launcher that asks
  ends up there. Without it the player joins a team, or gets `409 room_full`, even when places in the stands are free.
- `PUT /v1/rooms/{room_id}/members/{user_id}` with `{"team": "spectator"}` moves a member into the stands, and
  `{"team": "alpha"}` / `"bravo"` back to play, like a team change (the member's `ready` becomes `false`).
- Errors: `409 spectators_closed` (no places), `409 spectators_full`, `403 spectators_not_allowed` (`details.who`),
  `409 incompatible_game` (section 2), `409 team_full` (back to a full team).
- `POST /v1/rooms/{room_id}/start`: the minimum and "both teams non-empty" count players only; every member must be
  ready, spectators included; every member's game is checked (section 2); a player who refuses to be watched gives
  `409 spectators_refused` (`details.players`), unless every spectator is a referee. The host is the owner (or a random
  player); an owner in the stands hosts from there only when the declaration says `host: true`, else a player hosts.
- A room's places are taken in the room: its match is ready at once and nobody joins its stands through the match.

### 3.2 Queues, activities, series

A queue object, and an activity in `GET /v1/activities`, may carry `"spectators": {"seats", "who", "label", "wait_s"}`
(or `null`): the places of every match (`who` is `anyone`, `friends` — a friend of a player of the match — or
`referees`).

- A queue match's places can be taken **before its launch**: during the accept phase (`pending_accept`) and the draft
  (`drafting`). When the match becomes `ready` they close.
- A series' places (an activity's series, any origin's) are taken **for the series**: the place holds for every next
  game until it is left; the current game too while its places are open (its draft). A change in the stands makes
  the next game a new game session (no `continue_from`, a new relay room: the engine's `next` round needs the same
  members).

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `GET /v1/spectate` | — | `{"items": [Watchable]}`: what the caller may watch, oldest first, at most 100 | `404 feature_disabled`, `429 rate_limited` |
| `POST /v1/matches/{match_id}/spectate` | `{}` | `200` Match (the caller's spectator view); idempotent. A game of a series seats for the series | `404 not_found` (no match, no places, a room's match), `409 match_started`, `409 spectators_full`, `403 spectators_not_allowed`, `403 spectators_refused`, `403 blocked`, `409 incompatible_game`, `409 in_match`, `409 already_in_queue`, `429 rate_limited` |
| `DELETE /v1/matches/{match_id}/spectate` | — | `204`: before the launch the place is free again; after it the spectator is `left` (its launcher closes its game; the game may wait `spectators_wait` for it) | `404 not_found` |
| `POST /v1/series/{series_id}/spectate` | `{}` | `200` Series (with `spectators`) | as for a match, `409 in_match` for a player of the series |
| `DELETE /v1/series/{series_id}/spectate` | — | `204` | `404 not_found` |

**Watchable**:

```json
{"kind": "match", "match_id": "m_...", "series_id": null, "origin": {"kind": "queue", "queue_id": "duel"},
 "state": "drafting", "rules": {"mode": "<mode id>", "stage": null, "teams": [1, 1]},
 "teams": {"alpha": [User], "bravo": [User]},
 "places": {"seats": 2, "taken": 1, "who": "friends", "label": {"en": "Friends"}},
 "series": null, "draft": true, "can_take": true, "why": null, "missing": []}
```

`kind` `series` items carry `series_id`, `series` (`index` of the next game, `format`, `wins`, `labels`) and the
current game's `match_id`. `why`: `spectators_full`, `incompatible_game` (with the caller's own `missing`; the players'
games are checked when the place is taken), or `null`. A match is listed only to callers who may take its places
(`who`), never when a player refuses to be watched (unless the caller is a referee), never a room's match, and never once
it is launched: **a match already started cannot be joined** (a limit of the game).

### 3.3 The operator's referees

`[spectators] referees` (the reference server) lists the operator's referee accounts: any open place is theirs whoever
it is for, a player's refusal does not apply to them, and a policy may count only their reports. The operator API also
seats a referee in a given match or series (scope `results`, [operators.md](operators.md)):

| Endpoint | Body | Answer |
|---|---|---|
| `POST /v1/operator/matches/{match_id}/spectators` | `{"user_id": "u_..."}` | `204`: the account has a place (flagged `referee`) in a match not launched yet |
| `POST /v1/operator/series/{series_id}/spectators` | `{"user_id": "u_..."}` | `204`: for the series' next games |

The referee plays the seat with its own launcher like any spectator (a person watching, with a game that writes the
witness report); a bot token alone cannot watch.

## 4. The match object

Additive members (participant view; a spectator gets the participant view too):

```json
"spectators": [
  {"user": {"id": "u_c", "display_name": "Cleo"}, "member_name": "Cleo", "role": "join", "referee": false,
   "launched": true, "left": false, "status": {"state": "in-match", "at": "..."}, "reported": false}
],
"rules": {"...": "...", "spectators": {"seats": 2, "who": "anyone", "wait_s": 60, "open": false}},
"you": {"user_id": "u_c", "member_name": "Cleo", "team": "spectator", "seat": "spectator", "role": "join", "referee": false, "left": false},
"game": {"players": 3, "rules": "players=3,teams=Ana:A,Bob:B,Cleo:S,mode=<mode id>,spectators_wait=60", "...": "..."}
```

- `participants` stays **the players**: a spectator is never in it, never in a team's count, never in `outcome.players`.
- `spectators` lists every spectator to every player and spectator: nobody watches unseen. The public view of an ended
  match has no `spectators` and no `outcome.spectators`.
- `you.team` is `"spectator"` (the value 1.1 reserved) and `you.seat` `spectator` (`player` for a player).
- `rules.spectators`: the places (`open` while they can be taken); `game.players` counts every member; `game.rules`
  seats the spectators after the players with the letter `S` and carries `spectators_wait`.
- `game.host_name` may be a spectator's (a room whose operator allows it): its `you.role` is then `host`.
- The draft (1.1.6) is shown to spectators **read-only**: `draft.you` is `{"team": "spectator", "role": "none",
  "can_act": false, "choice": null}`, no team's `votes`, hidden choices stay hidden; `POST .../draft` answers
  `403 not_your_turn`.
- Events: every `match.*` event goes to the spectators too; `series.updated` to a series' spectators.

## 5. Playing the seat (launcher)

A spectator launches as any participant of contract 2 ([match-contract-2.md](match-contract-2.md)): the same variables
(`KLM_MATCH=join`, or `host` when `you.role` is `host`; `KLM_MATCH_RULES = game.rules`, which seats it by its
member name), the same status and result files. Differences:

1. **Launch first, then start the game**: `POST /v1/matches/{id}/launch` before the process starts. `409
   match_started` (a player's stage loads, or later): do not start the game (it would wait in the lobby and fail); upload
   `{"version": 2, "match_id": ..., "error": "launcher-spectator-late"}` and tell the player the match started without
   them. Also skip the launch when a player's mirrored status is already `loading-stage` or later.
2. **No loadout**: `rules.loadout` is never applied to a spectator (no `KLM_MATCH_LOADOUT`, no pick).
3. **Report**: upload the result file as written (`outcome: spectated`); an error as usual. Nothing a spectator reports
   is held against it or the match.
4. **Leave**: `DELETE .../spectate` (or `decline` before the launch), then close the hidden game if it runs.
5. **Rounds**: with `keep_open`, a spectator continues like a guest (`next id=<match id>`; a host in the stands sends
   the host's line). A series whose stands changed never continues a session.
6. Show the player that they watch (not "your team"), the game's spectator controls with the keys the player's own
   settings bind, and no "it is your turn" sound or draft action.

## 6. Witness reports and the decision

### 6.1 The checks, revised

A spectator's report goes through the **witness reading** of the checks of [api.md](api.md#result-decision); a
player's report gains one condition. Nothing else changes.

| # | Player's report (1.1.7) | Witness report |
|---|---|---|
| V1 | version 1, or version 2 with `outcome: completed`; **and `seat`, when present, `player`** (a game that seated the player in the stands did not play) | version 2 with `outcome: spectated`; `seat`, when present, `spectator` |
| V2 | `match_id` is `game.match_id` | same |
| V3 | `role` is the uploader's `role` | same, the spectator's own role (a spectator may host) |
| V4 | one entry per **player**, names exactly the players' member names | same (spectators are never in the record) |
| V5 | each player's team | same |
| V6 | exactly one entry with `self: true`, the uploader's | **no** entry with `self: true` |
| V7 | mode and stage | same |
| V8 | teams and winner | same |
| V9 | numbers in range | same |
| V10 | a host's `settings_ok` is not `false` | same for a host in the stands |
| V11 | 1.1.8: a record of `rule` 0 does not clearly contradict its winner ([api.md](api.md#result-decision)) | same |

A spectator's error report (any object with `error`, an `aborted` file) is recorded and **ignored**: it never
contradicts the record (step 5), never voids a match, never is an offense; its absence is not a no-show.

### 6.2 Policy members

| Member | Values | Meaning |
|---|---|---|
| `witness` | `ignore` (default), `tiebreak`, `required` | what counted witness reports are used for (below) |
| `witness_from` | `any` (default), `referees` | whose reports count: any spectator's, or only referees' (`spectators[].referee`) |

### 6.3 The procedure with witnesses

*Counted witnesses*: spectators whose report counts under `witness_from` (none with `ignore`); *their records*: their
valid witness reports.

- **When** (addition): with `witness` other than `ignore`, "every participant has a terminal report" also waits for the
  counted witnesses who launched and did not leave; `grace_s` and `deadlines.result` still bound it.
- **Equal** records: as two players' results are (api.md, [step 2](api.md#result-decision)); 1.1.8: with the policy's
  `stats` `flag` or `ignore`, a witness agrees on what decides the match (winner, stage, every player's team and
  disconnection ...): its game computes its own counters like the players' games.
- **Step 4b** (`tiebreak`, `required`): when the players' valid results differ, if the counted witnesses' records are
  all equal and equal to exactly one group, that group is the candidate, decision **`witnessed`** (the others
  `dissent`). Otherwise `disagreement` applies as before. Witnesses never overturn players who agree. 1.1.8: the
  counted witnesses' records that agree with the candidate also help pick the record kept (the agreeing report closest
  to them).
- **Step 6**: a `witnessed` candidate counts the witnesses as one more voice out of one more: the quorum is checked on
  support + 1 of *n* + 1 (a 1v1: 2 of 3).
- **Step 8** (1.1.12): a counted witness who is a **referee** and whose record equals the accepted one confirms it
  for every team: a `witnessed` result is then rated. A witness who is not a referee may break the tie (step 4b) but
  never confirms for a team, so such a result is not rated (`unrated_reason: one_sided`): any spectator may be a
  player's second account.
- **Step 7c** (`required`): a counted witness whose valid record differs from the accepted one voids the match
  (**`witness_disagreement`**: the players agree on what the witness did not see); without a counted witness whose
  record equals the accepted one, the result counts but is not rated (`unrated_reason: no_witness`).

The outcome adds (participants only):

```json
"spectators": [{"user_id": "u_c", "referee": false, "counted": true, "verdict": "agreed"}],
"reports": {"expected": 2, "...": "...", "witnesses": 1},
"witnessed": true
```

Spectator verdicts: `agreed` (valid, equal to the accepted record in the sense of step 2), `dissent` (valid, differs), `reported` (valid, the
match is void), `invalid` (with `text`), `error`, `silent` (launched, no report), `absent` (never launched).

What witnesses guarantee: with `tiebreak`, one player alone cannot void a lost 1v1 by reporting a false result when a
counted witness reports; with `required` and referees, two players who collude on a false record are caught when the
referee's game recorded the match. What they do not: a modified game lies to every member alike (the witness's record
included), and a witness who is a friend of one side, or one side's second account, may collude: with `witness_from:
any` and places open to anyone, a cheater's second account in the stands decides the tie for them (1.1.12: counted, never
rated; in a series or a tournament a counted game still counts). Use `witness_from: referees` for ranked play and
tournaments, as the reference presets do; the reference server's `check-config` warns about the other combination.

## 7. Privacy and abuse

- **No hidden spectator**: the players see every spectator (room members, the match's `spectators`, the game's own
  pre-match screen).
- **Refusing to be watched**: with `opt_out`, the account setting `allow_spectators` (Me `settings`; `PATCH /v1/me`)
  lets a player refuse: their queue and series matches are not listed, places are refused (`403 spectators_refused`),
  a room with spectators does not start (`409 spectators_refused`). Referees are not refused.
- **Blocks**: a player who blocked a spectator (or the reverse) keeps them out (`403 blocked`), and the match is not
  listed to them.
- **Anti-spying**: the game shows the match live; there is no delay. The reference example reserves ranked places for
  referees; an operator chooses `who` per queue and activity.
- **Rate limits**: places taken or left and lists of watchable matches are limited per player (reference:
  `rate_limits.spectate_per_min`, 20).
- **Data**: export (`GET /v1/me/export`) has `matches_watched`; erasure anonymizes the account's places like its
  matches; raw witness reports are purged with the players' after the retention period.
- The public view of a match never names its spectators.

## 8. Error codes (1.1.7)

| Code | Status | When |
|---|---|---|
| `match_started` | 409 | the match is launched (a place) or a player's stage loads (a spectator's launch) |
| `spectators_full` | 409 | every place is taken |
| `spectators_closed` | 409 | the room has no places |
| `spectators_not_allowed` | 403 | the places are reserved (`details.who`) |
| `spectators_refused` | 403 / 409 | a player refuses to be watched |
| `incompatible_game` | 409 | section 2 |

Launcher error code: `launcher-spectator-late` (a spectator's launch was refused or skipped: the match had started).

## 9. Launchers and games without the capability

- A launcher without `launcher.spectate` is **never seated**: it cannot take a place, join or be moved into the stands;
  a room join without `seat` never lands in the stands.
- It is never in a match with spectators either: a place is refused while it plays (the reason names it), and a room
  where spectators sit refuses to start while it plays. Without spectators everything is as before.
- It ignores the room members whose `team` is `spectator` (an unknown team: shown in no team) and the new members.

## 10. Conformance

Area `spectators` of the suite (`kalamar-testkit conformance --api <url> --areas spectators`):

| Scenario | Checks |
|---|---|
| `spectators.declaration` | the meta object (version 1, `late` false), the room fields of known types, queues' `spectators`, policies' `witness`, `GET /v1/spectate` |
| `spectators.no-capability` | a launcher without the seat cannot join the stands (`409 incompatible_game` naming `launcher:spectate`) and joins a team instead |
| `spectators.room` | a member joins the stands, the match seats it (rules with `S`, `players` counted, `you.team: spectator`, listed to the players), a witness report never counts as a player's, the decision follows the declared policy with witnesses (oracle `kalamar_protocol::decision`) |
| `spectators.late` | a spectator's launch after a player's stage loads is `409 match_started`; its error report voids nothing |

## 11. The reference server's configuration

```toml
[features]
spectators = true

[spectators]
referees = ["u_..."]          # the operator's referee accounts
opt_out = true                # players may refuse to be watched
verified_modes_only = false   # true: only the modes the engines verified with spectators
wait_s = 60                   # spectators_wait (0-600)
summary = { en = "Spectators watch from the stands: the players see who watches." }

[rooms.spectators]
max = 2                       # 0 hides the option
default = 0
who = ["anyone", "invited", "friends", "referees"]
host = false                  # an owner in the stands may host

[[queues]]                    # ranked: referees only
spectators = { seats = 1, who = "referees" }

[[activities]]                # a place holds for the series' next games
spectators = { seats = 2, who = "anyone" }

[[result_policies]]
witness = "tiebreak"          # or "required"
witness_from = "referees"     # or "any": any spectator, maybe a player's second account (never for ranked play)
```

## 12. What an operator can set, and what stays fixed

The operator chooses: whether matches are watchable at all, per room form (places 0 to 2, who), per queue and per
activity (places, who: anyone, friends of the players, referees), whether players may refuse, which accounts are
referees (and seats them per match through the API), how long the host waits for spectators, whether only verified modes
get places, whether an owner may host from the stands, and what witness reports weigh in each result policy (nothing, a
tie-break, a required referee for rated results).

Fixed by the game: two places, no entry after the start, every participant's game must know spectators, spectators are
shown to the players, a spectator sees the match live.
