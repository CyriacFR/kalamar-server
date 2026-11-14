# Writing your own server (protocol 1.0 and 1.1)

You can run the reference server in this repository, or write your own in any language from these documents. This
page gives the shortest path to a server a launcher can use, two worked minimal servers at opposite ends of the
range, the conformance areas, and how to run the conformance suite against any implementation.

## 1. The minimum viable server

A useful server needs the **core** and one way to create matches (private rooms *or* queues). In order:

1. **Discovery and meta.** Serve `/.well-known/kalamar-server.json` ([server-card.md](server-card.md)) and `GET
   /v1/meta` ([api.md](api.md#meta)) with: `protocol.versions: ["1.0"]`, `features` (your areas), `game_contract:
   {"id": "lan-auto-match", "versions": [1]}`, one login provider, one result policy, `limits`, `rules`.
2. **Errors.** One JSON error shape ([overview.md](overview.md#6-error-model)); malformed input gives 4xx, never 5xx.
3. **Login.** The simplest provider is `direct` with one `display_name` field: create an account, return a session
   (access + refresh token; store only their hashes). Implement `POST /v1/auth/refresh` and `POST /v1/auth/logout`.
4. **Account.** `GET /v1/me`, `PATCH /v1/me` (display name), `PUT /v1/me/game` (you may ignore the body),
   `GET /v1/state`, `GET /v1/me/export`, `DELETE /v1/me?confirm=erase`, `GET /v1/users/{id}`.
5. **Events.** The WebSocket of [events.md](events.md): `hello`, heartbeat, numbered events, and resumption (or
   always answer `resumed: false`: that is conformant, launchers then refetch `GET /v1/state`).
6. **Matches.** Create a match object ([api.md](api.md#match-object)) with a `game` block built as
   [match-contract.md](match-contract.md) says (member names, rules string, an unguessable room code, your relay),
   accept `launch`, `status` and `result` uploads, and decide with your declared result policy
   ([api.md](api.md#result-decision)).
7. **Rooms or queues.** Rooms: declare the creation form, implement create / join by code / teams / ready / start.
   Queues: declare at least one queue, implement tickets, the accept phase and its deadlines.
8. **A relay.** Run [`kalamar-relay`](../../crates/kalamar-relay) (or any relay protocol v1 implementation) on an
   open UDP port and name it in your matches.

Everything else (friends, presence, parties, ratings, leaderboards, statistics, moderation) is optional: declare an
area only when you implement all of its endpoints.

## 2. Worked example A: private rooms only, display name only

A server for a group of friends: no third-party login, no rating, no history.

**Meta (abridged):**

```json
{
  "name": "Thursday rooms",
  "protocol": {"versions": ["1.0"]},
  "time": "2026-11-11T20:00:00Z",
  "features": ["rooms"],
  "relay": "rooms.example.net:47313",
  "operator": {"name": "Sam", "contact": "mailto:sam@example.net"},
  "privacy_url": "https://rooms.example.net/privacy",
  "game_contract": {"id": "lan-auto-match", "versions": [1]},
  "limits": {"body_bytes": 16384, "result_bytes": 65536, "status_bytes": 4096,
             "display_name": {"min_len": 1, "max_len": 10}},
  "auth": {"providers": [{"id": "guest", "kind": "direct", "label": {"en": "Choose a name"},
    "fields": [{"key": "display_name", "type": "string", "label": {"en": "Name"}, "required": true,
                "min_len": 1, "max_len": 10}]}]},
  "rules": {"url": null, "summary": {"en": "Play nice."}},
  "result_policies": [{"id": "casual", "name": {"en": "Casual"},
    "summary": {"en": "The first result received is kept unless another one contradicts it."},
    "quorum": "any", "silence": "ignore", "disagreement": "void", "rated_requires_both_teams": false,
    "abort": "void", "disconnect": "game_result", "no_show": "void",
    "deadlines": {"accept_s": 20, "launch_s": 120, "result_s": 2400, "grace_s": 60}}],
  "rooms": {"options": [
      {"key": "teams", "type": "teams", "label": {"en": "Format"}, "required": true, "default": [4, 4],
       "choices": [[1, 1], [2, 2], [4, 4]]},
      {"key": "password", "type": "string", "label": {"en": "Password"}, "required": false, "secret": true,
       "min_len": 1, "max_len": 32}],
    "min_players": 2, "result_policy": "casual", "ladder": null, "invites": false, "idle_close_s": 3600}
}
```

The rooms declare no `mode` field: every room match uses the operator's mode (the server puts its configured mode id
in `rules.mode`, for example the id the game uses for its standard mode). **Endpoints:** the core list of section 1
plus the twelve room endpoints of [api.md](api.md#private-rooms) (`invites` is `false`, so `POST
/v1/rooms/{id}/invites` answers `404 feature_disabled`). Storage can live in memory: after a restart, tokens are
unknown (`401 unauthorized`) and the launcher logs in again with a new guest account. Export returns the account
and its matches; erase deletes them. The conformance suite run with `--areas core,rooms` passes on such a server.

## 3. Worked example B: a 1v1 ranked ladder with its own rating

A competitive server with one queue and a points system of its own: +3 for a win, -1 for a loss (never below 0),
tiers every 30 points.

**Meta (the parts that differ):**

```json
{
  "features": ["queues", "ratings", "leaderboards"],
  "auth": {"providers": [{"id": "discord", "kind": "oauth_loopback", "label": {"en": "Log in with Discord"},
            "loopback_redirects": ["http://127.0.0.1:53682/callback"]}]},
  "result_policies": [{"id": "strict", "name": {"en": "Strict"},
    "summary": {"en": "Both players' results must agree; a silent player who played accepts the other's result."},
    "quorum": "all", "silence": "consent_after_play", "disagreement": "void", "rated_requires_both_teams": false,
    "abort": "void", "disconnect": "game_result", "no_show": "void",
    "deadlines": {"accept_s": 15, "launch_s": 90, "result_s": 1800, "grace_s": 90}}],
  "queues": {"count": 1, "penalties": {"summary": {"en": "Declining blocks the queue for 2 minutes."},
             "counted": ["decline", "accept_timeout"], "window_s": 3600, "cooldowns_s": [120]}}
}
```

**The queue** (`GET /v1/queues`):

```json
{"items": [{"id": "duel", "name": {"en": "Duel"}, "description": {"en": "First to climb wins."}, "order": 1,
  "teams": [1, 1], "min_teams": [1, 1], "rated": true, "ladder": "points", "party": {"min": 1, "max": 1},
  "selection": "game", "pool": [{"mode": "<the operator's mode id>", "stage": null}], "rotation": null,
  "result_policy": "strict", "availability": {"open": true, "windows": []}, "estimated_wait_s": null,
  "searching": null, "requirements": [], "eligible": true}]}
```

**Its rating as presentable data** (`GET /v1/users/{id}/ratings`):

```json
{"items": [{"ladder": "points", "season": null, "value": 42, "display": "42 pts",
  "tier": {"id": "t2", "name": {"en": "Tier II"}, "color": "#3A7BD5", "order": 2},
  "progress": {"value": 12, "max": 30, "label": {"en": "12 / 30 to Tier III"}},
  "provisional": null,
  "last_change": {"delta": 3, "match_id": "m_1", "at": "2026-11-11T21:00:00Z", "reason": {"en": "Win"}},
  "fields": [{"key": "wins", "label": {"en": "Wins"}, "value": 16}]}]}
```

and its leaderboard declares the columns `rank`, `player`, `tier`, `points` (type `number`), `wins`. The launcher
shows all of it without knowing anything about points or tiers. Ratings change when the decision is `completed` and
rated; the event `rating.changed` carries the views. The conformance suite run with `--areas
core,queues,ratings,leaderboards` passes on such a server (it needs `--tokens` since there is no `direct` provider:
see below).

## 4. Conformance areas

Conformance is per area. A server is **conformant** when the core and every area it declares pass; areas it does not
declare are reported as "not declared" and do not count. The suite derives its expectations from what the server
declares: it uses the server's queues as listed, fills room forms from the declared fields, and computes the
expected decision of every scenario from the declared result policy with the normative procedure of
[api.md](api.md#result-decision).

| Area | Scenarios (ids as printed by the suite) |
|---|---|
| core | `core.discovery` (well-known document, consistency with meta), `core.meta` (required members, versions, contract), `core.errors` (401 shape, 400 `invalid_json`, 404, 413), `core.login` (provider, `/v1/me`, refresh rotation and reuse window, logout), `core.account` (rename, invalid name 422, settings, `PUT /v1/me/game`), `core.events` (hello, ping/pong, ticket, resume with replay), `core.match.agreed`, `core.match.disagreement`, `core.match.silent`, `core.match.error`, `core.match.invalid`, `core.match.crash`, 1.1.8 `core.match.counters` and `core.match.counters-gap` (one game's counters differ: the declared `stats` decides; `outcome.stats_check` when declared), 1.1.12 `core.match.forged` and `core.match.forged-crash` (one player forges a record that shows the other team gone while the others' games fail before the match, or crash in it: the forgery is never rated and never wins; the decision follows step 5), `core.match.contract` (game block: member names, rules string, room code entropy and format, players), 1.1.10 `core.match.free` (a participant whose report is in is free of a match waiting for the others; skipped without `matches.free_after_report`), 1.1.11 `core.match.leave` (a departure in play, `left-by-player` with `in_play: true`, against `connection-lost` from the others: decision `forfeit` for the other team, `outcome.forfeit`; skipped when the policy of the matches the suite creates does not declare `leave` `forfeit` or `forfeit_unrated`), `core.erase` (export, erase, token revoked) — match scenarios run through rooms if declared, else through the first eligible queue |
| rooms | `rooms.declaration`, `rooms.lifecycle` (create, join by code in lower case, wrong password, team change, ready, start refused until ready, match ready for all, room back to open), `rooms.full`, `rooms.kick-leave-owner` |
| queues | `queues.list` (shape, team sizes within the contract), `queues.match` (every eligible solo queue: fill it, accept, play, completed), `queues.decline` (canceled, accepters requeued, offense), `queues.timeout` (accept timeout; needs `test-clock` or a short deadline), `queues.cancel` |
| parties | `parties.lifecycle` (create, invite, join, leader, kick, leave), `parties.queue` (when a queue accepts parties: party kept on one team) |
| friends | `friends.requests` (by code, accept, decline, cancel, remove, code rotation), `friends.presence` (with `presence`) |
| ratings | `ratings.ladders`, `ratings.change` (a rated match produces rating changes and views for every participant) |
| leaderboards | `leaderboards.shape` (columns, rows, players) |
| stats | `stats.history` (the match appears with the right result), `stats.groups` |
| moderation | `moderation.report`, `moderation.block` |
| activities (1.1) | `activities.views` (every activity's view: component schema, limits, actions), `activities.play` (players join through each activity's `join` action; every match it creates is played until the activity finishes or stops creating matches; series invariants checked) |
| series (1.1) | `series.invariants` (every series met: wins counted from decided games, the format settled exactly when a side reached it) |
| core (1.1) | `core.capabilities` (a 1.1 body of `PUT /v1/me/game` is accepted) |
| core (1.1.9) | `core.relays` (`meta.relays`: at most 16, unique ids, host and port, `region` and `label` shapes; `meta.revision` an integer that does not go down; a `launcher.relay_rtt_ms` naming every relay plus an unknown id answered `204`; skipped without `meta.relays`), `core.relay.join` (a ready match's relay, reached with the relay protocol: `HELLO`, `CHALLENGE`, `JOIN` of the room the service gave under the participant's member name, then `LEAVE`: `JOIN_ACK` passes, `JOIN_NAK` fails — reason 3 is a relay bound by a secret the service does not seal its codes with; the match must be in `meta.relays` when declared; skipped when the relay does not answer from where the suite runs), `core.reload` (with `--reload-command`: the command makes the server read its configuration again, a connected launcher gets `meta.changed` with a larger `revision`, its session goes on; skipped without the option); `core.events` accepts another event between the one sent while disconnected and the resume |
| core (1.1.13) | `core.relay.status` (each relay of `meta.relays` that answers a `HELLO` from where the suite runs gets a status query sealed with a key it does not hold: no answer, and it still answers a `HELLO` afterwards; neither `meta.relays` nor `GET /v1/population` carries the relays' load; skipped when no relay answers) |
| population (1.1.4) | `population.counts` (public, shape, `Cache-Control` with `max-age`, two connected players counted once the cached answer is renewed); `core.errors` checks the 404 of an undeclared population |
| core (1.1.5) | `core.oauth` (every `oauth_loopback` provider: an unlisted redirect refused, the state in the authorization URL, `pkce_mismatch`, single-use and unknown states — all before any provider call), `core.password` (password forms: sign-up, login with the address in capitals, a wrong password and an unknown address answering alike, no address shown to other players, `POST /v1/me/password` ending the other sessions; skipped when sign-up needs an e-mailed code), `core.password.lockout` (repeated failures end in `429` with `Retry-After`; only on servers declaring `test-clock`, since it locks the suite's address for password checks a while) |
| draft (1.1.6) | `draft.declaration` (a queue `draft` or a room form option `draft`), `draft.play` (two launchers declaring `launcher.draft` play a whole draft: shape and invariants at every step, `403 not_your_turn` for the other team, `409 draft_turn` for another turn, the result in `rules` and in the game's rules string, the match played out), `draft.timeout` (nobody acts: the draft ends by itself; test clock or `--max-wait`), `draft.no-capability` (launchers without the capability: their turns are played, or the entry is refused when the draft is `required`), `draft.leave` (a decline during the draft cancels the match, `cancel_reason: left`) |
| spectators (1.1.7) | `spectators.declaration` (the meta object, room fields of known types, queues' `spectators`, `GET /v1/spectate`), `spectators.no-capability` (a launcher without `launcher.spectate` never joins the stands, and the refusal names what is missing), `spectators.room` (a member in the stands is seated with `S`, shown to the players, its witness report never counts as a player's, the decision follows the policy's `witness`), `spectators.late` (a spectator's launch after a player's stage loads is `409 match_started`, its error voids nothing) ([spectators.md](spectators.md#10-conformance)) |

Scenarios that need a deadline to pass use the [test clock](api.md#test-clock) when the server declares
`test-clock`, else wait in real time up to `--max-wait` seconds and are reported `skip` beyond it.

## 5. Running the conformance suite

The suite is the `kalamar-testkit` program of this repository. It plays several launchers at once (it never starts
the game: it simulates the game's status and result files exactly as the supported game build writes them) and needs
accounts:

- with a `direct` provider whose fields are text (for example `dev` or `guest`), it creates its own accounts;
- otherwise give it access tokens of test accounts, one per line, with `--tokens <file>` (it needs as many accounts
  as the largest declared queue or room plus one; it renames them `kt-<n>`).

```sh
cargo run -p kalamar-testkit --release -- conformance --api https://test.example.org/api
cargo run -p kalamar-testkit --release -- conformance --api http://127.0.0.1:8080 --areas core,rooms --max-wait 30
cargo run -p kalamar-testkit --release -- conformance --api https://test.example.org/api --tokens tokens.txt --json report.json
```

Options: `--api <url>` (required), `--discovery <host[:port]>` (where the well-known document is, default the API's
host), `--areas <list>` (default: every declared area), `--provider <id>`, `--tokens <file>`, `--max-wait <s>`
(default 30), `--json <file>` (machine-readable report), `--verbose`, `--reload-command "<shell command>"` (1.1.9 `core.reload`:
how to make the server read its configuration again; for the reference server `kalamar-service -c <file> admin
reload`), plain `http://` is accepted for loopback addresses only. The suite needs outbound UDP to the relays for
`core.relay.join`.

Output: one line per scenario (`pass`, `fail` with the reason, `skip` with the reason), then a table per area. The
exit code is 0 when every declared area passes.

**Run it only against a test instance**: it creates accounts, rooms and matches, files a moderation report and erases
accounts. The reference server's integration tests run it in-process against three configurations (rooms only, 1v1
queues only, the full reference configuration).

## 6. Four example servers (1.1)

Each of these needs **no launcher change**: only data the launcher already renders.

**A. An elimination tournament.** Area `activities` + `series`. The activity's view has a `countdown` to the start,
a `list` of registered players, a `register` action (the activity's `join`), then a `bracket` whose matches carry
`series_id`. Each bracket match is a best-of-3 [series](series.md); when a series ends the server fills the next round
and sends `activity.updated`. The reference server ships this one (`kind = "tournament"` in its configuration).

```json
{"type": "bracket", "rounds": [
  {"title": {"en": "Semi-finals"}, "matches": [
    {"id": "sf1", "state": "completed", "series_id": "sr_1",
     "slots": [{"label": {"en": "Camille"}, "score": 2, "winner": true}, {"label": {"en": "Lou"}, "score": 1, "winner": false}]},
    {"id": "sf2", "state": "running", "series_id": "sr_2", "slots": [ ... ]}]},
  {"title": {"en": "Final"}, "matches": [{"id": "f", "state": "waiting", "slots": [{"label": {"en": "Camille"}}, {"label": {"en": "?"}}]}]}]}
```

**B. A 1v1 league with stage pick and ban.** (Since 1.1.6 the area `draft`, [draft.md](draft.md), does the picks and
bans inside each match with its own screen: declare a preset per series and let the loser of each game pick. The
activity-only way below still works.) Each week's pairings are an activity; a pairing is a best-of-3 series.
Before each game the view shows a `form` with one `stage` field whose `values` are the stages still in play (taken
from the host's declared `stages`) and an action `ban` (then `pick`) enabled only for the player whose turn it is.
When the picks are done the server creates the game with `rules.stage` set — possible only with contract 2 and a
host whose engine declared `rule_key:stage`. Standings are a `table` (wins, losses, games, a `delta` column).

**C. "Imposed weapons" evening.** A scheduled activity (`starts_at`, `countdown`) that runs a queue-like rotation:
every match carries `rules.loadout` with one fixed weapon per team drawn by the server among ids it read from the
players' `options`, and a `summary` telling everyone it is honor-based. Players whose launcher did not declare
`engine.loadout: ["weapon"]` see the requirement unmet. Scores are a `table` of points per player.

**D. A clan server.** Clans live in the server only: an activity "Clans" whose view lists clans (`list` with
`trailing` member counts), with `form`s to create one and actions to ask to join; clan leaders see `accept` actions.
A "scrim" action challenges another clan: both leaders confirm, the server creates a 4v4 best-of-5 series with each
clan's chosen players. A ladder per clan season ranks clans (`GET /v1/ladders/{id}/leaderboard`, a `player` column
replaced by a `text` column with the clan's name), and clan badges ([presentation.md](presentation.md)) reward
seasons.

## 7. Interoperability notes

- Launchers ignore unknown members and handle unknown enum values: add `x-` members freely.
- Keep ids opaque and stable; never put personal data in ids or URLs.
- Make room codes and relay rooms unguessable and rate-limit joins by code.
- Decide results only as your declared policy says; a launcher shows the policy to the player.
- Test against the reference launcher's expectations by running this suite; report specification ambiguities as
  issues so the documents improve.
