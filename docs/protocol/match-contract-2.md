# Kalamar community protocol 1.1 — match contract `lan-auto-match`, version 2, and capability negotiation

The protocol fixes only two things: **what the game can really do** (this contract) and **transport and security**.
Everything else is the server's invention. This document extends [match-contract.md](match-contract.md) (version 1,
unchanged and still valid) with everything a private match of the supported game build can express, a negotiation so
a server only assigns matches the players' game engines can run, constraints the launcher applies itself, and rounds
in one game session.

## 1. What is possible, and who guarantees it

Aligned with the engine's own capability list (`--list-match-options`). "Measured" means run with the supported game
build; "declared" means the engine lists it and the server must check the player's declaration
([section 2](#2-declaring-capabilities-put-v1megame-extended)).

| Capability | Status |
|---|---|
| 2 players, teams by name, mode, relay mode, result file | **guaranteed**: contract 1, measured |
| 2 to 8 players, uneven teams (each 1 to 4 places), "any team" rows | **declared** (`max_players`, `max_per_team`, `uneven_teams`); the engine's `verified_players_max` says how many were actually run (2 today) |
| mode by key | **declared** (`modes`) |
| stage by scene string, or the game's draw | **declared** (`stage_list`): `all` — every stage of the game's lists, whatever the saves (the guest needs nothing); `host_save` (older engines) — only the host's save's stages, see [section 3](#3-assigning-only-feasible-contracts) |
| the room's other settings | **declared** (`rule_keys`: one key per option the engine offers, opaque to the service) |
| result screens' hold time (`results_hold`) | **declared**, per player |
| several rounds in one session | **declared** (`rounds`); whether a round reaches its intro sooner than a relaunch is **declared by the engine** too (`rounds_faster`, from its `rounds.faster_than_restart`) ([section 6](#6-rounds-in-one-session)) |
| result file version 2: `outcome` (`completed` / `aborted`), `round`, `requested`, `settings_ok`, `room`, and each player's `weapon` and `gear` | **declared** (contract 2) |
| weapon and gear per player | **declared** (`match_loadout`): the engine applies an imposed loadout (a save profile with `weapon` and `gear`, `KLM_MATCH_LOADOUT`) to the match's copy of the save, never to the player's; **verifiable after the match**: the player's own `loadout.ok` and every player's `weapon` / `gear` in every report ([section 5](#5-loadout-constraints)) |
| spectators | **declared** (1.1.7, [spectators.md](spectators.md)): up to 2 per match (team letter `S`, counted in `players=`, the host's `spectators_wait`), verified with 1 and 2 spectators besides 2 players, the host in the stands too, rounds; a spectator's result says `outcome: spectated`. Assigned (`you.team: "spectator"`) only when **every** participant declares `engine.spectator_seats` and `launcher.spectate` (an older engine refuses the letter `S`); never after the start (the engine cannot seat a late spectator) |
| match duration | **not feasible**: fixed per mode by the game (no setting in the room) |
| password, session name | **not feasible** (the relay room is the isolation) |
| a third team, more than 8 players or 4 per team, 1-player matches, game-enforced weapon rules | **not feasible** |

Mode, stage, weapon and gear ids are **game identifiers** from the player's own game data. **A stage id is the
engine's opaque scene string** (`scene` in its option list, `stage_scene` in results), never the stage's number; in
`PUT /v1/me/game`, `stages` lists those strings, `rules.stage` is one, and check V7 compares it with `stage_scene`.
Weapon and gear ids are the engine's (strings or numbers, compared as text). This specification and its examples
contain none.

## 2. Declaring capabilities (`PUT /v1/me/game`, extended)

1.1 adds members to the 1.0 body; a 1.0 body stays valid (it means "contract 1, nothing more").

```json
{
  "contract": {"id": "lan-auto-match", "version": 1},
  "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
  "build": "opaque build id",
  "engine": {
    "rule_keys": ["players", "teams", "mode", "stage", "results_hold", "<other engine key>"],
    "rounds": true,
    "max_players": 8,
    "max_per_team": 4,
    "uneven_teams": true,
    "spectators": false,
    "loadout": ["weapon", "gear"],
    "match_loadout": true,
    "rounds_faster": true,
    "stage_list": "all"
  },
  "modes": ["<mode id>", "<mode id>"],
  "stages": ["<stage id>", "<stage id>"],
  "can_select_stage": true,
  "max_players": 8,
  "options": { "...": "the engine's --list-match-options output, verbatim (at most 48 KiB)" }
}
```

| Member | Meaning |
|---|---|
| `contracts` | every contract id and version the launcher implements (1.1). Without it: `contract` alone (1.0) |
| `engine.rule_keys` | the rule keys of `KLM_MATCH_RULES` the engine accepts: `players`, `teams`, `mode`, `stage`, `results_hold` and the keys of its `room_options` |
| `engine.rounds` | the engine plays several rounds in one session (`KLM_MATCH_CMD`) |
| `engine.max_players`, `max_per_team`, `uneven_teams`, `spectators` | team limits |
| `engine.loadout` | which loadout slots the launcher can impose (`weapon`, `gear`) |
| `engine.match_loadout` | 1.1.3: the engine applies an imposed loadout to the match's copy of the save (its `match_loadout` capability) |
| `engine.rounds_faster` | 1.1.3: the engine's `rounds.faster_than_restart` |
| `engine.stage_list` | 1.1.3: `all` or `host_save` (the engine's `stage_list`) |
| `stages` | for contract 2: the stages of this player's **room list** (only they can be picked when this player hosts) |
| `options` | the engine's own list (a JSON object or array, at most 48 KiB), opaque to the protocol (its format is the engine's); a server MAY use it to name or check ids. Servers that declare `1.1` accept `PUT /v1/me/game` bodies up to 64 KiB |
| `launcher` | 1.1.6: what the launcher itself renders: `draft` (versions of the draft object it shows, [draft.md](draft.md#7-launchers-without-the-capability)), e.g. `{"draft": [1]}`; 1.1.7: `spectate` (versions of the spectator seat it runs, [spectators.md](spectators.md#2-declaring-the-capability)) |
| `engine.spectator_seats` | 1.1.7: the engine's `spectators` member: `{"max", "modes", "verified_modes", "host"}`. The 1.1 boolean `engine.spectators` alone never seats anyone |

A launcher sends it after each login and whenever the engine or game changes. Unknown capability names are ignored by
servers; a launcher declares only what it really supports.

## 3. Assigning only feasible contracts

A server chooses for every match: the contract version (the highest one **every** participant declared), the host,
and rules within the participants' capabilities:

- **host-side** keys (`players`, `teams`, `mode`, `stage`, room settings) need only the **host's** engine; the server
  may pick the host for that;
- with engines declaring `stage_list: "all"` any stage of the pool works; with `host_save` engines a **stage** is
  chosen in the intersection of the server's pool and the **designated host's** declared `stages`
  (the stages of that player's save). When the intersection is empty the server picks another host, or asks for
  the game's draw (`stage` omitted). If the host's engine still stops with `stage-unavailable` (its save changed
  since it declared), the server **draws again** — a new match with the same participants, another stage of the
  intersection or the game's draw — and never simply voids the players' evening: the void match's
  `outcome.rematch_id` names the replacement;
- **per-player** keys (`results_hold`) and loadout constraints need **every** participant's;
- rounds (`keep_open`) need every participant's `rounds`.

When no assignment is feasible (for example a queue whose pool needs a stage choice and a player whose engine cannot),
the server refuses: `409 incompatible_game` with `details`:

```json
{"error": {"code": "incompatible_game", "message": "...",
  "details": {"players": [{"user_id": "u_b", "missing": ["contract:2", "rule_key:stage", "rounds"]}]}}}
```

`missing` items are `contract:<version>`, `rule_key:<key>`, `rounds`, `players:<n>`, `per_team:<n>`, `uneven_teams`,
`spectators`, `loadout:<kind>`, `mode:<id>`, `stage:<id>`, 1.1.6 `launcher:<capability>` (`launcher:draft`), 1.1.7
`launcher:spectate`, `spectators:<n>`, `spectator_mode:<mode>`, `spectator_host`; unknown items are shown as text.
Queues show the same as an unmet requirement (`eligible: false`).

## 4. The `game` block of contract 2

```json
"game": {
  "contract": {"id": "lan-auto-match", "version": 2},
  "match_id": "S-k7q2mxa9-2",
  "relay": {"host": "relay.example.org", "port": 47313, "room": "K7Q2MXA9PL4DZB3W8R6TNY5HCF"},
  "players": 4,
  "rules": "players=4,teams=Ana:A,Bob:B,Cleo:A,Dee:B,mode=<mode id>,stage=<stage id>,results_hold=15",
  "host_name": "Ana",
  "timeouts": null,
  "keep_open": true,
  "continue_from": null,
  "continue_window_s": 90
}
```

Differences with version 1:

- `match_id` is 1 to 64 of `A-Z a-z 0-9 - _` and **need not equal** `relay.room` (the room stays the same across the
  rounds of one session; each round has its own match id);
- `rules` goes to **every** participant (`KLM_MATCH_RULES`); the engine applies room keys on the host only and
  per-player keys everywhere;
- `keep_open`: when `true`, the launcher sets `KLM_MATCH_CMD` to a command file of its own and does **not** set
  `KLM_MATCH_EXIT`; the game then waits after the result (status `waiting-command`);
- `continue_from`: the `id` of the previous match object of the same session (a [series](series.md)); in contract 2
  a match's `game.match_id` equals its `id`, so it is also the round the running game knows; see section 6.

Every other variable is set as in version 1 (`KLM_LAN`, `KLM_LAN_ROOM`, `KLM_LAN_NAME`,
`KLM_NICKNAME`, `KLM_MATCH`, `KLM_MATCH_ID`, `KLM_MATCH_HOST`, statuses and results). Status and result
paths SHOULD contain `{id}` (the engine substitutes the round's match id).

## 5. Loadout constraints

`rules.loadout` (additive member of the match rules):

```json
"loadout": {
  "all": {"weapon": {"allowed": ["<weapon id>", "<weapon id>"]}},
  "teams": {"bravo": {"weapon": {"fixed": "<weapon id>"}}},
  "players": {"Ana": {"gear": {"head": {"fixed": "<gear id>"}}}},
  "summary": {"en": "Bravo plays one fixed weapon; everyone else picks among two."}
}
```

- Constraint kinds: `fixed` (one id), `allowed` (a list), `denied` (a list), exactly one per slot; slots: `weapon`,
  `gear.head`, `gear.clothes`, `gear.shoes`. Ids are game identifiers (strings or numbers, compared as text).
  Precedence: `players` (by member name) over `teams` over `all`.
- A server assigns a loadout only to players whose engine declares `match_loadout` (1.1.3) or, for older engines,
  whose launcher declared the slot in `engine.loadout`. Rooms, queues, series and activities may all carry one.
- With `match_loadout` the launcher writes the `fixed` items (and, for `allowed` / `denied`, the player's compliant
  pick) into a save profile and passes it as `KLM_MATCH_LOADOUT`; the engine stops with `loadout-failed` when it
  refuses an id. Each result file then has the player's own `loadout` with `ok`.
- Before launching, the launcher applies them through the player's save profile: `fixed` is set; for `allowed` /
  `denied`, a current item that breaks the constraint is replaced by one the player picks among compliant items
  owned in their own game data. A launcher that cannot apply a constraint (today: gear) **refuses cleanly**: it does
  not start the game and uploads `{"version": 2, "match_id": ..., "error": "launcher-loadout-unavailable"}`. The
  match is then void (decision `aborted`) and the server MUST NOT count it as an offense of that player.
- The launcher reports what it applied: `POST /v1/matches/{id}/launch` with `{"loadout": {"weapon": "<id>", "gear":
  {"head": "<id>", "clothes": "<id>", "shoes": "<id>"}}}` (informational).
- **Checked after the match.** Version 2 result files carry each player's `weapon` and `gear`. `weapon` is a shared
  field (equal in every report; 1.1.8: it decides only when the policy checks loadouts, else it is a counter compared
  with the tolerance of [api.md](api.md#result-decision) step 2); `gear` is informative. A result policy MAY check
  the constraints against the accepted record with its `loadout_check` member ([api.md](api.md#result-policies)):
  `ignore` (default), `void` (the match is void, decision `loadout_violation`) or `forfeit` (a team with a violating
  player loses; violations in both teams void the match). This catches a launcher that did not apply the constraint,
  not a modified game.

## 6. Rounds in one session

Rounds in one game session are a **declared capability**, and whether they are faster than a relaunch is **declared by
the engine** (`rounds_faster`). A server keeps the session (`keep_open: true`) only when every
participant's engine declares both `rounds` and `rounds_faster`, else it relaunches; and only then:

- the continuing match has the **same participants, the same host and the same relay room** as the previous one
  (the engine's room belongs to its host); a server that changes any of them MUST NOT set `continue_from`;
- `game.continue_window_s` (1 to 300, default 90) bounds how long the launcher keeps the hidden game waiting.

The launcher, when a match has `keep_open: true`:

1. After the round's result is uploaded, it waits at most `continue_window_s` for a `match.ready` whose
   `game.continue_from` is that round; then it appends to its command file: host `next id=<game.match_id>
   <game.rules>`, guest `next id=<game.match_id>`, reports `POST .../launch`, and mirrors statuses and the result as
   usual.
2. It appends `quit` when the window passes without a continuing match, when the series ends (`series.updated`, state
   other than `running`), or when the player quits.
3. A continuing match that arrives after `quit` is launched as a new game: `continue_from` is a hint, never an
   obligation.

## 7. Reports of contract 2

- Status files may have the state `waiting-command` (between rounds): mirrored as any state.
- **No result file means no match was played to its end.** The engine writes one only when the game ended a match:
  `"version": 2`, `round` (integer, from 1), `outcome`: `completed` (a record) or `aborted` (no record; the launcher
  uploads it as is; the server treats it as the error `match-aborted`). Also `settings_ok` (boolean, host only),
  `rule_name` (string), `requested` / `room` (objects defined by the engine), and per player `weapon` and `gear`.
- When the engine stops before a result (status `error`), the launcher uploads `{"version": 2, "match_id":
  "<game.match_id>", "error": "<the status file's error code>", "reason": "..."}`. Engine error codes: before the
  match `bad-settings`, `stage-unavailable`, `rules-not-applied`, `teams-not-set`, `room-timeout`, `boot-timeout`,
  `network-unavailable`, `list-timeout`, `create-failed`, `players-timeout`, `room-not-found`, `join-failed`,
  `host-left`, `player-left`, `start-timeout`, `stage-timeout`; during the match `match-aborted`, `record-timeout`,
  `connection-lost` (the session went silent for 45 s; 1.1.11 engines: 15 s in a match of two players),
  `left-by-player`. 1.1.13 engines say why a relay refused the player, before the match, where they said
  `network-unavailable`: `relay-full` (the relay is full: `JOIN_NAK` 5), `relay-room-full` (the match's room is full:
  `JOIN_NAK` 4), `relay-source-limit` (too many members of the room from the player's Internet address: `JOIN_NAK` 4
  with detail 1, [relay.md](relay.md#6-running-a-relay-without-a-service)), `relay-source-full` (too many players on
  the relay from that address: `JOIN_NAK` 5 with detail 1). The launcher reports them as it reports any engine error;
  they are not `connection-lost`, and the decision treats them as any error before the match. 1.1.11: with
  `left-by-player` the status file has `in_play` (boolean): `true` when the player left the match while it was being
  played (the game heard every other player within the last 2 s), `false` before the match (the stage loading) or
  with another player already silent; a player who leaves after the session stopped (the opponent's game gone) gets
  `connection-lost` instead. The launcher copies `in_play` into its report: `{"version": 2, "match_id": ..., "error":
  "left-by-player", "reason": ..., "in_play": true}`. The game's window closed during the match is the same departure
  (the engine writes it before it exits). 1.1.3 engines add, before the lobby, `save-unavailable` (no save, or the
  match's copy of it cannot be made) and `loadout-failed` (the imposed loadout is refused); `bad-settings` also
  covers an unreadable `KLM_MATCH_LOADOUT`.
- The decision procedure ([api.md](api.md#result-decision)) applies with these changes: V1 accepts versions 1 and 2;
  a version 2 report needs `outcome: "completed"` to be a record; **V10**: a host report with `settings_ok: false` is
  invalid (the room did not take the rules); `weapon` joins the shared fields of a player; `requested`, `room`,
  `rule_name`, `round`, `gear` and the teams' `count` are not shared fields; `connection-lost` and
  `launcher-crashed` count as mid-match errors. 1.1.8: reports agree on what decides the match, the counters
  being each game's own (step 2, policy `stats`); check V11 refuses a record of `rule` 0 whose declared loser clearly
  leads both team scores.
- 1.1.7: result files have `seat` (`player`, `spectator`) and `seat_asked`; a player's file whose `seat` is not
  `player` fails V1; a spectator's file says `outcome: spectated` and is a **witness report**, checked by the witness
  reading of the checks and used only as the policy's `witness` says
  ([spectators.md](spectators.md#6-witness-reports-and-the-decision)).

New launcher error codes: `launcher-loadout-unavailable`, `launcher-incompatible` (the engine lacks a capability the
match needs; the server should not have assigned it). Under contract 2 every launcher error report carries
`"version": 2` and `match_id` = `game.match_id`.
