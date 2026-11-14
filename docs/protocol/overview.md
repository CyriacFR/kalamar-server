# Kalamar community protocol, version 1.1 — overview

This directory is the open specification of the protocol between a **launcher** (the program a player uses to start
matches of the supported game build) and a **community service** (a server run by anyone, for any community). A
launcher or a server written from these documents interoperates with the reference implementation in this
repository; the one part specified elsewhere is the relay's wire format ([relay.md](relay.md)). The words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are used as in RFC 2119.

| Document | Contents |
|---|---|
| [overview.md](overview.md) (this file) | actors, trust model, privacy, areas, versioning, extensibility, error model |
| [server-card.md](server-card.md) | how a player adds a server: the server card, the one-line form, the discovery document |
| [api.md](api.md) | every HTTPS endpoint, object and error code |
| [openapi.yaml](openapi.yaml) | the same HTTPS API, machine-readable (OpenAPI 3.1) |
| [events.md](events.md) | the WebSocket of server-to-launcher pushes: heartbeat, resume, event types |
| [match-contract.md](match-contract.md) | how a launcher turns a match object into the game's start settings and what it reports back |
| [relay.md](relay.md) | the UDP relay the matches run through, and how a service allocates relay rooms |
| [writing-your-own-server.md](writing-your-own-server.md) | the minimal subset, worked servers, conformance levels, the conformance suite |
| [extending.md](extending.md) (1.1) | **rules for extending**: versions, the `x-` namespace, what to do with anything unknown, what a server may never ask |
| [activities.md](activities.md) (1.1) | generic activities (tournaments, leagues, events...) as declarative views of data components |
| [series.md](series.md) (1.1) | series of matches (best of N, first to N, rematches) |
| [match-contract-2.md](match-contract-2.md) (1.1) | everything the game can express, capability negotiation, launcher-applied loadouts, rounds in one session |
| [presentation.md](presentation.md) (1.1) | badges, titles, server-designed profiles, any number of ratings |
| [operators.md](operators.md) (1.1) | webhooks, the bot and administration API, referee result policies |
| [draft.md](draft.md) (1.1) | the stage (and mode) draft, "pick and ban", between "match found" and the launch |
| [spectators.md](spectators.md) (1.1) | places in the stands: spectators of a match, never players; their reports as witnesses |
| [leaving.md](leaving.md) (1.1) | a player who leaves a match in play, and the forfeit |
| [for-launcher-implementers-1.1.md](for-launcher-implementers-1.1.md) | what a 1.0 launcher adds for 1.1 |

The protocol describes this project's community service only. It contains no game content, no description of any online
service of the game's publisher, and no list of the game's modes or stages: those come from the player's own copy of
the game ([match-contract.md](match-contract.md), "Mode and stage identifiers").

## 1. Actors

```
            HTTPS + WebSocket                       UDP (relay protocol v1)
 launcher  <------------------->  community   relay  <------------------->  game (started by the launcher)
 (menu)                          service    (rooms)
    |  starts the game with KLM_* settings, reads its status and result files
    +-------------------------------------------------------------------------> game
```

- **Launcher.** The player's menu: accounts, friends, parties, private rooms, queues, results. It is a data-driven
  client: it shows only what a server declares (areas, queues, room options, ratings, rules) and renders server data
  without interpreting it. It starts the game for one match with the settings a match object dictates, mirrors the
  game's status file to the service and uploads the game's result file.
- **Community service** ("the service", "the server"). An HTTPS/JSON API plus one WebSocket per launcher. It owns
  accounts, social data, rooms, queues, matchmaking, the rules of every match, the decision on every result, ratings
  and moderation. It decides everything about what it offers; this specification only fixes how that offer is
  described and used.
- **Relay.** A UDP datagram forwarder ([relay.md](relay.md)). It has no accounts and no database: it forwards opaque
  datagrams between members of the same room. The service names the relay and the room of each match; the relay knows
  nothing of the service.
- **Game.** The supported game build, started by the launcher for exactly one match. It plays the match over its own
  LAN mode tunneled through the relay and writes a status file (rewritten at every change of state) and a result file
  (the match's record, or an error). It never talks to the service.

Without a server the launcher is an offline tool. A player who adds a server ([server-card.md](server-card.md)) unlocks
the areas that server declares.

## 2. Areas and the minimal core

The protocol is split into **areas**. A server implements the **core** and any subset of the optional areas, and
declares the optional ones in `features` of `GET /v1/meta` ([api.md](api.md#meta)). A launcher MUST hide every area
that is not declared and MUST NOT call its endpoints; a server answers calls to an undeclared area with `404
feature_disabled`.

| Area (`features` id) | What it covers | Depends on |
|---|---|---|
| core (always) | discovery, meta, rules, at least one login provider, the account (display name, settings, export, erasure), the event WebSocket, the match object and its lifecycle (launch, status mirror, result intake) | — |
| `rooms` | private rooms: options declared by the server, codes, passwords, invitations, teams, ready, start | core |
| `queues` | server-defined queues, tickets, match found, accept / decline, penalties | core |
| `parties` | parties (a group that queues together) | core; `queues` to be useful |
| `friends` | friend codes, requests, friend list | core |
| `presence` | presence of friends (`online`, `in_queue`, `in_match`, `offline`) | `friends` |
| `ratings` | ladders, seasons, per-player ratings shown as opaque presentable data | core |
| `leaderboards` | ordered leaderboard rows with a server-declared column schema | `ratings` |
| `stats` | per-player statistics and match history | core |
| `moderation` | reporting a player, blocking a player; bans are visible as account restrictions | core |
| `activities` (1.1) | server-invented activities shown as views and actions ([activities.md](activities.md)) | core |
| `series` (1.1) | series of matches ([series.md](series.md)) | core |
| `badges` (1.1) | badges and titles of the server's own design ([presentation.md](presentation.md)) | core |
| `profiles` (1.1) | server-designed player profiles ([presentation.md](presentation.md)) | core |
| `population` (1.1.4) | public counts of players online, in a match and searching, without identities ([api.md](api.md#population)); undeclared, its endpoint answers `404 not_found` | core |
| `draft` (1.1.6) | a server-run draft of stages and modes (bans, picks, draws, by captain, vote or anyone) before a match is ready ([draft.md](draft.md)) | core; `rooms`, `queues` or `series` to be useful |
| `spectators` (1.1.7) | places in the stands of a match: who may watch, the list of watchable matches, spectators' reports as witnesses ([spectators.md](spectators.md)) | core; `rooms`, `queues` or `series` to be useful |

A server with only private rooms, a server with only a 1v1 ranked ladder, and a server with everything are all
conformant, each at its level ([writing-your-own-server.md](writing-your-own-server.md)). Nothing in this
specification assumes that a particular team format, queue, mode or stage exists.

## 3. Trust model

### 3.1 What the service decides

The service is authoritative for: who is who (accounts), who may play together (rooms, queues, parties, bans), the
**complete rules of every match** (teams, team of each player, mode, stage or "the game's choice", options), which
player hosts the game session, the relay and room of the match, and the **outcome** of the match it accepts (and every
rating change that follows). A conformant launcher applies the rules of a match exactly as given or refuses to launch
([match-contract.md](match-contract.md#refusing-a-match)); it never substitutes its own.

### 3.2 What the service can know — and what it cannot

**Results are client-reported.** The game computes the match's record on each player's machine; the launcher uploads
that record. The service never sees the match itself. It sees, per participant: whether the launcher said it
launched, the game's status file as the launcher mirrors it, and the result file.

The supported game build gives every player's record of one match the same **outcome** (rule, mode, stage, winner,
every player's name, team and disconnection); its **counters** (the teams' scores and each player's statistics) are each game's own and may
differ slightly between machines. A service therefore cross-checks the outcome of the reports of one match, the counters with a tolerance
([api.md](api.md#result-decision)), and applies a **result policy** it declares in advance
([api.md](api.md#result-policies)).

What cross-checking **does** guarantee:

- a single player who edits the outcome in their own result file, or uploads someone else's, is detected when at least
  one other participant reports honestly (the reports disagree, or the uploaded file does not belong to its uploader);
  an edit of the counters alone changes no outcome; beyond a wide tolerance it is flagged (policy `stats: flag`),
  and with three reports or more the edited one is never the record kept;
- a result that does not match the rules the service set (wrong players, wrong teams, wrong mode) is detected and not
  counted;
- a player cannot change the outcome by not reporting, when the server's policy treats silence after the match has
  started as consent (the reference policy does).

What it **does not** guarantee:

- several players who **collude** (all participants, or a majority under a majority policy) can report any outcome
  they agree on;
- a **modified game** that lies to its peers during the match (for example sends false statistics to the others) changes
  every participant's record the same way: the reports agree and the lie is not detected;
- the service cannot tell a crash from a deliberate disconnect, nor a slow network from a player who left on purpose;
  it only sees what the game recorded (`disconnected` flags) and what launchers reported;
- clocks: `end_utc` and `t0_unix` in a result come from each player's machine and are informative only;
- the **counters** of a duel: with two reports and no witness, nothing tells the honest counters from slightly
  edited ones (within the tolerance); the host's report is the record, so its counters are the statistics recorded.

A server SHOULD say this plainly to its players (its rules page). Moderation (reports, review of disputed matches,
bans) is the remedy for what cross-checking cannot catch.

### 3.3 Credentials and transport

- The API is HTTPS only (TLS terminated by the server or by a reverse proxy in front of it). A launcher MUST refuse a
  non-`https` API URL, except `http://127.0.0.1`, `http://[::1]` and `http://localhost` for local testing.
- Authentication uses opaque bearer tokens. A server MUST store only a cryptographic hash of every token (access,
  refresh, WebSocket ticket) so that a database leak does not leak usable credentials.
- The relay carries game datagrams as they are: it adds no encryption and does not read the payload. The relay room of a match is
  unguessable and is given only to the participants of that match ([relay.md](relay.md)).
- Third-party login (for example an OAuth2 provider) uses the authorization-code flow with PKCE and a loopback
  redirect to the launcher ([api.md](api.md#oauth-loopback-providers)); the launcher never sees the provider's tokens
  and the server never sees the player's provider password.
- 1.1.5: a server's own passwords ([api.md](api.md#password-accounts)) travel only inside the HTTPS API, are
  stored only as a salted memory-hard hash, and a wrong password answers exactly like an unknown address.

## 4. Privacy

The protocol is designed for data minimization. A server SHOULD store only:

| Data | Why | Notes |
|---|---|---|
| account id, display name, creation time, settings | to be someone on the server | the display name is chosen by the player |
| a keyed hash of the login identity (provider + provider's user id) | to find the account at the next login | the reference server never stores a provider's user id, e-mail or avatar in clear |
| hashes of tokens, session creation and last-use times | to authenticate | |
| friend code, friendships, friend requests, blocks | the `friends` and `moderation` areas | |
| matches: rules, participants, reports, decision | to decide results, show history, compute ratings | raw result files MAY be purged after a retention period; the decision and the shared record are kept |
| ratings, statistics | the `ratings` and `stats` areas | |
| penalties, reports filed, bans | moderation | |
| 1.1.5, password accounts: a salted memory-hard hash of the password, the address (a keyed hash suffices), hashed one-time codes | to log in, to reset a forgotten password | the reference server stores the address only as a keyed hash and never sends mail the player did not just ask for |

A server MUST NOT require an e-mail address, a real name or a phone number to play (a server offering password
accounts also offers a login without an address), MUST NOT log tokens, passwords, one-time codes, OAuth codes or
result payloads, and SHOULD NOT log IP addresses beyond what abuse prevention needs (rate limiting can work in memory
only). Presence is shown only to friends and only as `online`, `in_queue`, `in_match` or `offline`
(never which room, queue or match), and a player can hide it.

Every server MUST offer, in the core: **export** (`GET /v1/me/export`: everything the server holds about the
player, as JSON) and **erasure** (`DELETE /v1/me`: the account and the data attached to it are erased; the
player's participation in other players' matches is anonymized). A banned player keeps both rights. The operator
publishes a privacy notice ([PRIVACY-TEMPLATE.md](../../PRIVACY-TEMPLATE.md) is a template) and links it from the
discovery document.

## 5. Versioning and compatibility

- The protocol has a version `MAJOR.MINOR`; this document is **1.1** (1.1 only adds to 1.0; the rules for
  extending are in [extending.md](extending.md)). The API's paths carry the major version
  (`/v1/...`). A server lists the versions it speaks in `protocol.versions` of the discovery document and of `GET
  /v1/meta` (for example `["1.0", "1.1"]`).
- A **revision** (`1.1.13`) names a state of these documents within a minor version. It is never negotiated: a
  revision only adds optional members, open enum values and capabilities, which each side discovers from the other's
  declarations ([section 8](#8-changes)). "1.1.N:" in these pages marks what revision N added.
- **Minor** versions only add: new endpoints, new optional request fields, new response fields, new enum values,
  new event types, new areas. A 1.x client works with any 1.y server.
- A **major** version changes or removes something and gets a new path prefix (`/v2`); a server MAY serve several
  majors side by side.
- **Negotiation.** A launcher sends `Kalamar-Protocol: <version>` (its highest supported 1.x version) on every
  request; a server answers with `Kalamar-Protocol: <version>` (the highest version both support). A launcher that
  finds no common major in `protocol.versions` MUST tell the player that the server needs a newer (or older) launcher
  and MUST NOT call anything but the discovery document. A server that receives a request for a major it does not
  serve answers `404`; one that receives `Kalamar-Protocol: 2.x` on a `/v1` path ignores the header.

### 5.1 Extensibility rules (both sides)

1. **Unknown fields are ignored.** A receiver MUST ignore object members it does not know, in requests and in
   responses, in events and in the documents of [server-card.md](server-card.md).
2. **Enums are open.** A receiver MUST handle an unknown enum value gracefully: the documents name the fallback for
   each enum (usually "show the server's text and treat as `other`"). A launcher MUST NOT crash or refuse a whole
   response because of one unknown value.
3. **Extension members** are keys starting with `x-` (for example `x-myserver-tournament`). Any object MAY carry
   them; they are the operator's own data, never defined by this specification. A launcher MAY show them only when
   they are inside a declared presentable structure (for example a leaderboard column).
4. **Server-declared presentation.** Everything a launcher shows that is the server's business (queue names, room
   options, rating tiers, leaderboard columns, rules, result policies) is sent as data with localizable text
   ([api.md](api.md#localized-text)). A launcher renders it; it does not hard-code queues, formats, modes, ratings or
   policies.
5. **New event types** are ignored by launchers that do not know them ([events.md](events.md)).
6. A server MAY add endpoints under `/v1/x-<name>/...` for its own clients; launchers ignore them.

## 6. Error model

Every error is an HTTP status plus a JSON body:

```json
{"error": {"code": "room_full", "message": "the room is full", "field": null, "retry_after_s": null}}
```

- `code` is stable and machine-readable (the list is in [api.md](api.md#error-codes)); a client branches on `code`,
  never on `message`. Unknown codes are handled by their HTTP status class.
- `message` is an English sentence for logs and developers; a launcher SHOULD show its own translated text for known
  codes and MAY show `message` for unknown ones. 1.1.5: `text` (optional) is the same for the player as localized
  text; a launcher without its own text for the code SHOULD show `text` rather than `message`.
- `field` names the offending request member for `validation_failed` (a JSON pointer such as `/options/password`).
- `retry_after_s` is set with `429 rate_limited`, `403 queue_cooldown`, `409 name_change_too_soon`, `403 banned`
  (when temporary) and 1.1.13 `503 relay_capacity`; the
  `Retry-After` header carries the same number for 429.
- A server MUST NOT answer 500 for malformed input: malformed JSON is `400 invalid_json`, a well-formed body with bad
  values is `422 validation_failed`, an oversize body is `413 payload_too_large`.

## 7. Conventions (summary; details in [api.md](api.md#conventions))

- JSON in UTF-8; times are RFC 3339 strings in UTC with a `Z` suffix (`2026-11-11T20:15:03Z`, fractional seconds
  allowed); durations are numbers of seconds with a `_s` suffix in their name.
- Identifiers are opaque strings of 1 to 64 characters from `A-Z a-z 0-9 _ -`; clients never parse them.
- Lists are paginated by an opaque `cursor`.
- Localizable text is an object mapping language tags to strings.

## 8. Changes

Revision 1.1.13 is the first public revision. Each revision only adds to the one before; the page named in a row
specifies what it added.

| Revision | Added |
|---|---|
| 1.1.13 | Relays that announce their load, error `503 relay_capacity`, the relay's refusal reasons shown to the player ([relay.md](relay.md#43-relays-that-announce-their-load-1113)). |
| 1.1.12 | Stricter result decision: an error report never supports a result, and a rating needs each team's own confirmation ([api.md](api.md#result-decision)); what a launcher may send about its installation. |
| 1.1.11 | The forfeit: a player who leaves a match in play can lose it, by policy ([leaving.md](leaving.md#4-forfeit-1111)). |
| 1.1.10 | A participant whose final report is in is free of a match that waits for the others ([leaving.md](leaving.md)). |
| 1.1.9 | Several relays, relays bound to one service by a secret, and offers that change while the server runs ([relay.md](relay.md#41-several-relays-119)). |
| 1.1.8 | Reports agree on what decides the match; counters are compared with a tolerance (policy member `stats`), and check V11 ([api.md](api.md#result-decision)). |
| 1.1.7 | Area `spectators`: places in the stands, witness reports, referees ([spectators.md](spectators.md)). |
| 1.1.6 | Area `draft`: the stage and mode draft between "match found" and the launch ([draft.md](draft.md)). |
| 1.1.5 | Password accounts as ordinary `direct` forms, the one-time code step, a localized `text` on errors ([api.md](api.md#password-accounts)). |
| 1.1.4 | Area `population`: `GET /v1/population` counts players without identities. |
| 1.1.3 | Loadouts imposed by a match, for rooms, queues, series and activities ([match-contract-2.md](match-contract-2.md)). |
| 1.1.2 | Frame counts and durations are no longer compared; the decision says why reports were refused (`invalid_reasons`). |
| 1.1.1 | Clarifications of series, rounds and stage choice ([series.md](series.md), [match-contract-2.md](match-contract-2.md)). |
| 1.1 | Activities and views, series, match contract 2 with capability negotiation, badges, titles and profiles, operator webhooks, the operator API and referee policies, rules for extending. |
| 1.0 | The core: accounts, friends, parties, rooms, queues, matches, result cross-checking, ratings, moderation. |
