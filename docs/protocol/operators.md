# Kalamar community protocol 1.1 — operator extension points

Interfaces for the operator's own tools: a Discord bot, a public statistics site, an external referee, a tournament
organizer. They are server-to-tool interfaces, separate from the launcher's API: launchers never call them. A server
declares the ones it offers in `GET /v1/meta` under `operator_api` (1.1, optional):

```json
"operator_api": {"webhooks": true, "bot_api": true, "referee": true}
```

## 1. Outgoing webhooks

The operator configures endpoints (`https://` only, in the server's configuration, never through the public API).
For each subscribed event the server POSTs:

```json
{"id": "wh_9x2k...", "type": "match.completed", "at": "2026-11-11T20:15:03Z", "data": { ... }}
```

- Types: `match.completed`, `match.void`, `series.completed`, `activity.updated`, `report.created`,
  `user.banned`, 1.1.6 `match.drafted` (a match's draft is over: the match with its `draft` and the stage it chose).
  `data` is the public view of the object (never tokens, never the relay room, never private settings, never a
  team's votes; reports carry ids and category only).
- Signature: header `Kalamar-Signature: t=<unix seconds>,v1=<hex HMAC-SHA-256(secret, "<t>." + body)>`; receivers
  reject a timestamp more than 5 minutes off and compare in constant time.
- Delivery: at least once; a server SHOULD retry with backoff on non-2xx, for as long as it chooses (24 hours is a
  reasonable span); `id` lets receivers drop duplicates.

## 2. Bot and administration API

Bearer tokens created by the operator (never by players), each with scopes: `read` (matches, series, activities,
public profiles, leaderboards), `results` (referee decisions), `moderation` (reports, bans), `activities` (create,
open, close, run activities). Paths are under `/v1/operator/`:

| Endpoint | Scope | What |
|---|---|---|
| `GET /v1/operator/matches?state=<state>` *paginated* | read | matches, including `under_review` |
| `GET /v1/operator/matches/{id}` | read | a match with every report (the raw files) |
| `POST /v1/operator/matches/{id}/decision` | results | decide or override: `{"counted": true, "winner": "alpha" or "bravo" or null, "rated": bool, "reason": localized text}` or `{"counted": false, "reason": ...}`; the outcome's `decision` becomes `admin` |
| `GET /v1/operator/reports` *paginated*, `POST /v1/operator/reports/{id}/resolve` | moderation | reports |
| `POST /v1/operator/users/{id}/ban`, `.../unban` | moderation | `{"until": time or null, "reason": text}` |
| `POST /v1/operator/activities/{id}/actions/{action}` | activities | run an organizer action (start, seed, disqualify) |
| `POST /v1/operator/matches/{id}/spectators` | results | 1.1.7: seat a referee account in a match not launched yet: `{"user_id"}` (a place whoever it is reserved for; the referee watches with its own launcher) |
| `POST /v1/operator/series/{id}/spectators` | results | 1.1.7: the same for a series' next games |

A token without the needed scope gets `403 forbidden`; a server that does not offer the bot API answers `404
feature_disabled` on `/v1/operator/...`. A decision through this API applies like any decision: ratings, series, events,
webhooks. A server MAY let a decision override an already decided match; it then reverses that match's rating
changes first.

## 3. Pluggable result policies, including a human referee

1.1 adds an optional member to a [result policy](api.md#result-policies):

| Member | Values | Meaning |
|---|---|---|
| `decider` | `auto` (default), `referee`, `auto_then_referee` | `auto`: the procedure of api.md decides; `referee`: when reports are in (or the deadline passed) the match waits in state `under_review` for a decision through the bot API; `auto_then_referee`: automatic decisions stand, but a disputed or void match goes `under_review` instead |
| `review_deadline_s` | seconds | how long a match may stay `under_review`; afterwards the automatic decision applies |
| `witness` (1.1.7) | `ignore`, `tiebreak`, `required` | spectators' reports as witnesses: nothing, a tie-break between players, or also a confirmation required for a rated result ([spectators.md](spectators.md#6-witness-reports-and-the-decision)) |
| `witness_from` (1.1.7) | `any`, `referees` | whose reports count: a referee's report (the operator's referee accounts, or one seated through the API) is the operator's eye in a match |
| `stats` (1.1.8) | `flag`, `ignore`, `strict` | what the reports' counters weigh ([api.md](api.md#result-decision), step 2): with `flag` a gap beyond the tolerance stays a counted result, flagged in `outcome.stats_check` (the operator API's match view and `match.completed` webhooks carry it) and in the server's log |

`under_review` is a new match state (a launcher treats an unknown state as waiting: no launch, refetch on `match.updated`). Its
`outcome` stays `null`; launchers show "waiting for a referee". A server MAY implement any other decider as an
`x-` value, as long as it declares it with a `summary` players can read.

## 4. The reference server's implementation

What the reference server does, including where it does less than the sections above allow. Webhooks (retried 5
times over about 10 minutes, queue in memory), the bot API (tokens configured as SHA-256 hashes,
made with `admin operator-token`), `GET /v1/operator/ladders/{ladder_id}/leaderboard` for statistics sites, and the
referee deciders. It decides only matches not yet decided (`ready`, `running`, `under_review`): overriding a decided
match answers `409 match_state`.
