# Architecture in 10 minutes

A map for a new contributor: what each crate does, where a request goes, how a match lives, and where to start
reading. Words in *italics* are in the [glossary](glossary.md).

## The whole picture

Kalamar has four pieces; this repository holds two of them and the specification that ties them together.

```
 the player's PC                                      a community server (this repository)
+------------------------------------+   HTTPS + WS   +-------------------------------------+
| launcher (Kalamar repository)      | <------------> | kalamar-service                     |
|   starts the game for one match,   |                |   accounts, rooms, queues, results  |
|   reads its status and result      |                +-------------------------------------+
|   files, uploads them              |                                 | sealed room codes
| game (the player's own build)      |   UDP          +-------------------------------------+
|   plays over its LAN mode          | <------------> | kalamar-relay (or the embedded one) |
+------------------------------------+                +-------------------------------------+
```

The service never talks to a game, and a game never talks to the service: the launcher is in between. The game's
traffic goes only to the *relay* the match names. The launcher and the game live in the Kalamar repository (engine
and launcher); their side of the contract is specified here, in `docs/protocol/`.

## The crates

| Crate | What | Start at |
|---|---|---|
| `kalamar-protocol` | the normative pieces as pure functions, no I/O: match contract helpers, result validation, the result decision procedure | `src/lib.rs`, `src/decision.rs` |
| `kalamar-service` | the reference service: HTTPS JSON API, the event WebSocket, matchmaking, decisions, ratings, moderation, the admin command line | `src/lib.rs`, `src/app.rs` |
| `kalamar-relay` | the UDP relay: stateless handshake, rooms, rate limits, one thread per processor, the load it tells its service; also runs inside the service for small servers | `src/relay.rs`, `src/wire.rs`, `src/server.rs` |
| `kalamar-testkit` | simulated launchers and a simulated game (it writes status and result files as the game does), and the *conformance suite* | `src/conformance.rs`, `src/sim.rs` |

## A request's path through the service

1. `app.rs` builds the router and the middleware: the client's address (believed from a proxy only when it is in
   `trusted_proxies`), rate limits, body limits, the protocol header, and a request log without personal data.
2. The handler of the *area* (`api/rooms.rs`, `api/queues.rs`, `api/ratings.rs`...) checks the input strictly:
   malformed input is a 4xx with the protocol's error shape, never a 5xx.
3. It takes the one lock of the shared state (`state.rs`): the configuration, the SQLite database (`db.rs`) and the
   live state (parties, rooms, tickets, event streams, which are in memory only). It works synchronously, queues the
   events it produces, and releases the lock; nothing awaits while holding it.
4. The queued events go out on the launchers' WebSockets (`events.rs`), numbered so that a launcher can resume.

Everything an operator can change lives in the TOML configuration (`config.rs`, presets in `presets/`), and
`admin reload` applies a new file without a restart (`reload.rs`).

## A match's life

1. **Formed**: from a room (`api/rooms.rs`) or by matchmaking from queue tickets (`matchmaking.rs`), possibly after a
   *stage draft* (`draft.rs`).
2. **Assigned**: `matches.rs` builds the *match object*: the contract version every player's engine supports, the
   relay (`relays.rs`: among those close to the fastest for the slowest player, the one with the most room left; none
   when every relay is full, `relay_capacity`), a fresh *room code* (sealed when the relay is bound to this service),
   each player's *member name*, the rules string.
3. **Played**: each launcher reports the launch, mirrors the game's status changes, then uploads the game's result
   file (or an error report).
4. **Decided**: the reports are validated and cross-checked under the match's *result policy*
   (`kalamar-protocol/src/decision.rs`); a match without a valid result that a player left while it was being played
   can be lost by that player's team when the policy says so (`leave`, protocol 1.1.11, `policy.rs`); the decision
   goes to the players, ratings change (`rating.rs`), series and activities move on (`series.rs`, `activities.rs`),
   webhooks fire (`webhooks.rs`).

What the cross-checks can and cannot catch is written in `docs/protocol/overview.md`, section 3.2.

## Tests

- Unit tests next to the code; HTTP tests in `crates/kalamar-service/tests/` against a service on 127.0.0.1 with a
  manual clock (no sleeping, no real network).
- The conformance suite (`kalamar-testkit conformance`) plays whole scenarios through the public API, for the areas a
  server declares. `cargo test` runs it against the reference service in three offerings
  (`crates/kalamar-testkit/tests/reference.rs`); anyone can run it against their own server.
- `deploy/test-install.sh` runs the installer in dry-run mode.

## Where to start

| You want to... | Start at |
|---|---|
| understand the protocol | [protocol/overview.md](protocol/overview.md), then [protocol/api.md](protocol/api.md) |
| add an endpoint | `crates/kalamar-service/src/api/`, `docs/protocol/api.md` and `openapi.yaml`, a conformance scenario |
| change how results are decided | `crates/kalamar-protocol/src/decision.rs` and `docs/protocol/api.md`, "Result decision" |
| change matchmaking | `crates/kalamar-service/src/matchmaking.rs` |
| change the relay | `crates/kalamar-relay/` and the engine's relay protocol (the Kalamar repository, `engine/docs/lan-relay-protocol.md`) |
| change the installer | `deploy/install.sh` and `deploy/test-install.sh` |
