# Kalamar community protocol 1.1 — rules for extending

The protocol fixes two things only: the **match contract** (what the game can really do:
[match-contract.md](match-contract.md), [match-contract-2.md](match-contract-2.md)) and **transport and security**
(HTTPS with bearer tokens, the event WebSocket, the relay). Everything else — how players reach a match, what happens
after it, what the launcher shows — is the server's to invent, inside the rules below. They are normative.

## 1. Versions

- `MAJOR.MINOR`; this is **1.1**. 1.1 only adds to 1.0: every 1.0 request, response and event keeps its meaning, so a
  1.0 launcher works unchanged against a 1.1 server (it simply does not show the new areas).
- A server lists `"1.1"` (and `"1.0"`) in `protocol.versions`; it MUST NOT send a 1.1-only response shape to a
  request that does not need it.
- A launcher announces its version in `Kalamar-Protocol`; a server MAY use it to decide whether to create activity
  matches with contract version 2 for that player (a 1.0 launcher only ever declares contract 1).

## 2. Names

| Namespace | Owner |
|---|---|
| members, enum values, event types, error codes, component types, feature ids without prefix | this specification |
| `x-<anything>` members, `x-` enum values (for example a decider `x-elo-referee`), `/v1/x-<name>/...` paths, `x-` component types, `x-` feature ids | the operator; never standardized; receivers that do not know them ignore them |
| game identifiers (modes, stages, weapons, gear), engine rule keys, engine capability names | the game engine ([match-contract-2.md](match-contract-2.md)) |

A name that becomes standard in a later version never starts with `x-`; operators who used an `x-` name switch when
they wish.

## 3. What a receiver does with something unknown

| Unknown thing | Launcher | Server |
|---|---|---|
| object member | ignore it | ignore it |
| enum value | the documented fallback (usually: show the server's text, treat as `other`) | refuse with `422 validation_failed` only in requests where it matters, else ignore |
| feature id | hide nothing extra, show nothing new | — |
| event type | ignore it | — |
| component type | show its `fallback` text if any, else skip it; never fail the whole view | — |
| action style, icon, notice level | the default style / no icon / `info` | — |
| capability name (in `PUT /v1/me/game`) | — | ignore it |
| error code | handle by HTTP status | — |
| match state | treat as waiting (no launch) and refetch on `match.updated` | — |
| result-policy value | show the policy's `summary` | — |
| contract id or version | refuse the match with `launcher-incompatible` | assign only declared contracts |

**Never crash, never block the player**: a launcher that cannot render something shows what it can and keeps every
other screen working. A server that receives something it does not know never answers 5xx.

## 4. Conformance in 1.1

Conformance stays per area ([writing-your-own-server.md](writing-your-own-server.md#4-conformance-areas)). 1.1 adds
the areas `activities`, `series`, `badges`, `profiles` and the core scenario `core.capabilities` (a 1.1 body of
`PUT /v1/me/game` is accepted); 1.1.4 adds the area `population` (scenario `population.counts`; `core.errors` checks
that an undeclared population answers 404); 1.1.5 adds the core scenarios `core.oauth` (every `oauth_loopback`
provider), `core.password` and `core.password.lockout` (servers with password forms; the lock only on test
servers); 1.1.6 adds the area `draft` (scenarios `draft.declaration`, `draft.play`, `draft.timeout`,
`draft.no-capability`, `draft.leave`); 1.1.7 adds the area `spectators` (scenarios `spectators.declaration`,
`spectators.no-capability`, `spectators.room`, `spectators.late`); 1.1.8 adds the core scenarios
`core.match.counters` (one game's counters a little off) and `core.match.counters-gap` (far
off), decided by the declared policy's `stats` (absent: `strict`) and, when the server declares it, checked in
`outcome.stats_check`. A server is **1.1-conformant** at its level when the core and every area it declares pass; a 1.0-conformant
server stays 1.0-conformant.

## 5. What a server must never ask of a launcher

- run code, scripts, HTML, or load a URL without the player's click;
- open any network connection for the game but to the relay of a match (the game never resolves names: the launcher
  gives it an address);
- disable TLS or certificate checks;
- handle money: the protocol has no payment, no shop, no currency that can be bought;
- use game content: servers name game things only by game identifiers, which the launcher resolves locally.
