# Kalamar community protocol 1.1 — match contract `lan-auto-match`, version 1

How a launcher turns a [match object](api.md#match-object) into the start settings of the supported game build, and
what it reports back. Written so that any launcher and any server interoperate: a server builds `game` as described
here, a launcher applies it without interpretation.

The supported game build has an *automatic match* mode: started with the settings below, it enters a private
match on the local network by itself (as host or guest) over the relay, plays one match with the window hidden until the stage intro,
writes a status file at every change of state and a result file at the end, then closes. The game never talks to
the service; the launcher does.

`GET /v1/meta` announces the contracts a server produces in `game_contract` (`{"id": "lan-auto-match", "versions":
[1]}`); every match carries the one it uses in `game.contract`. A launcher that does not implement that id and
version MUST refuse the match ([Refusing a match](#refusing-a-match)).

## 1. Values in the match object

`game` (present to participants from state `ready` on):

| Member | Constraint | Meaning |
|---|---|---|
| `contract` | `{"id": "lan-auto-match", "version": 1}` | this document |
| `match_id` | 1 to 32 characters from `A-Z 0-9` (upper case) | the game's match id; equal to `relay.room` in version 1 |
| `relay.host` | DNS name or IPv4 address | the relay of this match |
| `relay.port` | 1 to 65535 | its UDP port |
| `relay.room` | 1 to 32 characters from `A-Z 0-9 - _` (upper case), unguessable | the relay room ([relay.md](relay.md#3-room-allocation)) |
| `players` | 2 to 8 | number of players the host waits for: the number of participants |
| `rules` | at most 1024 bytes | the value of `KLM_MATCH_RULES`, built by the server ([below](#the-rules-string)) |
| `host_name` | a member name | the host's `member_name` |
| `timeouts` | `null` or a string | the value of `KLM_MATCH_TIMEOUTS` (`<step>=<seconds>,...`), when the server wants the game's step limits aligned with its deadlines |

and from the rest of the match: `you.role` (`host` or `join`), `you.member_name`, `you.team`, `participants[*]`,
`rules` (the authoritative rules), `deadlines`.

### Member names

Every participant has a `member_name`: the nickname the game shows and the name the relay gives the other members.
The server derives it (from the display name, for example) and guarantees:

- 1 to 10 Unicode scalar values and at most 32 bytes in UTF-8;
- no control character (U+0000-U+001F, U+007F), none of `,` `:` `=` `"`, no leading or trailing space;
- unique within the match, compared case-insensitively (the reference server appends a digit on a collision:
  `Camille`, `Camille2`).

The result file names players by these names; they are how the server maps a record to accounts.

### The rules string

`game.rules` is built by the server, comma-separated, in this order:

```
players=<n>,teams=<name>:<A|B>,<name>:<A|B>,...,mode=<mode id>
```

- `players=` the number of participants;
- `teams=` every participant's `member_name` with `A` for team `alpha` and `B` for team `bravo`, in participant
  order (the first entry follows `teams=`, the next ones are separated by commas);
- `mode=` `rules.mode`.

Version 1 of the contract has **no stage setting**: the game picks the stage. A server MUST therefore send
`rules.stage: null` to launchers of contract version 1 unless every participant declared `can_select_stage: true`
(`PUT /v1/me/game`) and the server knows the key the game build uses for it, which it then appends to `game.rules`.
The launcher passes `game.rules` verbatim either way.

Example (two players, `Alpha` placed in team bravo and `Bravo` in team alpha):
`players=2,teams=Alpha:B,Bravo:A,mode=<mode id>`.

## 2. Mode and stage identifiers

Mode and stage ids are **game identifiers**: the strings the game runtime itself uses, in its `mode=` setting and in
its result file (`mode`, and `stage_scene` for the stage). **A stage id of this protocol is always the engine's opaque
scene string** (`stage_scene` of the result, `scene` of the engine's option list), never the stage's number: the
number (`stage_id` in results) is only informative. The game runtime lists the valid modes, stages and
(mode, stage) pairs from the player's own game data (its `--list-match-options` facility); the launcher reports them
with `PUT /v1/me/game`.

- This specification ships **no list** of modes or stages, and a server never needs game data: it uses ids as opaque
  strings, may restrict them to a pool, reorder them, and give them display names in `catalog` of `GET /v1/meta`.
- A launcher resolves names and icons **locally** from the player's game data; it uses the server's `catalog` label,
  then the raw id, only when it cannot.
- `rules.stage: null` means "the game's choice"; the stage actually played is in the result (`stage_scene`).

## 3. Launching

When a match reaches `ready` (`match.ready` event, or `GET /v1/matches/current`):

1. **Check.** `game.contract` is implemented; `rules.mode` is in the game's modes; `game.players` is at most the
   game's player limit; `rules.stage` is `null` or the game can select stages. Otherwise refuse
   ([below](#refusing-a-match)).
2. **Resolve the relay.** The game only takes an IPv4 address: resolve `game.relay.host` (A records; an IPv4 literal
   as is). Failure: report `launcher-relay-unresolved`.
3. **Choose two local paths** for the status and result files, in a folder of the launcher's own (one per match:
   for example `<data>/matches/<game.match_id>/status.json` and `result.json`). Delete stale files there first.
4. **Start the game** with exactly these environment variables, in addition to the player's usual ones (and with
   every other `KLM_MATCH*` and `KLM_LAN*` variable removed, under any of its names):

| Variable | Value | For |
|---|---|---|
| `KLM_START` | `lobby-lan` | everyone |
| `KLM_LAN` | `relay:<resolved IPv4>:<game.relay.port>` | everyone |
| `KLM_LAN_ROOM` | `game.relay.room` | everyone |
| `KLM_LAN_NAME` | `you.member_name` | everyone |
| `KLM_NICKNAME` | `you.member_name` | everyone |
| `KLM_MATCH` | `host` when `you.role` is `host`, else `join` | everyone |
| `KLM_MATCH_ID` | `game.match_id` | everyone |
| `KLM_MATCH_RULES` | `game.rules`, verbatim | host only |
| `KLM_MATCH_HOST` | `game.host_name` | guests only |
| `KLM_MATCH_STATUS` | the status path | everyone |
| `KLM_MATCH_RESULT` | the result path | everyone |
| `KLM_MATCH_EXIT` | `1` | everyone |
| `KLM_MATCH_ON_ERROR` | `exit` | everyone |
| `KLM_MATCH_REVEAL` | `intro` (the player MAY prefer `loading`) | everyone |
| `KLM_MATCH_TIMEOUTS` | `game.timeouts`, when not `null` | everyone |

5. **Report the launch**: `POST /v1/matches/{id}/launch` as soon as the process started, before
   `deadlines.launch`.

The window stays hidden until the stage intro; the game closes itself after the result screens (`KLM_MATCH_EXIT`).
The launcher SHOULD keep the player informed from the mirrored statuses of every participant (`match.updated`).

### Refusing a match

A launcher that cannot apply the rules exactly MUST NOT start the game with other rules. It uploads an error report
(`POST /v1/matches/{id}/result`, below) with `launcher-unsupported-rules` and tells the player why. Servers that use
`PUT /v1/me/game` avoid this case.

## 4. What the launcher reports

### Status mirror

The game rewrites its status file whole at every change (written to a temporary file then renamed: a reader never
sees half a file). One JSON object, for example:

```json
{"version": 1, "match_id": "K7Q2MXA9PL4DZB3W8R6TNY5HCF", "role": "host", "state": "waiting-for-players",
 "since_unix": 1794346577.120, "detail": "waiting for the players", "present": 1, "expected": 2, "visible": false,
 "pid": 4242}
```

`state` is one of `booting`, `lobby`, `creating-room` (host), `waiting-for-players` (host; `present` of `expected`),
`joining` (guest), `in-room`, `loading-stage`, `in-match` (the intro started; the window is shown), `results` (the
result file is written), `finished` (the game closes), `error` (`error` and `reason` set). The launcher sends the
object verbatim to `POST /v1/matches/{id}/status` whenever it changes (at most 2 per second). The server uses
`in-match` as evidence that the participant played ([api.md](api.md#result-policies), `silence`).

### The result file

When the match's record appears, the game writes the result file (the game's `<path>.bin` next to it is not
uploaded). Example (host side, shortened):

```json
{
  "version": 1, "match_id": "R3", "role": "host",
  "rule": 0, "mode": "<mode id>", "stage_id": 3, "stage_scene": "<stage id>",
  "end_utc": 1794346807, "battle_frames": 10800, "duration_s": 180.00,
  "win_team": 1, "winner": "bravo", "deemed_result": -1,
  "teams": [{"team": "alpha", "paint_points": 28, "turf_permille": 11},
            {"team": "bravo", "paint_points": 57, "turf_permille": 22},
            {"team": "charlie", "paint_points": 0, "turf_permille": 0}],
  "players": [
    {"name": "Alpha", "self": true, "team": "bravo", "paint": 56, "kills": 0, "assists": 0, "deaths": 0, "specials": 0, "disconnected": false},
    {"name": "Bravo", "self": false, "team": "alpha", "paint": 27, "kills": 0, "assists": 0, "deaths": 0, "specials": 0, "disconnected": false}
  ],
  "t0_unix": 1794346556.640,
  "timeline_s": [["lobby loading", 7.3], ["match record", 251.2]]
}
```

| Fields | Between the participants' files |
|---|---|
| `match_id`, `rule`, `mode`, `stage_id`, `stage_scene`, `win_team`, `winner`, `deemed_result`, every player's `name`, `team`, `disconnected` | **equal** (the game's shared outcome: what decides the match) |
| every team's `paint_points` and `turf_permille`, every player's `paint`, `kills`, `assists`, `deaths`, `specials`, the order of players and teams | **each game's own counters** (1.1.8): they may differ slightly between machines, and are compared with a tolerance ([api.md](api.md#result-decision), step 2) |
| `end_utc` | equal only if the machines' clocks agree: informative |
| `battle_frames`, `duration_s` | 1.1.2: informative (the games of one match may count a few frames apart) |
| `role`, `self`, `t0_unix`, `timeline_s` | different by nature |

`win_team` is 0 for alpha, 1 for bravo; `winner` is the team's name. A team's `paint_points` is not the sum of its
players' `paint` (the game counts the two differently): servers compare values between reports, never recompute
them. The `charlie` team may appear with zeros and is ignored.

When the flow stops, the result file holds an error instead of a record:

```json
{"version": 1, "match_id": "E1", "role": "join", "error": "room-not-found",
 "reason": "no session answered within 20 s (step join)", "t0_unix": 1794346887.274, "timeline_s": [ ... ]}
```

Game error codes: `bad-settings`, `boot-timeout`, `network-unavailable`, `list-timeout`, `create-failed`,
`players-timeout`, `room-not-found`, `join-failed`, `host-left`, `player-left`, `start-timeout`, `stage-timeout`,
`left-by-player`, `match-aborted`, `record-timeout`. The ones that can happen after the match started are
`match-aborted`, `record-timeout` and `left-by-player`. 1.1.13 engines say why a relay refused the player instead of
`network-unavailable` ([match-contract-2.md](match-contract-2.md#7-reports-of-contract-2)): `relay-full`, `relay-room-full`,
`relay-source-limit`, `relay-source-full`, all before the match.

### Uploading

The launcher uploads the result file verbatim to `POST /v1/matches/{id}/result` as soon as it exists (status
`results` or `error`), once. When there is no file, it uploads a **launcher error report** instead:

```json
{"version": 1, "match_id": "<game.match_id>", "error": "launcher-crashed", "reason": "the game exited with code -1073741819 and no result"}
```

| Launcher code | When |
|---|---|
| `launcher-failed` | the game could not be started |
| `launcher-crashed` | the game process ended without a result file |
| `launcher-cancelled` | the player canceled before the game reached its lobby |
| `launcher-relay-unresolved` | the relay host could not be resolved |
| `launcher-unsupported-rules` | the launcher refused the rules |

`launcher-crashed` counts as a mid-match error (like `match-aborted`). On a network failure the launcher retries the
same body (the server treats an identical upload as a repeat) with backoff until it gets an answer or the match ends.
It keeps the files until the upload is answered.

### When the match ends without the player

On `match.void` or `match.cancelled` while the game is still before `in-match`, the launcher closes the game (it is
hidden) and uploads nothing more. When the game is already in the match, the launcher lets it finish; an upload after
the decision gets `409 match_state`, which the launcher ignores.

## 5. Guarantees and limits of this contract

- The relay room is the only isolation between matches; it is unguessable and shown only to participants.
- Team assignment by name works in relay mode (the relay tells each member the others' names).
- A result proves only what the game recorded on that machine ([overview.md](overview.md#32-what-the-service-can-know--and-what-it-cannot)).
- Verified with the supported game build: matches of two players through a relay, between two machines. Not
  verified: more than two players. Servers SHOULD keep launch and result deadlines generous (the game's own step
  limits add up to about 40 minutes in the worst case; a normal match takes about 5 minutes from launch).
