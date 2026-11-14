# Kalamar community protocol 1.1 — HTTPS API

Read [overview.md](overview.md) first (actors, areas, trust, versioning, error model). Version 1.1 adds the documents
listed in [section "1.1 additions"](#11-additions); everything below stays valid. The machine-readable form of
this document is [openapi.yaml](openapi.yaml); when the two differ, this document wins and the difference is a bug.

Contents: [Conventions](#conventions) · [Meta](#meta) · [Authentication](#authentication) · [Account](#account) ·
[Friends and presence](#friends-and-presence) · [Parties](#parties) · [Private rooms](#private-rooms) ·
[Queues](#queues) · [Matches](#matches) · [Result policies and the decision](#result-policies) ·
[Ratings, ladders, seasons, leaderboards](#ratings) · [Statistics and history](#statistics-and-history) ·
[Moderation](#moderation) · [Population](#population) · [Test clock](#test-clock) · [Error codes](#error-codes) ·
[Endpoint index](#endpoint-index)

---

## Conventions

### Base URL and paths

`<api>` is the `api` value of the server card or discovery document ([server-card.md](server-card.md)), for example
`https://play.example.org/api`. Every path below is relative to it: `GET /v1/meta` means `GET
https://play.example.org/api/v1/meta`. Paths are case-sensitive. A trailing slash is not part of any path.

### Requests and responses

- Bodies are JSON (`Content-Type: application/json`, UTF-8). A request without a body may omit the header; a
  request whose body is not a JSON object where one is expected gets `400 invalid_json`.
- Sizes: a server declares its limits in `limits` of `GET /v1/meta`. Every server MUST accept request bodies up to
  16 KiB, result uploads up to 64 KiB and status uploads up to 4 KiB, and answers `413 payload_too_large` above its
  own limits.
- Every endpoint needs `Authorization` except those marked *public* in the [endpoint index](#endpoint-index) (the
  WebSocket also accepts a ticket, [events.md](events.md)). Every endpoint with a path parameter MAY answer `404
  not_found`; every endpoint of an undeclared area answers `404 feature_disabled`.
- In responses and events, every member shown in an example is always present (possibly `null`) unless the text
  calls it optional. In request bodies, a member is optional unless the text or the form declaration requires it.
- Success is `200` with a JSON body, `201` when something was created (the body is the created object) or `204` with
  no body. Clients treat any 2xx as success.
- Headers sent by the launcher: `Authorization: Bearer <access token>` (every endpoint not marked *public*),
  `Kalamar-Protocol: 1.1`, `Accept-Language: <tags>` (optional: the server MAY reduce [localized text](#localized-text)
  to the requested languages), `User-Agent: <launcher name>/<version>` (no personal data).
- Headers sent by the server: `Kalamar-Protocol: 1.1`; `Retry-After` with `429`; `Cache-Control: no-store` on
  everything that carries tokens or private data.

### Time

Times are RFC 3339 strings in UTC with `Z` (`"2026-11-11T20:15:03Z"`; fractional seconds MAY appear and MUST be
accepted). Durations are JSON numbers of seconds in members whose names end in `_s`. A server's current time is
`time` in `GET /v1/meta`; a launcher computes countdowns (accept deadline, cooldowns) from the difference between a
deadline and that time, not from its own clock alone.

### Identifiers

Ids are opaque strings of 1 to 64 characters from `A-Z a-z 0-9 _ -`, never equal to a fixed path segment (`current`,
`join`, `requests`, `invites`, `tickets`, `me`). They are case-sensitive and never reused for
another object. A client MUST NOT parse them. Kinds of ids: user, party, room, queue, ticket, match, ladder, season,
report, request; a client never mixes them up because every endpoint says which it takes.

Codes meant to be typed by humans (room codes, friend codes) are different: they are compared case-insensitively,
and a server SHOULD use an alphabet without look-alike characters (the reference server uses Crockford base 32 and
shows friend codes as `XXXX-XXXX-XXXX`; dashes and spaces are ignored on input).

### Localized text

A member documented as *localized text* is an object mapping language tags (BCP 47, for example `en`, `fr`,
`pt-BR`) to strings:

```json
{"en": "Ranked duel", "fr": "Duel classé"}
```

A launcher shows the entry that best matches the player's language, else `en`, else any entry. A server SHOULD
include `en`. Strings are plain text (no markup); a launcher MUST NOT interpret them as HTML.

### Pagination

List endpoints marked *paginated* take `?limit=<1..100>` (default 20) and `?cursor=<opaque>` and answer:

```json
{"items": [ ... ], "next_cursor": "c2VlZD0xMjM"}
```

`next_cursor` is `null` on the last page. Cursors are opaque, at most 256 characters, and MAY expire (then `400
bad_cursor`: start again without a cursor).

### Extension members

Any object, in requests and responses, MAY carry members whose names start with `x-`; they are the operator's own
data. Receivers ignore every member they do not know ([overview.md](overview.md#51-extensibility-rules-both-sides)).

### Rate limits

Servers limit request rates and answer `429 rate_limited` with `Retry-After` (seconds) and `retry_after_s` in the
error. A launcher MUST wait at least that long before retrying the same call and SHOULD back off exponentially on
repeated 429 or 5xx (start at 1 s, double, cap at 60 s, add jitter). The reference server's limits are in
[its configuration](../../deploy/kalamar.example.toml). 1.1.12: a server SHOULD count a client by its network rather
than its single address where one client holds many (an IPv6 /64 by default in the reference server), limit the
accounts one client network creates (a guest login creates one), and limit the room matches a player starts.

### Common objects

**User** (public view of an account):

```json
{"id": "u_3k9w2m7x1q", "display_name": "Camille"}
```

**Field** — a server-declared form field, used for login forms and room options:

| Member | Type | Meaning |
|---|---|---|
| `key` | string | the member name in the request (`options.<key>`) |
| `type` | string | `string`, `integer`, `boolean`, `enum`, `teams`, `mode`, `stage`. Unknown type: the launcher hides the field if it is not `required`, else refuses the form with a "launcher too old" message |
| `label` | localized text | the field's label |
| `help` | localized text, optional | one sentence under the field |
| `required` | boolean | the request MUST carry the member |
| `default` | any, optional | the value the form starts with |
| `nullable` | boolean, optional | `null` is allowed; `null_label` (localized text) names it ("Game's choice") |
| `min`, `max` | integers, optional | `integer` range (inclusive) |
| `min_len`, `max_len` | integers, optional | `string` length in Unicode scalar values |
| `secret` | boolean, optional | `string` shown masked (passwords) |
| `values` | array, optional | `enum`, `mode`, `stage`: allowed values, each `{"id": "...", "label": <localized text, optional>}` in the order to show |
| `by_mode` | object, optional | `stage` only: mode id → array of allowed stage ids, instead of `values`, when stages depend on the mode |
| `choices` | array, optional | `teams` only: allowed team sizes as `[alpha, bravo]` pairs, for example `[[1,1],[2,2],[4,4]]` |
| `autocomplete` | string, optional | 1.1.5: an HTML autocomplete token (`email`, `username`, `nickname`, `current-password`, `new-password`, `one-time-code`) a launcher MAY pass to the system or a password manager; unknown values are ignored |

`mode` and `stage` values are [game identifiers](match-contract.md#2-mode-and-stage-identifiers): the launcher resolves
their names and icons from the player's own game data, and uses `label` (or the server's `catalog`) only when it
cannot.

---

## Meta

### `GET /v1/meta`

No authentication. Everything the service offers, in one document. A launcher fetches it when the server is added,
at each start, and when an event `meta.changed` arrives ([events.md](events.md)).

```json
{
  "name": "Thursday Night Club",
  "description": {"en": "A small community for Thursday nights."},
  "protocol": {"versions": ["1.0", "1.1"]},
  "software": {"name": "kalamar-service", "version": "0.1.0"},
  "time": "2026-11-11T20:15:03Z",
  "features": ["rooms", "queues", "parties", "friends", "presence", "ratings", "leaderboards", "stats", "moderation"],
  "relay": "relay.example.org:47313",
  "operator": {"name": "Camille", "contact": "mailto:admin@example.org"},
  "privacy_url": "https://play.example.org/privacy",
  "game_contract": {"id": "lan-auto-match", "versions": [1]},
  "limits": {"body_bytes": 16384, "result_bytes": 65536, "status_bytes": 4096,
             "display_name": {"min_len": 1, "max_len": 10}, "friends": 200, "party_size": 4},
  "auth": {"providers": [ ... ]},
  "rules": { ... },
  "result_policies": [ ... ],
  "catalog": {"modes": [{"id": "<mode id>", "label": {"en": "Mode one"}}], "stages": []},
  "rooms": { ... },
  "queues": {"count": 4, "penalties": { ... }},
  "moderation": {"report_categories": [{"id": "cheating", "label": {"en": "Cheating"}}]}
}
```

| Member | Required | Meaning |
|---|---|---|
| `name`, `description` | `name` | as in the discovery document |
| `protocol.versions` | yes | protocol versions served |
| `software` | no | `{"name", "version"}`: free text about the implementation |
| `time` | yes | the server's clock |
| `features` | yes | declared optional areas ([overview.md](overview.md#2-areas-and-the-minimal-core)); `test-clock` additionally marks a test server ([Test clock](#test-clock)) |
| `relay` | yes, or `null` | default relay `host[:port]` (informational) |
| `relays` (1.1.9) | no | every relay a match of this server may be given: an array (at most 16) of `{"id", "host", "port"}` with optional `region` (free text) and `label` (localized). A launcher MAY measure its round trip to each ([relay.md](relay.md#4-choosing-the-relay-of-a-match)) and declare it in [`PUT /v1/me/game`](#put-v1megame--auth); it never chooses the relay of a match |
| `revision` (1.1.9) | no | an opaque integer (at most 2^53) that grows each time the server's offer changes while it runs (announced by the event `meta.changed`), and also from one start of the server to the next as long as the server's clock moves forward; a launcher only compares it: one that kept a meta with another revision reads it again |
| `operator`, `privacy_url` | yes | as in the discovery document |
| `game_contract` | yes | the match contracts the server can produce ([match-contract.md](match-contract.md)): `id` and supported `versions`. A launcher that supports none of them shows the server as incompatible |
| `limits` | yes | `body_bytes`, `result_bytes`, `status_bytes`; `display_name.min_len`/`max_len` (Unicode scalar values); optional `friends`, `party_size` |
| `auth.providers` | yes, non-empty | [login providers](#authentication) |
| `rules` | yes | [rules block](#rules-block) |
| `result_policies` | yes, non-empty | [result policies](#result-policies) referenced by rooms and queues |
| `catalog` | no | operator-provided display names for mode and stage ids: `modes` and `stages`, arrays of `{"id", "label"}`. Never a list of what exists: only names, used when the launcher cannot resolve an id from the game data |
| `rooms` | with `rooms` | [room declaration](#room-declaration) |
| `queues` | with `queues` | `count` (number of queues; the list is `GET /v1/queues`) and `penalties` ([penalty declaration](#penalties)) |
| `moderation` | with `moderation` | `report_categories`: array of `{"id", "label"}` |
| `matches` (1.1.10) | no | `{"free_after_report": true}`: a participant whose terminal report is in is free of a match that waits for the others' reports ([Lifecycle](#lifecycle)). Absent: a participant is held until the decision |

### Rules block

```json
{
  "url": "https://play.example.org/rules",
  "summary": {"en": "Be kind. One account per person. Results are reported by your launcher."},
  "sections": [
    {"id": "fair-play", "title": {"en": "Fair play"},
     "body": {"en": "Results are uploaded by each player's launcher and compared ..."}}
  ]
}
```

`url` (or `null`) points to the human rules page; `summary` is one paragraph; `sections` (optional) are titled
paragraphs of plain text. The machine-readable part of the rules is the result policies, the penalties and the
queue and room declarations.

---

## Authentication

### Providers

`auth.providers` lists how a player can log in. Each provider is:

```json
{"id": "discord", "kind": "oauth_loopback", "label": {"en": "Log in with Discord"},
 "loopback_redirects": ["http://127.0.0.1:53682/callback", "http://127.0.0.1:53683/callback"]}
```
```json
{"id": "guest", "kind": "direct", "label": {"en": "Just a name"},
 "fields": [{"key": "display_name", "type": "string", "label": {"en": "Name"}, "required": true, "min_len": 1, "max_len": 10}],
 "warning": {"en": "Your account lives only in this launcher. If you lose its login, you lose the account."}}
```

| Member | Meaning |
|---|---|
| `id` | the provider's id in the paths below (`/v1/auth/<id>/...`) |
| `kind` | `direct` (a form of `fields`, answered at once) or `oauth_loopback` (browser authorization with a loopback redirect). Unknown kind: the launcher does not offer the provider |
| `label` | the button's text |
| `fields` | `direct` only: the login form ([Field](#common-objects)) |
| `warning` | optional text to show before using the provider |
| `loopback_redirects` | `oauth_loopback` only: the redirect URIs the server accepts, in order of preference. Each is `http://127.0.0.1:<port>/<path>` or `http://[::1]:<port>/<path>` |
| `purpose` | optional (1.1.5): what a `direct` form does: `login` (the default), `signup`, `reset`, `change` ([password accounts](#password-accounts)). Unknown value: treat as `login` |
| `group` | optional (1.1.5): providers with the same `group` are forms of one account system (a launcher MAY show them as tabs of one card) |

The ids `dev` (username-only test accounts, never in production) and `guest` (an account with only a display name)
are conventional; a server MAY use any id. A server MAY offer any number of `oauth_loopback` providers (the reference
server: Discord, Google, GitHub, Twitch or any OpenID Connect provider, by configuration).

### Tokens

A login returns a **session**:

```json
{
  "access_token": "kat1.Zb3...",
  "access_expires_at": "2026-11-11T21:15:03Z",
  "refresh_token": "krt1.Qx9...",
  "refresh_expires_at": "2026-12-11T20:15:03Z",
  "created": false,
  "me": { ... }
}
```

- Tokens are opaque strings of at most 512 characters. The access token is sent as `Authorization: Bearer <token>`.
- `created` is `true` when the login created the account; `me` is the [Me](#me-object) object.
- An expired access token gets `401 token_expired`; the launcher then calls `POST /v1/auth/refresh`. Any other
  `401` (`unauthorized`, `session_revoked`) means "log in again".
- **Refresh rotation.** Each refresh returns a new pair; the old access token stops working. The refresh token used
  is replaced; presenting it again within 60 s of its rotation returns another fresh pair (a lost response does not
  lock the player out), and presenting it later revokes the whole session (`401 session_revoked`) as a stolen-token
  defense.
- The launcher stores the tokens in the operating system's credential store when one exists and never logs them.

### Direct providers

#### `POST /v1/auth/{provider}/login`

Body: the provider's `fields` as an object, for example `{"display_name": "Camille"}` or `{"username": "alice"}`.
Answer: `200` session. Errors: `404 not_found` (unknown or non-direct provider), `422 validation_failed`, `429
rate_limited`. A login to a banned account succeeds ([Bans](#moderation)).

Semantics are the provider's: `dev` returns the same account for the same username (case-insensitively); `guest`
creates a new account at every call.

### OAuth loopback providers

The authorization-code flow with PKCE (RFC 7636) and a loopback redirect (RFC 8252), split so that the provider's
client secret stays on the server and the provider's tokens never reach the launcher:

1. The launcher creates a PKCE pair: `code_verifier` (43 to 128 characters from `A-Z a-z 0-9 - . _ ~`, from a
   cryptographic random source) and `code_challenge = BASE64URL-NOPAD(SHA-256(code_verifier))`.
2. It binds a loopback listener on the port of one of `loopback_redirects` (trying them in order), then calls
   `POST /v1/auth/{provider}/begin`:

   ```json
   {"redirect_uri": "http://127.0.0.1:53682/callback", "code_challenge": "E9Mel...", "code_challenge_method": "S256"}
   ```

   Answer `200`: `{"authorize_url": "https://provider.example/oauth2/authorize?...", "state": "st_...",
   "expires_at": "..."}`. `redirect_uri` MUST be one of `loopback_redirects` exactly (else `422
   validation_failed`). The server remembers `(state, code_challenge, redirect_uri)` for at most 10 minutes and puts
   `state` and the same `code_challenge` in `authorize_url`.
3. The launcher opens `authorize_url` in the system browser. The provider redirects the browser to `redirect_uri?code=
   ...&state=...` (or `?error=...`). The launcher's listener answers the browser with a plain page ("you can close this
   tab"), checks that `state` is the one it got, and stops listening.
4. The launcher calls `POST /v1/auth/{provider}/complete`:

   ```json
   {"state": "st_...", "code": "<code from the redirect>", "code_verifier": "<the verifier>"}
   ```

   The server checks that `state` exists, is unexpired and unused (it is single-use), that
   `BASE64URL-NOPAD(SHA-256(code_verifier))` equals the stored challenge, exchanges `code` with the provider (sending
   its client secret, the same `redirect_uri` and, when the provider supports PKCE, the `code_verifier`), reads the
   provider's user id, and answers `200` with a session. Errors: `400 invalid_state` (unknown, expired or used
   state), `400 pkce_mismatch`, `502 provider_error` (the provider refused or failed), `404 not_found` (unknown or
   non-OAuth provider; also for `begin`).

Even when the provider ignores PKCE, the server's own check of the verifier makes a code intercepted on the loopback
useless without the verifier.

A server calls the provider over HTTPS only, follows no redirect, bounds the time and the size of every answer, and
answers `502 provider_error` without repeating the provider's answer (at most its standard OAuth `error` code). It
never stores or returns the provider's tokens.

### Password accounts

1.1.5, optional. A server MAY offer accounts with an e-mail address and a password as ordinary `direct` providers:
every 1.x launcher draws their forms from `fields` and sends them to `POST /v1/auth/{provider}/login`. The forms of
one system share a `group` and differ by `purpose`:

| `purpose` | Fields (keys of the reference server) | Answers |
|---|---|---|
| `login` | `email`, `password` (`secret`) | `200` session; `403 wrong_password` |
| `signup` | `email`, `password` (`secret`), `display_name`; `code` (optional) when the server confirms addresses by e-mail | `200` session with `created: true`; `422 code_required`; `409 account_exists` (servers without confirmation only) |
| `reset` | `email`, `code`, `new_password` (`secret`); `code` and `new_password` optional when codes are e-mailed | `200` session; `422 code_required`; `422 validation_failed` with `field` `/code` (wrong, expired or used code) |
| `change` | `email`, `password` (current), `new_password` | `200` session |

**Codes.** A form whose `code` field is optional is sent first without it: the server sends a one-time code (by
e-mail) and answers `422 code_required` with `field` `/code`; the player types the code in the same form (a launcher
keeps the values) and sends it again. Without e-mail, `code` is required and comes from the server's operator (the
form's `help` and `warning` say how). Codes are random (at least 40 bits), single use, expiring, limited to a few
tries and stored hashed.

A server offering password accounts:

- MUST answer a wrong password and an address without an account **identically** (status, `code`, `message`,
  `field`, `text`) and at the same cost (it checks the password against a fixed dummy hash when there is no
  account); likewise a reset request answers `code_required` whether or not an account uses the address (the code
  goes only to an existing account). A sign-up confirmed by e-mail answers `code_required` even for an address
  already used (the owner gets a notice instead of a code); without confirmation a sign-up cannot hide it and
  answers `409 account_exists`;
- MUST store passwords only as a salted memory-hard hash (Argon2id with the current OWASP parameters is the
  reference choice), never log or return them, and SHOULD store the address only as a keyed hash (the reference
  server always does: every mail it sends answers a form where the player has just typed the address);
- MUST limit failed checks per account and per client address (`429 rate_limited` with `retry_after_s`), and SHOULD
  NOT let failures from elsewhere lock an account out of an address it recently logged in from;
- MUST end every other session of the account after a `reset` or a `change` (`session.revoked`, reason
  `password_changed`);
- MUST NOT make it the only login: the protocol forbids requiring an e-mail address to play
  ([overview.md](overview.md#4-privacy)); a server also offers `guest` or an OAuth provider;
- MUST never show the address to other players (no API answer carries it).

A password account is its own account: the reference server adds no password to a guest or OAuth account and no
other login to a password account (linking would need a confirmed address and a rule for conflicts).

### `POST /v1/auth/refresh`

Body `{"refresh_token": "krt1...."}`. Answer `200` session (with `me`). Errors: `401 unauthorized`, `401
session_revoked`. A banned account can refresh and log in ([Bans](#moderation)).

### `POST /v1/auth/logout` — *auth*

Body `{"all": false}` (optional; `true` revokes every session of the account). Answer `204`. Event connections of the
revoked sessions are closed ([events.md](events.md)).

---

## Account

### Me object

```json
{
  "id": "u_3k9w2m7x1q",
  "display_name": "Camille",
  "created_at": "2026-11-01T18:00:00Z",
  "logins": ["discord"],
  "friend_code": "7KQ2-MXA9-PL4D",
  "settings": {"share_presence": true, "public_profile": true, "show_on_leaderboards": true,
               "allow_friend_requests": true},
  "display_name_change": {"allowed_at": "2026-11-11T20:15:03Z"},
  "restrictions": [
    {"kind": "queue_cooldown", "until": "2026-11-11T20:20:03Z", "reason": {"en": "Declined 3 matches"}}
  ]
}
```

| Member | Meaning |
|---|---|
| `logins` | the provider ids linked to the account (never the provider's user id) |
| `friend_code` | with `friends` only |
| `settings` | booleans; a server includes those it supports: `share_presence` (`presence`), `allow_friend_requests` (`friends`), `public_profile` (`stats`: others see statistics and history), `show_on_leaderboards` (`leaderboards`), 1.1.7 `allow_spectators` (`spectators`, when the operator lets players refuse to be watched: [spectators.md](spectators.md#7-privacy-and-abuse)) |
| `display_name_change.allowed_at` | when the next rename is allowed (now or later) |
| `restrictions` | active restrictions: `kind` is `ban` (no play; `until` `null` = permanent), `queue_cooldown` (no queue until `until`), `chat`/other kinds MAY appear (unknown kind: show `reason`) |
| `password` | optional (1.1.5): `{"changed_at": "..."}` when the account has a password that [`POST /v1/me/password`](#post-v1mepassword--auth) changes; never the address |

### `GET /v1/me` — *auth*

Answer `200` Me. Works for banned accounts.

### `PATCH /v1/me` — *auth*

Body (every member optional): `{"display_name": "Cam", "settings": {"share_presence": false}}`. Answer `200` Me.
Errors: `422 validation_failed` (`field` `/display_name` for a name outside `limits.display_name` or with characters the
server refuses), `409 name_change_too_soon` (with `retry_after_s`), `409 display_name_taken` (if the server wants
unique names; the reference server does not).

### `POST /v1/me/password` — *auth*

1.1.5, optional ([password accounts](#password-accounts)). Body `{"password": "<current>", "new_password": "..."}`.
Answer `204`; every other session of the account ends, this one stays. Errors: `403 wrong_password` (the current
password), `422 validation_failed` (`field` `/new_password`), `429 rate_limited`, `404 not_found` (the account has
no password, or the server no password accounts). A launcher offers it when Me has `password`.

### `PUT /v1/me/game` — *auth*

What the player's game build supports, so the server only offers matches the launcher can run. The launcher sends it
after each login and whenever the game build changes. Body:

```json
{
  "contract": {"id": "lan-auto-match", "version": 1},
  "build": "opaque build id",
  "modes": ["<mode id>", "<mode id>"],
  "stages": ["<stage id>", "<stage id>"],
  "can_select_stage": false,
  "max_players": 8
}
```

`modes`/`stages` are the [game identifiers](match-contract.md#2-mode-and-stage-identifiers) the game lists; `build` is
an opaque string; `can_select_stage` says whether the game can be told a stage. Answer `204`. A server MAY ignore it.
1.1.12: `build` names a **build**, never an installation: a launcher sends a value that is the same for every copy
of the same game build (a version, a commit, a hash of the build's contents), or omits it; never a value made of
local file sizes, times or paths (such a value tells one installation from every other and links accounts across
servers). A server SHOULD
neither store nor log it beyond what it uses it for; the reference server ignores it.
1.1 adds members ([match-contract-2.md](match-contract-2.md#2-declaring-capabilities-put-v1megame-extended)); 1.1.7 adds
`engine.spectator_seats` and `launcher.spectate`, which every participant of a match with spectators must declare
([spectators.md](spectators.md#2-declaring-the-capability)); 1.1.9 adds the optional `launcher.relay_rtt_ms`: an object
from relay `id` (of `meta.relays`) to the round trip the launcher measured in milliseconds, or `null` for a relay
that did not answer (`{"eu": 24, "us": 131, "asia": null}`; at most 16 entries, values from 1 to 2000). A launcher
that measured sends every relay of the list; a server uses it only to choose the relay of the player's matches,
ignores ids it does not know, and MAY bound the values or ignore a declaration that leaves relays out
([relay.md](relay.md#41-several-relays-119)).
A server that uses it evaluates eligibility from it (a queue whose pool needs a stage choice is `eligible: false`
with a requirement explaining why).

### `GET /v1/state` — *auth*

A snapshot of everything live, used at start and after an event stream could not be resumed:

```json
{"me": { ... }, "party": null, "party_invites": [], "room": null, "room_invites": [], "ticket": null, "match": null}
```

Members exist only for declared areas; `match` is the player's current (non-terminal) match; `party_invites` items
are `{"party_id", "from": User, "expires_at"}` and `room_invites` items `{"room_id", "code", "from": User,
"expires_at"}` (pending invitations; they expire, ignoring one declines it).

### `GET /v1/me/export` — *auth*

Everything the server holds about the player, as one JSON document (`Content-Disposition: attachment`). The format
is the server's; it MUST include the account, settings, linked login kinds, sessions (creation and last-use times),
social data, matches with the player's own reports, ratings, penalties, restrictions and the reports the player filed.
Works for banned accounts.

### `DELETE /v1/me?confirm=erase` — *auth*

Erases the account: it can no longer log in, all its sessions end, and the data attached to it is deleted. Its
participation in other players' matches is anonymized (shown as a deleted player). The query parameter
`confirm=erase` is required (`422 validation_failed` without it). Answer `204`. A server MAY keep, for the duration
of an active ban only, a keyed hash of the login identity to stop ban evasion, and MUST say so in its privacy
notice. 1.1.12: likewise, a keyed hash of the login identity with what still runs at the erasure — a queue
cooldown and the penalties that make the next one longer, for as long as they run, and the open reports about the
account for a limited time (the reference server: 30 days) — which a new account with the same login takes over:
otherwise erasing the account and logging in again clears them. Reports about an erased account MAY be kept,
anonymized, until resolved. Works for banned accounts.

### `GET /v1/users/{user_id}` — *auth*

Answer `200` User (`id`, `display_name`), plus `friend` (boolean, with `friends`) and `blocked` (boolean, with
`moderation`). `404 not_found` for unknown or erased users.

---

## Friends and presence

Area `friends`; presence needs `presence` too.

### Friend object

```json
{"user": {"id": "u_...", "display_name": "Lou"}, "since": "2026-11-02T10:00:00Z", "presence": "in_match"}
```

`presence` (with `presence` only) is one of `online` (an event connection is open, or closed less than 60 s ago),
`in_queue`, `in_match` (a match the player takes part in is between `ready` and its end), `offline`. A player with
`share_presence: false` is always `offline` to others. Unknown value: show as `online`.

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `GET /v1/friends` | — | `{"items": [Friend]}` (all friends, not paginated; at most `limits.friends`) | |
| `DELETE /v1/friends/{user_id}` | — | `204` (both sides stop being friends) | `404 not_found` |
| `GET /v1/friends/requests` | — | `{"incoming": [Request], "outgoing": [Request]}` | |
| `POST /v1/friends/requests` | `{"friend_code": "7KQ2-MXA9-PL4D"}` | `201` Request, or `200` Friend when the other player had already asked (it becomes a friendship); 1.1.1: both carry `kind` (`request` or `friend`) | `404 not_found` (no such code), `409 already_friends`, `409 request_exists`, `403 blocked` (either side blocked the other, or the target refuses requests), `422 validation_failed` (own code), `409 friend_limit` |
| `POST /v1/friends/requests/{request_id}/accept` | — | `200` Friend | `404 not_found`, `409 friend_limit` |
| `POST /v1/friends/requests/{request_id}/decline` | — | `204` | `404 not_found` |
| `DELETE /v1/friends/requests/{request_id}` | — | `204` (cancel an outgoing request) | `404 not_found` |
| `POST /v1/me/friend-code` | — | `200` `{"friend_code": "..."}` (a new code; the old one stops working) | |

**Request**: `{"id": "fr_...", "user": User, "created_at": "..."}` (`user` is the other player).
Requests expire after a server-chosen time (the reference: 30 days). Events: `friend.request`, `friend.added`,
`friend.removed`, `friend.presence` ([events.md](events.md)).

---

## Parties

Area `parties`. A party is a group of players that joins queues together. A player is in at most one party. Every
member is shown to the others; the leader manages it.

### Party object

```json
{
  "id": "p_...",
  "leader": "u_a",
  "members": [{"user": {"id": "u_a", "display_name": "Camille"}, "joined_at": "..."}],
  "invites": [{"user": {"id": "u_b", "display_name": "Lou"}, "expires_at": "..."}],
  "max_size": 4,
  "ticket": null
}
```

`ticket` is the party's current queue ticket ([Queues](#queues)) or `null`. Every party endpoint that changes the
party MAY answer `409 party_busy` while the party is in a match (declining an invitation does not change the party).

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `GET /v1/party` | — | `{"party": Party or null}` | |
| `POST /v1/party` | — | `201` Party (the caller is leader) | `409 already_in_party` |
| `POST /v1/party/invites` | `{"user_id": "u_b"}` | `201` Party | `403 not_leader`, `409 party_full`, `403 blocked`, `403 not_friends` (a server MAY require friendship), `409 already_in_party` (target is in this party) |
| `DELETE /v1/party/invites/{user_id}` | — | `200` Party | `403 not_leader`, `404 not_found` |
| `GET /v1/party/invites` | — | `{"items": [{"party_id", "from": User, "expires_at"}]}` (incoming) | |
| `POST /v1/parties/{party_id}/join` | — | `200` Party | `404 not_found` (no invite / expired), `409 already_in_party`, `409 party_full`, `409 party_busy` (in a queue or a match) |
| `POST /v1/parties/{party_id}/decline` | — | `204` | `404 not_found` |
| `POST /v1/party/leave` | — | `204` | `404 not_found` |
| `POST /v1/party/kick` | `{"user_id": "u_b"}` | `200` Party | `403 not_leader`, `404 not_found` |
| `POST /v1/party/leader` | `{"user_id": "u_b"}` | `200` Party | `403 not_leader`, `404 not_found` |

Rules: invitations expire (reference: 5 minutes). When the leader leaves, the longest-present member becomes leader;
a party with no member left is gone. Joining, leaving or kicking cancels the party's queue ticket. A party cannot
change while its members are in a match (`409 party_busy`). Events: `party.updated`, `party.invite`, `party.closed`.

---

## Private rooms

Area `rooms`. A room gathers players who start a match together on their own terms. A player is in at most one room.

### Room declaration

In `rooms` of `GET /v1/meta`: how the creation form looks and what a room may be.

```json
{
  "options": [
    {"key": "teams", "type": "teams", "label": {"en": "Format"}, "required": true, "default": [4, 4],
     "choices": [[1, 1], [2, 2], [3, 3], [4, 4]]},
    {"key": "mode", "type": "mode", "label": {"en": "Mode"}, "required": true, "default": "<mode id>",
     "values": [{"id": "<mode id>"}]},
    {"key": "stage", "type": "stage", "label": {"en": "Stage"}, "required": false, "nullable": true,
     "null_label": {"en": "Game's choice"}, "default": null, "values": []},
    {"key": "password", "type": "string", "label": {"en": "Password"}, "required": false, "secret": true,
     "min_len": 1, "max_len": 32}
  ],
  "min_players": 2,
  "result_policy": "casual",
  "ladder": null,
  "invites": true,
  "idle_close_s": 7200
}
```

| Member | Meaning |
|---|---|
| `options` | [Fields](#common-objects) of the creation form, in display order. The keys `teams`, `mode`, `stage` and `password` have the meaning below; any other key is the server's (the launcher shows it as declared and sends the value back) |
| `min_players` | fewest players for a start (at least 2) |
| `result_policy` | id of the [result policy](#result-policies) of room matches |
| `ladder` | `null` (room matches are unrated) or the id of a ladder they count for |
| `invites` | rooms accept invitations (`POST /v1/rooms/{id}/invites`) |
| `idle_close_s` | a room without activity for that long is closed |

Reserved option keys:

- `teams` (type `teams`): `[alpha, bravo]`, the most players per team. Both at least 1; the sum at most the game
  contract's player limit ([match-contract.md](match-contract.md)).
- `mode` (type `mode`): a mode id.
- `stage` (type `stage`): a stage id, or `null` for the game's choice.
- `password` (type `string`, `secret`): never returned; the room shows `has_password`.
- `loadout` (1.1.3, type `enum`, nullable): one of the server's loadout presets, whose constraints become
  `rules.loadout` ([match-contract-2.md](match-contract-2.md#5-loadout-constraints)); `null` leaves loadouts free.
- `draft` (1.1.6, type `enum`, nullable): one of the server's drafts ([draft.md](draft.md#2-declarations)); its
  values carry the draft's declaration (`label`, `summary`, `required`, `turn_s`, `pool`); `null`: no draft. With a
  draft, the room's match starts in `drafting` and the draft decides the stage.
- `spectators` (1.1.7, type `integer`, `min` 0, `max` 1 or 2) and `spectators_who` (1.1.7, type `enum`): places in the
  room's stands and who may take them ([spectators.md](spectators.md#31-private-rooms)); the declaration then also has a
  `spectators` object (`max`, `who`, `host`, `wait_s`, `late`, `opt_out`, `summary`).

A server that offers no choice for a reserved key omits the field; the match rules then carry the server's value.

### Room object

```json
{
  "id": "r_...",
  "code": "7KQ2MX",
  "owner": "u_a",
  "options": {"teams": [4, 4], "mode": "<mode id>", "stage": null},
  "has_password": true,
  "state": "open",
  "members": [
    {"user": {"id": "u_a", "display_name": "Camille"}, "team": "alpha", "ready": true, "joined_at": "..."}
  ],
  "match_id": null,
  "created_at": "...",
  "closes_at": "..."
}
```

`state` is `open` (players may join, change teams, ready up), `in_match` (a match started from the room is running;
`match_id` names it) or `closed`. Unknown state: treat as `in_match`. `team` is `alpha` or `bravo` (1.1.7: or `spectator`,
a member in the stands; a launcher that does not know it shows the member in no team). `code` is what players type to join
(case-insensitive). 1.1.7: `POST /v1/rooms/join` takes `"seat": "spectator"` to join in the stands, `PUT .../members/{user_id}`
takes `{"team": "spectator"}`, and the start counts players only ([spectators.md](spectators.md#31-private-rooms)).

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `POST /v1/rooms` | `{"options": {...}}` | `201` Room (the caller is owner, in team alpha, not ready) | `409 already_in_room`, `409 in_match`, `422 validation_failed` |
| `GET /v1/rooms/current` | — | `{"room": Room or null}` | |
| `GET /v1/rooms/{room_id}` | — | `200` Room (members only) | `404 not_found` |
| `PATCH /v1/rooms/{room_id}` | `{"options": {...}}` (only the keys to change; `"password": null` removes it) | `200` Room; every member's `ready` becomes `false` | `403 not_owner`, `409 room_state`, `422 validation_failed` (also when the new `teams` cannot hold the current members) |
| `POST /v1/rooms/join` | `{"code": "7kq2mx", "password": "..."}` | `200` Room; the player is put in the team with fewer players (alpha on a tie) that has room | `404 not_found`, `403 wrong_password`, `409 room_full`, `409 room_state`, `409 already_in_room`, `403 kicked`, `429 rate_limited` |
| `POST /v1/rooms/{room_id}/leave` | — | `204` | `404 not_found` |
| `POST /v1/rooms/{room_id}/invites` | `{"user_id": "u_b"}` | `204`; the invitee gets `room.invite` and may join without the password for 10 minutes | `403 blocked`, `403 not_friends` (a server MAY require it), `404 not_found` |
| `POST /v1/rooms/{room_id}/kick` | `{"user_id": "u_b"}` | `200` Room | `403 not_owner`, `404 not_found` |
| `POST /v1/rooms/{room_id}/owner` | `{"user_id": "u_b"}` | `200` Room | `403 not_owner`, `404 not_found` |
| `PUT /v1/rooms/{room_id}/members/{user_id}` | `{"team": "bravo"}` | `200` Room; the moved player's `ready` becomes `false` | `403 not_owner` (others than oneself), `409 team_full`, `409 room_state` |
| `POST /v1/rooms/{room_id}/ready` | `{"ready": true}` | `200` Room | `409 room_state` |
| `POST /v1/rooms/{room_id}/start` | — | `201` [Match](#match-object) in state `ready` | `403 not_owner`, `409 not_ready` (someone is not ready; the owner counts as ready when starting), `409 not_enough_players` (fewer than `min_players`, or a team empty), `409 room_state`, `409 incompatible_game` (a member's game cannot run the rules; 1.1.7: or play with spectators), 1.1.7 `409 spectators_refused`, 1.1.13 `503 relay_capacity` (every relay is full: nothing changed, start again later) |

Rules: when the owner leaves, ownership passes to the longest-present member; an empty room closes. A kicked player
cannot rejoin the same room (`403 kicked`). While `in_match`, members may leave the room (it does not affect the
match) but nobody can join. When the match ends the room returns to `open` with every `ready` reset. Room matches
have no accept phase. Join attempts by code are rate-limited to stop code guessing. Events: `room.updated`,
`room.invite`, `room.closed`.

---

## Queues

Area `queues`. Queues are entirely the server's: it decides which exist, their formats, pools, rating, entry rules
and opening hours. A launcher lists them exactly as given, in `order`.

### Queue object

```json
{
  "id": "duel-ranked",
  "name": {"en": "Ranked duel", "fr": "Duel classé"},
  "description": {"en": "One against one. Counts for the Duel ladder."},
  "order": 10,
  "teams": [1, 1],
  "min_teams": [1, 1],
  "rated": true,
  "ladder": "duel",
  "party": {"min": 1, "max": 1},
  "selection": "random",
  "pool": [{"mode": "<mode id>", "stage": null}],
  "rotation": null,
  "result_policy": "standard",
  "availability": {"open": true, "windows": []},
  "estimated_wait_s": 40,
  "searching": 3,
  "requirements": [{"id": "game-contract", "label": {"en": "A launcher that can run this queue"}, "met": true}],
  "eligible": true
}
```

| Member | Meaning |
|---|---|
| `id`, `name`, `description`, `order` | identity and presentation; lower `order` first |
| `teams` | team sizes of a full match, `[alpha, bravo]` (equal or not); the sum is at most the game contract's limit |
| `min_teams` | the smallest team sizes the server may start with when few players are searching (equal to `teams` when it never shrinks) |
| `rated`, `ladder` | whether matches change ratings, and on which [ladder](#ratings) (`null` when unrated) |
| `party` | party sizes accepted (`min`, `max`); solo players are parties of 1 |
| `selection` | how the server picks a match's mode and stage: `random` (at random in `pool`), `rotation` (at random in the current `rotation` entry's pool), `fixed` (the first entry of `pool`), `vote` (participants vote during the accept phase; reserved, not specified in 1.0), `game` (the stage is left to the game: `stage: null`). Unknown value: show as "chosen by the server" |
| `pool` | the allowed `{"mode", "stage"}` pairs (`stage` `null` = the game's choice) |
| `rotation` | `null`, or the current and next entries of a schedule: `[{"starts_at", "ends_at", "pool": [...]}]` |
| `result_policy` | id of the [result policy](#result-policies) |
| `availability` | `open` now or not, and `windows`: upcoming opening windows `[{"starts_at", "ends_at"}]` (empty when always open); `season` (optional) the season id the queue belongs to |
| `estimated_wait_s` | the server's estimate, or `null` |
| `searching` | players currently searching, or `null` when the server does not tell |
| `requirements` | entry conditions evaluated by the server **for the caller**: `id`, `label` (shown as a checklist), `met` |
| `eligible` | the caller may join now (open, every requirement met, no cooldown) |
| `draft` | 1.1.6, optional: the draft of the queue's matches (`id`, `label`, `summary`, `required`, `turn_s`, `pool`), or `null` ([draft.md](draft.md#2-declarations)); a required draft adds the requirement `draft` |
| `spectators` | 1.1.7, optional: places in the stands of the queue's matches (`seats`, `who`, `label`, `wait_s`), or `null` ([spectators.md](spectators.md#32-queues-activities-series)) |

### Penalties

`queues.penalties` in `GET /v1/meta`:

```json
{"summary": {"en": "Declining or missing a match blocks queues: 1, 5, then 15 minutes ..."},
 "counted": ["decline", "accept_timeout", "no_show", "left"], "window_s": 86400,
 "cooldowns_s": [0, 60, 300, 900, 3600]}
```

`counted` lists the offenses (`decline`: declined a found match; `accept_timeout`: did not answer in time;
`no_show`: did not launch in time; `left`: left a queue match before its end, or abandoned its draft; 1.1.6
`draft_timeout`: let a turn of a match's draft run out), `cooldowns_s[k]` is the queue cooldown
after the (k+1)-th offense within `window_s` (the last entry repeats). A server MAY declare other offenses (unknown
ones: show `summary`). A player under a cooldown has a `queue_cooldown` restriction.

### Endpoints

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `GET /v1/queues` | — | `{"items": [Queue]}` (evaluated for the caller) | |
| `GET /v1/queues/{queue_id}` | — | `200` Queue | `404 not_found` |
| `POST /v1/queues/{queue_id}/tickets` | `{}` | `201` Ticket (solo, or for the whole party when the caller leads one) | `404 not_found`, `409 queue_closed`, `403 not_eligible` (with `details.requirements` unmet), `403 queue_cooldown` (`retry_after_s`), `409 already_in_queue`, `409 in_match`, `403 not_leader`, `409 party_size`, `409 in_room` (a server MAY refuse queueing from a room) |
| `GET /v1/tickets/current` | — | `{"ticket": Ticket or null}` | |
| `DELETE /v1/tickets/current` | — | `204` (any member of the ticket may cancel it, for everyone) | `404 not_found`, `409 match_found` (too late: answer the match) |

**Ticket**:

```json
{"id": "t_...", "queue_id": "duel-ranked", "party_id": null,
 "members": [{"id": "u_a", "display_name": "Camille"}], "created_at": "...",
 "state": "searching", "match_id": null, "estimated_wait_s": 40}
```

`state` is `searching` or `match_found` (then `match_id` is set). A ticket ends when its match is created and
accepted, when it is canceled, or when the queue closes. Events: `queue.ticket` (ticket changed or ended),
`match.found`.

### Matchmaking and the accept phase

When the server forms a match from tickets, it creates the [match](#match-object) in state `pending_accept`, sets
every ticket to `match_found`, and sends `match.found` to each participant. Each participant answers before
`deadlines.accept` with `POST /v1/matches/{match_id}/accept` or `POST /v1/matches/{match_id}/decline`.

- Everyone accepts: the match becomes `ready` (`match.ready`), tickets end.
- Someone declines, or the deadline passes with answers missing: the match becomes `cancelled` (`match.cancelled`
  with `cancel_reason` `declined` or `accept_timeout`); players who declined or did not answer get an offense; tickets of
  players who accepted go back to `searching` keeping their original creation time (their priority); the tickets of
  the others end (a party with a decliner leaves the queue as a whole).

How tickets are grouped (rating windows, waiting time, party constraints, shrinking to `min_teams`) is the server's
choice; it SHOULD describe it in its rules.

---

## Matches

The match object is the contract between service and launcher for one game. It is created by a room start or by
matchmaking.

### Match object

The participant view (`GET /v1/matches/{id}` by a participant, and every `match.*` event):

```json
{
  "id": "m_9x2k...",
  "state": "ready",
  "origin": {"kind": "queue", "queue_id": "duel-ranked"},
  "rules": {
    "teams": [1, 1],
    "mode": "<mode id>",
    "stage": null,
    "options": {},
    "rated": true,
    "ladder": "duel",
    "season": "s3",
    "result_policy": "standard"
  },
  "participants": [
    {"user": {"id": "u_a", "display_name": "Camille"}, "member_name": "Camille", "team": "alpha", "role": "host",
     "accept": "accepted", "launched": true, "status": {"state": "in-room", "at": "2026-11-11T20:16:00Z"},
     "reported": false},
    {"user": {"id": "u_b", "display_name": "Lou"}, "member_name": "Lou", "team": "bravo", "role": "join",
     "accept": "accepted", "launched": false, "status": null, "reported": false}
  ],
  "you": {"user_id": "u_a", "member_name": "Camille", "team": "alpha", "role": "host"},
  "game": {
    "contract": {"id": "lan-auto-match", "version": 1},
    "match_id": "K7Q2MXA9PL4DZB3W8R6TNY5HCF",
    "relay": {"host": "relay.example.org", "port": 47313, "room": "K7Q2MXA9PL4DZB3W8R6TNY5HCF"},
    "players": 2,
    "rules": "players=2,teams=Camille:A,Lou:B,mode=<mode id>",
    "host_name": "Camille",
    "timeouts": null
  },
  "deadlines": {"accept": null, "launch": "2026-11-11T20:17:03Z", "result": "2026-11-11T20:45:03Z"},
  "created_at": "2026-11-11T20:15:03Z",
  "updated_at": "2026-11-11T20:16:00Z",
  "ended_at": null,
  "cancel_reason": null,
  "outcome": null
}
```

| Member | Meaning |
|---|---|
| `state` | see [Lifecycle](#lifecycle). Unknown state: treat as `running` and refetch later |
| `origin` | `{"kind": "room", "room_id"}` or `{"kind": "queue", "queue_id"}`; unknown kind: show nothing |
| `rules` | **authoritative** rules: actual `teams` sizes of this match, `mode`, `stage` (`null` = the game's choice), `options` (the server's other options, echoed), whether it is `rated`, its `ladder` and `season`, and the `result_policy` id |
| `participants` | every player, in a fixed order. `member_name`: the name this player uses in the game and on the relay ([match-contract.md](match-contract.md#member-names)); `team`: `alpha` or `bravo`; `role`: `host` (creates the game session) or `join`; `accept`: `pending`, `accepted`, `declined` (`accepted` for room matches); `launched`: the launcher reported the launch; `status`: the last mirrored game state and when it was received, or `null`; `reported`: a terminal report (result or error) was received |
| `you` | the caller's own entry, for convenience; 1.1.10: `free: true` when the caller's report is in and the match no longer holds it ([Lifecycle](#lifecycle)) |
| `game` | the values of the [match contract](match-contract.md). Present only to participants and only from state `ready` on |
| `deadlines` | `accept` (state `pending_accept`), `launch` (every participant must have reported its launch), `result` (the latest time for results). A deadline MAY move (the server sends `match.updated`) |
| `cancel_reason` | `null`, or for `cancelled`: `declined`, `accept_timeout`, `room_closed`, `admin`, 1.1.6 `left` (a player abandoned the draft) (unknown: show nothing) |
| `outcome` | `null` until the match ends `completed` or `void` ([Outcome](#outcome-object)) |
| `draft` | 1.1.6, optional: the match's stage (and mode) draft ([draft.md](draft.md#3-the-draft-object)); kept after the match for its history |
| `spectators` | 1.1.7, participants only: the match's spectators (`user`, `member_name`, `role`, `referee`, `launched`, `left`, `status`, `reported`), shown to every player; never in `participants`. With them `you.team` is `spectator` for a spectator, `you.seat` `player` or `spectator`, `rules.spectators` the places ([spectators.md](spectators.md#4-the-match-object)) |

Non-participants (with `stats`) get the public view of an ended match: `id`, `state`, `origin`, `rules`,
`participants` (only `user`, `team`), `ended_at`, `outcome` (without `players` verdicts). They get `404 not_found`
for a match that has not ended.

### Lifecycle

```
 queue:   pending_accept --all accept--> ready --first launch--> running --decision--> completed | void
             |  decline / accept timeout                |  launch deadline missed (no-show)
             v                                           v
          cancelled                                   void
 room start:                    ready ...
 1.1.6, with a draft:  pending_accept --all accept--> drafting --draft done--> ready ...   (room start: drafting ...)
```

| State | Meaning | Launcher |
|---|---|---|
| `pending_accept` | found by matchmaking, waiting for every participant's answer | show accept / decline with the countdown to `deadlines.accept` |
| `drafting` | 1.1.6: every player is in; the draft decides the stage (and mode); no `game` yet | show the draft ([draft.md](draft.md)); unknown to a launcher: waiting |
| `ready` | rules final, `game` present; waiting for launches | launch the game ([match-contract.md](match-contract.md)), then `POST .../launch` |
| `running` | at least one launcher reported its launch | mirror status, upload the result |
| `completed` | the server accepted a result (`outcome.counted` true) | show the outcome |
| `void` | no result counts (aborted, disagreement, insufficient reports, no-show) | show `outcome.summary` |
| `cancelled` | ended before any launch (declined, accept timeout, room closed) | back to the menu |

A player is in at most one match between `pending_accept` and its end. `GET /v1/matches/current` returns it.

1.1.10 (a server that declares `matches.free_after_report`): from its terminal report on (a result or an error), a
participant has nothing left to do in a match that waits for the others' reports, and the match no longer holds it:
`GET /v1/matches/current` returns another match or `null`, the participant may search, join a room or take a place in
the stands, and it gets no more `match.updated` of that match. The decision still applies to it (its verdict, its
rating, an offense) and still reaches it as `match.completed` or `match.void`; its view of the match says
`you.free: true` until then. Exceptions, held until the decision as before: a match of a [series](series.md) (its next
game keeps the same players), and a report that counts as leaving a queue match (the offense `left`: errors
`left-by-player` and `launcher-cancelled`). Why: when one player of a 1v1 vanishes with its launcher (a computer that
crashed), the match would otherwise wait for its result deadline and hold the other player (`409 in_match`) all
along ([leaving.md](leaving.md)).

### Endpoints

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `GET /v1/matches/current` | — | `{"match": Match or null}` | |
| `GET /v1/matches/{match_id}` | — | `200` Match | `404 not_found` |
| `POST /v1/matches/{match_id}/accept` | — | `200` Match (idempotent) | `409 match_state`, `404 not_found` |
| `POST /v1/matches/{match_id}/decline` | — | `200` Match (1.1.6: during `drafting` it abandons the match, `cancel_reason: left`) | `409 match_state`, `404 not_found` |
| `POST /v1/matches/{match_id}/draft` | `{"turn": 1, "choices": ["<id>"]}` | `200` Match (1.1.6, area `draft`, [draft.md](draft.md#4-acting)) | `403 not_your_turn`, `409 draft_turn`, `409 match_state`, `422 validation_failed`, `429 rate_limited` |
| `POST /v1/matches/{match_id}/launch` | `{}` | `200` Match (idempotent) | `409 match_state` (not `ready`/`running`) |
| `POST /v1/matches/{match_id}/status` | the game's status file, verbatim | `204` | `409 match_state`, `413 payload_too_large`, `422 validation_failed` (not an object with a string `state`) |
| `POST /v1/matches/{match_id}/result` | the game's result file verbatim, or a launcher error report | `200` `{"match": Match, "report": {"accepted": true, "verdict": null}}` | `409 match_state` (match ended), `409 already_reported` (a different report was already received; an identical one gets `200`), `413 payload_too_large`, `400 invalid_json` |

**Status mirror.** The body is the JSON object of the game's status file ([match-contract.md](match-contract.md#status-mirror)).
The server keeps the last one per participant, records whether the participant reached `in-match`, and shows the
others `status.state`. A launcher sends it at every change of state (at most 2 per second; the server MAY drop the
excess with `429`).

**Result upload.** The body is the result file as the game wrote it, or, when the game wrote none, a launcher error
report `{"version": 1, "match_id": "<game.match_id>", "error": "launcher-crashed", "reason": "..."}`
([match-contract.md](match-contract.md#4-what-the-launcher-reports)). 1.1.11: a launcher that reports its player's
departure (`left-by-player`) copies the game's `in_play` (boolean) from the status file into the report: `true` when
the player left the match while it was being played ([leaving.md](leaving.md#4-forfeit-1111)). The server records it whatever it contains (a
malformed or wrong record is recorded as an invalid report, not refused), answers `200`, and the
[decision](#result-decision) happens when its conditions are met. `report.verdict` is the uploader's
[verdict](#outcome-object) string, `null` until the decision.

### Outcome object

```json
{
  "decision": "agreed",
  "counted": true,
  "winner": "bravo",
  "rated": true,
  "unrated_reason": null,
  "summary": {"en": "Result confirmed by 2 of 2 reports."},
  "record": {
    "rule": 0, "mode": "<mode id>", "stage_id": 3, "stage_scene": "<stage id>", "battle_frames": 10800, "duration_s": 180.0,
    "win_team": 1, "winner": "bravo", "deemed_result": -1, "end_utc": 1794346807,
    "teams": [{"team": "alpha", "paint_points": 28, "turf_permille": 11},
              {"team": "bravo", "paint_points": 57, "turf_permille": 22}],
    "players": [{"user_id": "u_a", "name": "Camille", "team": "alpha", "paint": 27, "kills": 0, "assists": 0,
                 "deaths": 0, "specials": 0, "disconnected": false},
                {"user_id": "u_b", "name": "Lou", "team": "bravo", "paint": 56, "kills": 0, "assists": 0,
                 "deaths": 0, "specials": 0, "disconnected": false}]
  },
  "reports": {"expected": 2, "results": 2, "agreeing": 2, "errors": 0, "invalid": 0},
  "players": [{"user_id": "u_a", "verdict": "agreed"}, {"user_id": "u_b", "verdict": "agreed"}],
  "rating_changes": [],
  "rematch_id": null
}
```

| Member | Meaning |
|---|---|
| `decision` | `agreed` (valid results all equal), `majority` (a majority of valid results), `single` (one result, accepted under the policy), `forfeit`, `admin` (set by a moderator or referee), or for `void`: `aborted` (errors), `invalid_reports` (every report failed validation), `disagreement`, `insufficient_reports`, `no_reports`, `no_show`, `loadout_violation`; 1.1.7 `witnessed` (counted: a spectator's report broke the players' disagreement) and `witness_disagreement` (void: a counted witness contradicts the players). Unknown value: show `summary` |
| `spectators`, `witnessed` (1.1.7) | participants only: each spectator's `verdict` (`agreed`, `dissent`, `reported`, `invalid`, `error`, `silent`, `absent`), `referee`, `counted`; whether witnesses decided ([spectators.md](spectators.md#63-the-procedure-with-witnesses)) |
| `invalid_reasons` (1.1.2) | participants only: `[{"user_id", "check": "V7", "text": localized text}]`, one per refused report (`check` is the failed check of the list below, or `format`); empty when none |
| `counted` | `true` for `completed` (a result counts), `false` for `void` |
| `winner` | `alpha`, `bravo` or `null` |
| `rated`, `unrated_reason` | whether ratings changed; reason when a rated match was not (`one_sided`, `no_winner`, 1.1.7 `no_witness`, 1.1.11 `forfeit` (a forfeit under `leave: forfeit_unrated`), ...: open enum, show `summary`) |
| `forfeit` (1.1.11) | with decision `forfeit`: `{"reason", "team", "players": [{"user_id"}]}`: the team that lost by forfeit, the players concerned, and why: `left` (a player left the match in play, its own launcher said so: policy `leave`, step 3b), `disconnected` (`disconnect: forfeit`, step 7), `loadout` (`loadout_check: forfeit`, step 7b); open enum. A launcher that does not know it sees a counted result with a winner and shows `summary` |
| `summary` | localized sentence explaining the decision |
| `record` | the accepted record: the [shared fields](#result-decision) of the result file under their own names, plus `end_utc` (informative; `null` when it carries no integer), the `alpha` and `bravo` teams only, and `user_id` added to each player (`null` for an erased account); `null` for `void`. 1.1.8: one agreeing report as it was written, the one whose counters differ least from the other agreeing reports' (step 2); its player lines are the statistics recorded (`GET /v1/users/{user_id}/matches`) |
| `stats_check` (1.1.8) | with a `record`: `check` (the policy's `stats`), `differ` (a counter or weapon differs between the agreeing reports, by any amount), `flagged` (a gap beyond the tolerance, `stats: flag` only: the result stands; the server logs it for its operator), `gaps` (at most 16: `{"field", "team"` or `"user_id", "values": [smallest, largest]}`), `record_from` (`{"user_id"}`: whose report is the record). Unknown to a launcher: ignored; `summary` says it when flagged |
| `reports` | counts: participants `expected`, `results` received, `agreeing` with the record, `errors`, `invalid` |
| `players` | participants only: each participant's `verdict`: `agreed`, `dissent` (reported another outcome), `invalid` (report failed validation), `error` (reported an error), `silent` (no report), `no_show` |
| `rating_changes` | with `ratings`: [rating changes](#rating-change) of this match |
| `rematch_id` | the match created by a `rematch` policy, else `null` |

---

## Result policies

How a server turns reports into an outcome is **declared**, in `result_policies` of `GET /v1/meta`; rooms and queues
name the policy their matches use. A launcher shows `summary` to the player (for example in a queue's details) and
MAY show the structured members. A server MUST decide as its declared values say.

```json
{
  "id": "standard",
  "name": {"en": "Standard"},
  "summary": {"en": "A result counts when a majority of players report it, counting players who played and stayed silent ..."},
  "quorum": "majority",
  "silence": "consent_after_play",
  "disagreement": "majority",
  "rated_requires_both_teams": true,
  "abort": "void",
  "disconnect": "game_result",
  "no_show": "void",
  "deadlines": {"accept_s": 20, "launch_s": 90, "result_s": 2400, "grace_s": 120},
  "stats": "flag",
  "leave": "forfeit"
}
```

| Member | Values | Meaning |
|---|---|---|
| `quorum` | `any`, `majority`, `all`, or an integer k | how much **support** a result needs: at least 1, more than half of the participants, all of them, or at least min(k, participants) |
| `silence` | `consent_after_play`, `ignore` | `consent_after_play`: a participant who sent no terminal report but whose mirrored status reached `in-match` supports the candidate result; `ignore`: silence counts for nothing |
| `disagreement` | `majority`, `void`, `rematch` | when valid results differ: the strict majority wins, the match is void, or it is void and replayed |
| `rated_requires_both_teams` | boolean | a rated match changes ratings only if each team confirmed the record ([step 8](#result-decision); 1.1.12: with a valid result of its own, or a referee's; up to 1.1.11: a player of each team in the support, silences and errors included). `false` lets one team's reports rate a match: the reference server warns about it on a rated queue |
| `abort` | `void`, `rematch` | when no valid result arrives |
| `disconnect` | `game_result`, `forfeit` | `game_result`: the record decides (a player marked `disconnected` simply played less); `forfeit`: a team with a disconnected player loses (if both have one, the record decides) |
| `no_show` | `void`, `rematch` | when a participant has not launched by `deadlines.launch` |
| `deadlines` | numbers of seconds | `accept_s` (queue matches), `launch_s` after `ready`, `result_s` after `ready`, `grace_s` after the first valid result |
| `loadout_check` (1.1.1) | `ignore` (default), `void`, `forfeit` | checks `rules.loadout` against each player's `weapon` and `gear` in the accepted record ([match-contract-2.md](match-contract-2.md#5-loadout-constraints)) |
| `decider` (1.1) | `auto` (default), `referee`, `auto_then_referee` | who decides ([operators.md](operators.md#3-pluggable-result-policies-including-a-human-referee)) |
| `witness` (1.1.7) | `ignore` (default), `tiebreak`, `required` | what spectators' reports are used for: nothing, breaking a disagreement between players, or also confirming a rated result ([spectators.md](spectators.md#6-witness-reports-and-the-decision)) |
| `witness_from` (1.1.7) | `any` (default), `referees` | whose spectator reports count |
| `leave` (1.1.11) | `void` (default), `forfeit`, `forfeit_unrated` | what a player's voluntary departure from a match in play does when the match has **no valid result** ([step 3b](#result-decision), [leaving.md](leaving.md#4-forfeit-1111)): `void` — the match is void; `forfeit` — the leaver's team loses, counted and rated as any result; `forfeit_unrated` — it loses, no rating changes. Only the leaver's own report proves a departure (`left-by-player` with `in_play: true`): a crash, a lost connection, a silence never make anyone lose. The offense `left` ([Penalties](#penalties)) applies as before |
| `stats` (1.1.8) | `flag`, `ignore`, `strict`; absent: `strict` | what the reports' **counters** weigh ([step 2](#result-decision)): `flag` — reports that agree on the outcome agree, their counters are compared with the tolerance below and a larger gap is flagged (`outcome.stats_check`) without voiding; `ignore` — counters are not compared; `strict` — every shared field must be equal (a declaration without the member means `strict`). The reference service declares it on every policy, `flag` unless configured |

Unknown values: the launcher shows `summary` only. A server MAY add `x-` members to describe further rules.

### Result decision

This procedure is normative for the values above. Notation: *n* participants; each participant has at most one
**terminal report**: a *result* (a record), an *error* (an object with `error`) or nothing (*silent*).

**When.** The server decides at the earliest of: every participant has a terminal report; `grace_s` after the first
valid result; `deadlines.result`. It decides at once, as `void` with decision `no_show`, when `deadlines.launch`
passes while a participant has not reported its launch (`no_show` policy `void`), and tells the launchers
(`match.void`) so they can close the game. 1.1.11: under a policy whose `leave` is not `void`, a match of **two**
players one of whom left in play (`left-by-player` with `in_play: true`) is also decided `grace_s` after that report:
the other game ends with the match (no record can come) and its launcher may be gone. With more players the
others' games may play on and record the match: they are waited for as before.

**1. Validation.** A result is *valid* when all of these hold (else the report is *invalid*; the reporter's verdict
is `invalid`):

| # | Check |
|---|---|
| V1 | a JSON object with `version` 1 and no `error` member (1.1: or `version` 2 with `outcome: completed`; 1.1.7: and `seat`, when present, `player`) |
| V2 | `match_id` equals `game.match_id` |
| V3 | `role` is the uploader's `role` (`host` or `join`) |
| V4 | `players` has exactly one entry per participant, and its `name`s are exactly the participants' `member_name`s |
| V5 | each player's `team` is that participant's `team` |
| V6 | exactly one player has `self: true`, and it is the uploader's `member_name` |
| V7 | `mode` equals `rules.mode`; when `rules.stage` is not `null`, `stage_scene` equals it |
| V8 | `teams` has entries `alpha` and `bravo`; `win_team` is 0, 1 or -1 and `winner` is respectively `alpha`, `bravo` or `none` |
| V9 | numbers in range: `battle_frames` 1..216000, `kills`, `assists`, `deaths`, `specials` 0..9999, `paint` and `paint_points` 0..999999, `turf_permille` 0..1000 |
| V10 | contract 2: a host's `settings_ok` is not `false` ([match-contract-2.md](match-contract-2.md#7-reports-of-contract-2)) |
| V11 | 1.1.8: a record of `rule` 0 with a winner (`win_team` 0 or 1) and `deemed_result` -1 is not one whose declared loser leads the winner by more than 50 `turf_permille` **and** on `paint_points` |

V11 catches a winner flipped by hand in a file whose scores were left as they were. Each game computes its own
scores, which may differ slightly between machines, hence the margin of 50 and the two scores required. Under other
rules, and when a deemed result decided the match, nothing is checked. A coherent forgery (winner and scores
swapped) passes V11 and is caught by step 2.

An error report is any object with a string `error` (1 to 64 characters).

1.1.7: a **spectator's** report is never one of these terminal reports: it is a witness report, checked by the witness
reading of V1 to V11 (V1: a version 2 file with `outcome: spectated`; V6: no entry with `self: true`) and used only as
the policy's `witness` says (step 4b, step 6 and step 7c of [spectators.md](spectators.md#63-the-procedure-with-witnesses));
a spectator's error, silence or absence never voids a match and is no offense.

**2. Grouping.** The **shared fields** of a result are `rule`, `mode`, `stage_id`, `stage_scene`, `win_team`,
`winner`, `deemed_result`, `teams` (in order: `team`, `paint_points`, `turf_permille`), `players` (in order: `name`,
`team`, `paint`, `kills`, `assists`, `deaths`, `specials`, `disconnected`; contract 2: `weapon`). Never compared:
`role`, `self`, `end_utc`, `battle_frames` and `duration_s` (1.1.2: the engines of one match may count a few frames
apart), `t0_unix`, `timeline_s`, unknown members.

1.1.8: the shared fields are of two kinds. What **decides** the match: `rule`, `mode`, `stage_id`, `stage_scene`,
`win_team`, `winner`, `deemed_result`, and every player's `name`, `team` and `disconnected` (step 5 and
`disconnect: forfeit` read it), compared by name (the order of the players decides nothing); a player's `weapon` too
when the policy's `loadout_check` is not `ignore` and the match has a `loadout`. The **counters**: every team's
`paint_points` and `turf_permille`, every player's `paint`, `kills`, `assists`, `deaths`, `specials` (and `weapon`
otherwise). The counters are each game's own and may differ slightly between the reports of one match.

Two valid results are *equal* when, by the policy's `stats`:

| `stats` | Equal when |
|---|---|
| `flag`, `ignore` | what decides is equal; counters may differ |
| `strict` (and a policy that does not declare `stats`) | every shared field is equal, in order |

With `flag`, the counters of the candidate's reports (step 4) are compared: a counter whose smallest and largest values
differ by more than **both** its floor and its share of the larger value is a *gap*. The match is decided as without
it; the outcome's `stats_check` lists the gaps (`flagged: true`), the `summary` says it, the server logs it for its
operator. Tolerances (wide on purpose: a gap means an edited file or a game badly out of step, not the games'
ordinary difference):

| Counter | Floor | Share |
|---|---|---|
| `turf_permille` | 30 | 25 % |
| `paint_points`, `paint` | 100 | 25 % |
| `kills`, `assists`, `deaths`, `specials` | 2 | 50 % |
| `weapon` (when it does not decide) | any difference | |

**The record kept** (the outcome's `record`, and so the statistics recorded) is one of the candidate's reports as it
was written: the one whose counters and weapons differ, in number of fields, least from the other agreeing reports'
and from the agreeing reports of counted witnesses ([spectators.md](spectators.md#63-the-procedure-with-witnesses));
on a tie the host's, then the first in participant order. So with two reports it is the host's report when the host
agrees (its `battle_frames` included), and with three or more one report alone that strays from the
others (an edited file, a game out of step), while they match each other, is never the one recorded. Why not each
player's own line, or a mix: each player could then
inflate their own statistics, and a mix of reports is no record any game wrote (its winner could show a lower score
than the loser in a close match).

**3. No valid result.** The match is `void`: decision `aborted` when at least one error report exists,
`invalid_reports` (1.1.2) when reports arrived but every one failed validation — the outcome's `invalid_reasons`
says which check and why — and `no_reports` otherwise (`abort: rematch` additionally creates a rematch).

**3b. Departure (1.1.11).** Under a policy whose `leave` is `forfeit` or `forfeit_unrated`, before step 3 voids the
match: a team loses by forfeit (`completed`, decision `forfeit`, the other team the winner, `record` `null`,
`outcome.forfeit` with `reason: left`) when all of these hold:

- a participant's own report is `left-by-player` with `in_play: true`, and its mirrored status reached `in-match`;
- every such participant is in the same team (players of both teams left: void);
- no other participant's terminal report was received before the first of them (the departure is the first thing that
  went wrong: a crash or a lost connection reported earlier means the match was already lost);
- a participant of the other team reached `in-match`.

The match is rated when `rules.rated`, `leave` is `forfeit` and `witness` is not `required` (a witness cannot confirm a
match that ended without a record); else `unrated_reason` `forfeit` or `no_witness`. `rated_requires_both_teams` does
not apply: the losing team's own report is the proof, and the winners reached the match. Only a participant's own
report can make its team lose, so a report can only harm its author's team. Spectators are never concerned. When a
valid result exists, steps 4 to 8 decide as before: a record the other games agree on counts whatever the departure
(with `disconnect` deciding what a player shown `disconnected` changes; 1.1.12: when a teammate of the leaver
reported it, step 5), and a record that shows the leaver playing to the end is contradicted by its error (step 5,
void).

**4. Candidate.** If all valid results are equal, they are the candidate (decision `agreed`). Otherwise, by
`disagreement`: `majority` — the largest group is the candidate (decision `majority`) if it has more results than
all other groups together, else `void` with `disagreement`; `void` — `void` with `disagreement`; `rematch` — `void`
with `disagreement` and a rematch. Reporters outside the candidate's group get the verdict `dissent`.

**5. Errors (1.1.12).** An error report **never supports** a result. An error from participant *p* is *explained* by
the candidate record when all of these hold, and then it neither supports nor contradicts it (*p* is left out of the
count of step 6):

- its code is one that happens after the match started (`match-aborted`, `record-timeout`, `connection-lost`,
  `left-by-player`, `launcher-crashed`); any other code says the match never began for *p*;
- *p*'s mirrored status reached `in-match`;
- the record shows *p* with `disconnected: true`;
- a participant of *p*'s own team, other than *p*, is a reporter of the candidate: *p*'s own team's game recorded *p*
  leaving.

Any other error **contradicts** the candidate: the match is `void` with `disagreement`, and every reporter of a valid
result, the candidate's group included, gets the verdict `dissent`. In particular, in a 1v1 an error is never
explained (no game writes a record once the other player is gone, [leaving.md](leaving.md#1-what-the-game-does)), and a
record from one team alone can never mark a player of the other team gone.

**6. Support and quorum.** Support = the reporters of the candidate's group, plus, with `silence: consent_after_play`,
every silent participant whose status reached `in-match`. The quorum is checked on the support out of the participants
whose error is not explained (step 5). If the support is below `quorum`, the match is `void` with
`insufficient_reports`. A candidate backed by one valid result only is decided as `single`. (So in a team match a player
who quits mid-match, whose teammates' games record it leaving, does not void the match.)

**7. Winner.** `winner` of the record (`null` for `none`). With `disconnect: forfeit`, if the players marked
`disconnected` are all in one team, the other team wins (decision `forfeit`; 1.1.11: `outcome.forfeit` with `reason:
disconnected`).

**7b. Loadout (1.1.1).** When the match has a `loadout` and the policy's `loadout_check` is not `ignore`, the record's
`weapon` and `gear` are checked against it ([match-contract-2.md](match-contract-2.md#5-loadout-constraints)). With
`forfeit`, when the players who broke it are all in one team, the other team wins (decision `forfeit`,
`outcome.forfeit` with `reason: loadout`); otherwise, and with `void`, the match is `void` with `loadout_violation`.

**8. Rating (1.1.12).** A team **confirms** the record when one of its participants is a reporter of the candidate (a
valid result equal to it); an error, a silence or another team's report never confirm for a team. A counted witness
who is one of the operator's referees ([spectators.md](spectators.md#63-the-procedure-with-witnesses)) and whose record
equals the candidate confirms for every team. The match is rated when `rules.rated`, the winner is not `null`, and (if
`rated_requires_both_teams`) both teams confirmed it; else `rated` is `false` with `unrated_reason` `no_winner` or
`one_sided`. The departure forfeit of step 3b keeps its own rule: the proof is the losing team's own
report.

**What one participant can do alone** (every policy; a cheater controls its own launcher, its reports and its
statuses, and may have a second account in the stands; the other side's launchers are honest):

| The cheater's report | The other side's honest report | Outcome |
|---|---|---|
| a forged record (its team wins, the other side shown gone or not) | a valid record | `void`, `disagreement` (unless a strict majority of reports agree) |
| a forged record | an error before the match (`players-timeout`, `host-left`, ...) | `void`, `disagreement` (step 5) |
| a forged record | an error in the match (`connection-lost`, `launcher-crashed`, ...) | `void`, `disagreement` (step 5: nobody of the other side's team recorded the departure) |
| a forged record | nothing, the other side's status reached `in-match` | `silence: consent_after_play`: counted (`single`), **never rated** (`one_sided`); `silence: ignore`: `void` unless `quorum` counts one report (`any`) |
| a forged record | nothing, its status never reached `in-match` | `void` (`insufficient_reports`), unless `quorum` counts one report (`any`): then counted, never rated |
| a forged record, and a second account in the stands that "witnesses" it | a valid record | `witness: tiebreak` with `witness_from: any`: counted (`witnessed`), **never rated**; with `witness_from: referees`: `void` |
| any error, or nothing | anything | the match is `void` (a player can always refuse a result by reporting an error: nobody can tell a failure on purpose from a real one) |
| `left-by-player` with `in_play: true` (step 3b) | anything | only the cheater's **own** team can lose by it |

So a cheater alone never changes a rating against players who report, and never wins a counted result against a
player whose game reported a failure. What remains: a counted, unrated result against a player who played and then
sent nothing (a silence is consent under `consent_after_play`), and under the operator's own choices (`quorum: any`,
`witness_from: any` with stands open to anyone, `rated_requires_both_teams: false`) a counted result on one report,
which then also counts in a series or a tournament.

**Guarantees** (with the reference policy `standard`: `quorum: majority`, `silence: consent_after_play`,
`disagreement: majority`): one player alone cannot impose a false result on players who report; one team's
reports alone never rate a match; a player cannot avoid a counted loss by not reporting once the match has started
(it avoids the rating change, as it could by reporting an error); a 1v1 with one result against
another is void; a group that reports the same false record and is a majority wins (collusion is out of reach of any
cross-check: see [overview.md](overview.md#32-what-the-service-can-know--and-what-it-cannot)). With `stats:
flag` (the reference default): games whose counters differ slightly agree; a 1v1 whose reports name different
winners is still void; a winner flipped by hand in a file whose scores contradict it is refused (V11); counters edited by one player
change no outcome, are flagged beyond the tolerance, and with three reports or more are never the ones recorded.

---

## Ratings

Areas `ratings` and `leaderboards`. **Rating systems are the server's business**: this protocol does not prescribe
any formula. A server groups rated queues into **ladders**, may run **seasons**, and exposes ratings as presentable
data the launcher renders without interpreting.

### Ladder object

```json
{
  "id": "duel",
  "name": {"en": "Duel"},
  "description": {"en": "Ranked 1v1."},
  "queues": ["duel-ranked"],
  "season": {"id": "s3", "name": {"en": "Season 3"}, "starts_at": "...", "ends_at": null},
  "tiers": [{"id": "gold", "name": {"en": "Gold"}, "color": "#D4AF37", "order": 3}],
  "leaderboard": true
}
```

`tiers` (optional) lists the tier labels the ladder uses, `order` ascending from lowest; `color` is `#RRGGBB` (a hint).
`season` is the current season or `null` (no seasons).

### Rating view

```json
{
  "ladder": "duel",
  "season": "s3",
  "value": 1423,
  "display": "1423",
  "tier": {"id": "gold", "name": {"en": "Gold"}, "color": "#D4AF37", "order": 3},
  "progress": {"value": 73, "max": 200, "label": {"en": "73 / 200 to Platinum"}},
  "provisional": {"remaining": 0, "label": null},
  "last_change": {"delta": 12, "match_id": "m_...", "at": "...", "reason": {"en": "Win against a higher-rated opponent"}},
  "fields": [{"key": "wins", "label": {"en": "Wins"}, "value": 14}, {"key": "losses", "label": {"en": "Losses"}, "value": 9}]
}
```

Every member but `ladder` is optional or `null`: `value` (a number to show, or `null`), `display` (how to show it,
preferred over `value`), `tier`, `progress` (a bar), `provisional` (placement: `remaining` matches before the rating
is shown, with a label), `last_change`, and `fields` (extra `{"key", "label", "value"}` rows; `value` is a string or
a number).

### Rating change

In `outcome.rating_changes` and the event `rating.changed`:

```json
{"user_id": "u_a", "ladder": "duel", "season": "s3", "delta": 12,
 "before": {"display": "1411", "tier": {"id": "gold", ...}}, "after": {"display": "1423", "tier": {"id": "gold", ...}},
 "reason": {"en": "Win"}}
```

### Endpoints

| Endpoint | Answer | Errors |
|---|---|---|
| `GET /v1/ladders` | `{"items": [Ladder]}` | |
| `GET /v1/seasons` | `{"items": [{"id", "name", "starts_at", "ends_at", "current"}]}` (newest first) | |
| `GET /v1/users/{user_id}/ratings?season=<id>` | `{"items": [Rating view]}` (one per ladder the player has a rating in; default: current seasons) | `404 not_found` |
| `GET /v1/ladders/{ladder_id}/leaderboard?season=<id>` *paginated* | Leaderboard | `404 not_found` |

### Leaderboard

```json
{
  "ladder": "duel",
  "season": "s3",
  "columns": [
    {"key": "rank", "label": {"en": "#"}, "type": "rank"},
    {"key": "player", "label": {"en": "Player"}, "type": "player"},
    {"key": "tier", "label": {"en": "Tier"}, "type": "tier"},
    {"key": "rating", "label": {"en": "Rating"}, "type": "number"},
    {"key": "games", "label": {"en": "Games"}, "type": "number"}
  ],
  "items": [{"user_id": "u_a", "cells": {"rank": 1, "player": {"id": "u_a", "display_name": "Camille"},
             "tier": {"id": "master", "name": {"en": "Master"}, "color": "#9B59B6", "order": 6},
             "rating": 1987, "games": 41}}],
  "next_cursor": null,
  "you": null
}
```

Column `type`: `rank` (integer), `player` (User), `number`, `text` (string), `tier` (tier object), `percent` (number
0-100), `delta` (signed number). Unknown type: show the cell as text if it is a string or a number, else blank.
`you` is the caller's row (or `null`) so a launcher can show it under the list. Players who turned
`show_on_leaderboards` off are left out.

---

## Statistics and history

Area `stats`. Visible to everyone on the server unless the player turned `public_profile` off (then only to friends
and to themselves: others get `403 private_profile`).

### `GET /v1/users/{user_id}/stats`

Presentable groups chosen by the server:

```json
{"user_id": "u_a", "groups": [
  {"key": "all", "label": {"en": "All matches"}, "fields": [
    {"key": "matches", "label": {"en": "Matches"}, "value": 23},
    {"key": "win_rate", "label": {"en": "Win rate"}, "value": "61%"}]},
  {"key": "queue:duel-ranked", "label": {"en": "Ranked duel"}, "fields": [ ... ]}
]}
```

### `GET /v1/users/{user_id}/matches` *paginated*

Newest first; ended matches only (`completed` and `void`):

```json
{"items": [{
  "match_id": "m_...", "ended_at": "...", "state": "completed", "origin": {"kind": "queue", "queue_id": "duel-ranked"},
  "rules": {"teams": [1, 1], "mode": "<mode id>", "stage": null, "rated": true, "ladder": "duel"},
  "team": "bravo", "result": "win", "stage": "<stage id>",
  "stats": {"paint": 56, "kills": 0, "assists": 0, "deaths": 0, "specials": 0, "disconnected": false},
  "rating_delta": 12
}], "next_cursor": null}
```

`result` is `win`, `loss` or `none` (void, or no winner). 1.1.11: a match won or lost by forfeit also has `forfeit`,
the outcome's (`reason`, `team`, `players`). `stats` is the player's line of the accepted record (1.1.8:
the report kept, [step 2](#result-decision)), `null` when void. `rating_delta` is a number or `null`.

---

## Moderation

Area `moderation`.

| Endpoint | Body | Answer | Errors |
|---|---|---|---|
| `POST /v1/reports` | `{"user_id": "u_b", "match_id": "m_..." or null, "category": "cheating", "comment": "..."}` | `201` `{"id": "rp_...", "created_at": "..."}` | `404 not_found` (user or match), `422 validation_failed` (unknown category; comment over 1000 characters), `429 rate_limited` |
| `GET /v1/blocks` | — | `{"items": [{"user": User, "since": "..."}]}` | |
| `PUT /v1/blocks/{user_id}` | — | `204` (also ends a friendship and pending requests between the two) | `404 not_found` |
| `DELETE /v1/blocks/{user_id}` | — | `204` | `404 not_found` |

`category` is an id from `moderation.report_categories`. A blocked player cannot send the blocker friend requests,
party or room invitations; the server MAY avoid matching them together. Reports are reviewed by the operator; the
reporter is never told the outcome through the API.

**Bans.** A banned account gets `403 banned` (with `retry_after_s` when temporary) on every endpoint except `GET
/v1/me`, `GET /v1/me/export`, `DELETE /v1/me`, `POST /v1/auth/logout` and `POST /v1/auth/refresh`; the ban is listed
in `restrictions`. A login to a banned account succeeds (so the player can read why, export and erase).

---

## Population

Area `population` (1.1.4): how many players are around, so a launcher can show "12 online, 4 in a match" before and
after login.

### `GET /v1/population` — *public*

```json
{"online": 12, "in_match": 4, "searching": 2, "updated_at": "2026-11-11T20:15:03Z"}
```

| Member | Meaning |
|---|---|
| `online` | distinct accounts with at least one open event connection ([events.md](events.md)) or an authenticated request within the last 2 minutes |
| `in_match` | optional: distinct players in a match in progress, from the moment every participant accepted (`ready`) until the reports decide it (`running` ends) |
| `searching` | optional: distinct players with an active queue ticket (searching, or a found match not yet accepted) |
| `updated_at` | when the numbers were computed (RFC 3339) |

Every count is an integer ≥ 0; a server MAY omit `in_match` or `searching` (a launcher then shows only what is
present). The answer names nobody and gives no detail per queue, room or match. A server SHOULD compute it at most
every 10 seconds and serve the same answer meanwhile, with `Cache-Control: max-age=10`; it limits requests per
address (`429 rate_limited`). A launcher SHOULD NOT poll it more often than every 10 seconds. A server that does not
declare `population` answers `404 not_found`, exactly as a server that has never heard of it (this endpoint is the
exception to the `feature_disabled` rule of undeclared areas).

---

## Test clock

Only on servers that declare `test-clock` in `features` — never on a production server. It lets a conformance suite
reach deadlines without waiting.

`POST /v1/_test/clock` with `{"advance_s": 30}` moves the server's clock forward and runs every time-based transition
that became due. Answer `200` `{"time": "..."}`. Without `test-clock`, the path answers `404 feature_disabled`.

---

## Error codes

| HTTP | `code` | Meaning |
|---|---|---|
| 400 | `invalid_json` | body is not JSON or not the expected JSON type |
| 400 | `bad_request` | the request cannot be served as sent: `GET /v1/events` without a WebSocket upgrade (reference server) |
| 408 | `bad_request` | the request body did not arrive in time (reference server: 30 seconds) |
| 400 | `bad_cursor` | unknown or expired cursor |
| 400 | `invalid_state` | OAuth `state` unknown, expired or used |
| 400 | `pkce_mismatch` | the verifier does not match the challenge |
| 401 | `unauthorized` | no, unknown or malformed token |
| 401 | `token_expired` | access token expired: refresh |
| 401 | `session_revoked` | the session was revoked (logout, refresh reuse, erasure): log in again |
| 403 | `forbidden` | not allowed (generic) |
| 403 | `banned` | the account is banned (`retry_after_s` when temporary) |
| 403 | `blocked` | one player blocked the other, or the target does not accept this |
| 403 | `not_friends` | the action needs a friendship |
| 403 | `not_leader` | party leader only |
| 403 | `not_owner` | room owner only |
| 403 | `wrong_password` | room password wrong; 1.1.5: the address or the password of a password login (the same answer whether or not the account exists) |
| 403 | `kicked` | kicked from this room |
| 403 | `not_eligible` | a queue requirement is not met (`details.requirements`) |
| 403 | `queue_cooldown` | the player has a queue cooldown (`retry_after_s`) |
| 403 | `private_profile` | the player's statistics are private |
| 404 | `not_found` | no such object, or not visible to the caller |
| 404 | `feature_disabled` | the area is not declared by this server |
| 409 | `conflict` | generic state conflict |
| 409 | `already_in_party`, `party_full`, `party_busy`, `party_size` | party state |
| 409 | `already_in_room`, `room_full`, `team_full`, `room_state`, `not_ready`, `not_enough_players` | room state |
| 409 | `already_in_queue`, `queue_closed`, `match_found`, `in_room` | queue state |
| 409 | `in_match` | the player is in a match |
| 409 | `match_state` | the match is not in a state that allows this |
| 409 | `already_reported` | a different terminal report was already received |
| 409 | `incompatible_game` | a participant's game cannot run these rules (1.1: `details.players[].missing`) |
| 409 | `stale_view` | 1.1: the activity view acted on is out of date |
| 409 | `action_unavailable` | 1.1: the activity action is disabled now |
| 403 | `not_your_turn` | 1.1.6: the draft's current turn is not the caller's to play |
| 409 | `draft_turn` | 1.1.6: the draft turn acted on is not the current one, or the team already chose |
| 409 | `match_started` | 1.1.7: the match is launched (no place any more), or a player's stage loads (a spectator's launch) |
| 409 | `spectators_full`, `spectators_closed` | 1.1.7: every place in the stands is taken; the room has no places |
| 403 | `spectators_not_allowed` | 1.1.7: the places are reserved (`details.who`) |
| 403 / 409 | `spectators_refused` | 1.1.7: a player refuses to be watched |
| 409 | `already_friends`, `request_exists`, `friend_limit` | friends |
| 409 | `name_change_too_soon`, `display_name_taken` | account |
| 409 | `account_exists` | 1.1.5: a sign-up with an address an account already uses (servers that do not confirm addresses) |
| 413 | `payload_too_large` | body over the server's limit |
| 422 | `validation_failed` | a member has a bad value (`field`) |
| 422 | `code_required` | 1.1.5: a one-time code was sent (or must be asked for); send the form again with `code` (`field` `/code`) |
| 429 | `rate_limited` | too many requests (`retry_after_s`, `Retry-After`) |
| 500 | `internal` | server bug |
| 502 | `provider_error` | the login provider failed |
| 503 | `unavailable` | maintenance or overload (`Retry-After` MAY be set) |
| 503 | `relay_capacity` | 1.1.13: every relay that could take the match is full ([relay.md](relay.md#43-relays-that-announce-their-load-1113)): a room start is refused (`retry_after_s`); a queue forms no match meanwhile, its tickets wait |

Unknown codes: handle by HTTP status. 1.1.5: an error MAY carry `text`, the same as `message` as [localized
text](#localized-text) for the player; a launcher that has no text of its own for the code SHOULD show it rather than
`message`.

---

## Endpoint index

| Area | Method and path | Auth (*public*: none) |
|---|---|---|
| core | `GET /.well-known/kalamar-server.json` (on the discovery host) | *public* |
| core | `GET /v1/meta` | *public* |
| core | `POST /v1/auth/{provider}/login` · `POST /v1/auth/{provider}/begin` · `POST /v1/auth/{provider}/complete` · `POST /v1/auth/refresh` | *public* |
| core | `POST /v1/auth/logout` | yes |
| core | `GET /v1/me` · `PATCH /v1/me` · `DELETE /v1/me` · `GET /v1/me/export` · `PUT /v1/me/game` · `GET /v1/state` · `GET /v1/users/{user_id}` · `POST /v1/me/password` (1.1.5, optional) | yes |
| core | `GET /v1/events` (WebSocket) · `POST /v1/events/ticket` (answer `200`; [events.md](events.md)) | yes |
| core | `GET /v1/matches/current` · `GET /v1/matches/{match_id}` · `POST /v1/matches/{match_id}/launch` · `POST /v1/matches/{match_id}/status` · `POST /v1/matches/{match_id}/result` | yes |
| queues | `POST /v1/matches/{match_id}/accept` · `POST /v1/matches/{match_id}/decline` · `GET /v1/queues` · `GET /v1/queues/{queue_id}` · `POST /v1/queues/{queue_id}/tickets` · `GET /v1/tickets/current` · `DELETE /v1/tickets/current` | yes |
| rooms | `POST /v1/rooms` · `GET /v1/rooms/current` · `GET /v1/rooms/{room_id}` · `PATCH /v1/rooms/{room_id}` · `POST /v1/rooms/join` · `POST /v1/rooms/{room_id}/leave` · `POST /v1/rooms/{room_id}/invites` · `POST /v1/rooms/{room_id}/kick` · `POST /v1/rooms/{room_id}/owner` · `PUT /v1/rooms/{room_id}/members/{user_id}` · `POST /v1/rooms/{room_id}/ready` · `POST /v1/rooms/{room_id}/start` | yes |
| parties | `GET /v1/party` · `POST /v1/party` · `POST /v1/party/invites` · `DELETE /v1/party/invites/{user_id}` · `GET /v1/party/invites` · `POST /v1/parties/{party_id}/join` · `POST /v1/parties/{party_id}/decline` · `POST /v1/party/leave` · `POST /v1/party/kick` · `POST /v1/party/leader` | yes |
| friends | `GET /v1/friends` · `DELETE /v1/friends/{user_id}` · `GET /v1/friends/requests` · `POST /v1/friends/requests` · `POST /v1/friends/requests/{request_id}/accept` · `POST /v1/friends/requests/{request_id}/decline` · `DELETE /v1/friends/requests/{request_id}` · `POST /v1/me/friend-code` | yes |
| ratings | `GET /v1/ladders` · `GET /v1/seasons` · `GET /v1/users/{user_id}/ratings` | yes |
| leaderboards | `GET /v1/ladders/{ladder_id}/leaderboard` | yes |
| stats | `GET /v1/users/{user_id}/stats` · `GET /v1/users/{user_id}/matches` | yes |
| moderation | `POST /v1/reports` · `GET /v1/blocks` · `PUT /v1/blocks/{user_id}` · `DELETE /v1/blocks/{user_id}` | yes |
| population (1.1.4) | `GET /v1/population` | *public* |
| draft (1.1.6) | `POST /v1/matches/{match_id}/draft` | yes |
| spectators (1.1.7) | `GET /v1/spectate` · `POST`, `DELETE /v1/matches/{match_id}/spectate` · `POST`, `DELETE /v1/series/{series_id}/spectate` | yes |
| test-clock | `POST /v1/_test/clock` | *public* |

---

## 1.1 additions

Every addition is optional for servers (gated by `features`) and ignorable by 1.0 launchers.

| Area | Method and path | Document |
|---|---|---|
| core | `PUT /v1/me/game` accepts `contracts`, `engine`, `options` | [match-contract-2.md](match-contract-2.md#2-declaring-capabilities-put-v1megame-extended) |
| core | `POST /v1/matches/{match_id}/launch` accepts `{"loadout": {...}}` | [match-contract-2.md](match-contract-2.md#5-loadout-constraints) |
| activities | `GET /v1/activities` · `GET /v1/activities/{activity_id}` · `POST /v1/activities/{activity_id}/actions/{action_id}` | [activities.md](activities.md) |
| series | `GET /v1/series/{series_id}` | [series.md](series.md) |
| badges | `GET /v1/users/{user_id}/badges` · `GET /v1/titles` · `PATCH /v1/me` (`featured_badge`, `title`) | [presentation.md](presentation.md) |
| profiles | `GET /v1/users/{user_id}/profile` | [presentation.md](presentation.md#2-profiles) |
| operator | `/v1/operator/...` (bot tokens, not launchers) | [operators.md](operators.md) |
| population (1.1.4) | `GET /v1/population` (public counts) | [above](#population) |
| core (1.1.5) | password accounts as `direct` forms (`purpose`, `group`), `POST /v1/me/password`, Me `password`, field `autocomplete`, error `text`, codes `code_required` and `account_exists` | [above](#password-accounts) |
| draft (1.1.6) | match state `drafting`, the match's `draft`, `POST /v1/matches/{match_id}/draft`, queue `draft`, room option `draft`, `PUT /v1/me/game` `launcher.draft`, codes `not_your_turn` and `draft_turn`, `cancel_reason: left` | [draft.md](draft.md) |
| spectators (1.1.7) | `GET /v1/spectate`, `POST`/`DELETE /v1/matches/{match_id}/spectate` and `/v1/series/{series_id}/spectate`, room options `spectators` and `spectators_who`, room member `team: spectator`, join `seat`, queue and activity `spectators`, match `spectators`, `you.seat`, `rules.spectators`, `PUT /v1/me/game` `engine.spectator_seats` and `launcher.spectate`, policy `witness` and `witness_from`, outcome `spectators` and `witnessed`, setting `allow_spectators`, operator `POST /v1/operator/{matches,series}/{id}/spectators` | [spectators.md](spectators.md) |
| results (1.1.8) | policy `stats`, outcome `stats_check`, check V11, the record kept, grouping by what decides the match | [above](#result-decision) |
| relays (1.1.9) | meta `relays` and `revision`, `PUT /v1/me/game` `launcher.relay_rtt_ms`, `meta.changed` data `revision` | [relay.md](relay.md#4-choosing-the-relay-of-a-match) |
| matches (1.1.10) | meta `matches.free_after_report`, match `you.free`: a participant whose report is in is free of a match that waits for the others | [above](#lifecycle), [leaving.md](leaving.md) |
| results (1.1.11) | policy `leave` (`void`, `forfeit`, `forfeit_unrated`), step 3b, error report member `in_play`, outcome and history `forfeit`, `unrated_reason: forfeit`; a two-player match left in play decided `grace_s` after the departure | [above](#result-decision), [leaving.md](leaving.md#4-forfeit-1111) |
| results (1.1.12) | no new member: steps 5, 6 and 8 tightened (an error never supports a result, a duel's record never explains the other player's error, a match is rated only when each team confirmed the record or a referee did), the table of what one participant can do alone | [above](#result-decision), [security-model.md](../security-model.md) |
| relays (1.1.13) | error `503 relay_capacity` with `retry_after_s`; engine errors `relay-full`, `relay-room-full`, `relay-source-limit`, `relay-source-full` | [relay.md](relay.md#43-relays-that-announce-their-load-1113) |

Additive members: match `series` and `game.keep_open` / `game.continue_from` / contract 2 `game`; match rules
`loadout`; `origin.kind = "activity"` with `activity_id`; match state `under_review`; result policy `decider` and
`review_deadline_s`; ladder `group` and `primary`; User `badge` and `title`; meta `operator_api`. Decision procedure:
result files of version 2 and check V10 ([match-contract-2.md](match-contract-2.md#7-reports-of-contract-2)).
