# Kalamar server

The **open protocol** and a **reference server** for Kalamar communities: accounts, friends, parties, private rooms,
queues (any team format the game build supports), result cross-checking, ratings, leaderboards, statistics,
moderation. Without a server the Kalamar launcher is an offline tool; a player who adds a server unlocks what that
server declares. Anyone can run this server, configure an entirely different offering, or **write their own server
from the documentation alone**.

> [!IMPORTANT]
> **This software contains no game content and is not affiliated with, endorsed by or connected to the publisher of
> any game.** It is a community service for a fan launcher that starts matches of the supported game build from each
> player's own copy; it never touches game files, and the game never talks to it (the launcher does). **Operators are
> responsible for their community, their moderation and their legal compliance** (privacy law, terms of their hosting,
> local law). No money: the protocol has no payment, shop or currency.

## Status

The protocol is at version 1.1, revision 1.1.13, the first public revision (`docs/protocol/overview.md` lists what
each revision added), which the reference server implements. The programs are at version 0.1.0: a program version
names a build of this repository, a protocol revision names what launchers and servers agree on. What has been
checked, and what has not:

- `cargo test --workspace` runs the unit and HTTP tests and the **conformance suite** against three server offerings,
  with simulated launchers and a simulated game.
- With the real game and the Kalamar launcher: matches between **two players** (private rooms, a ranked queue, a
  best-of-3 series, a player leaving or a game killed, a win by forfeit). **Not verified**: more than two players in
  one match, many players on one server, long-running production servers.
- Logins through OAuth providers (Discord, Google, GitHub, Twitch, OpenID Connect) and e-mail through SMTP were tested
  against local stand-ins only, **never against the real services**.
- The installer is tested in dry-run mode for every role, mode and preset (`deploy/test-install.sh`).

## Security: the risks that remain

What the service and the relay defend against is in [docs/security-model.md](docs/security-model.md). What no
server can fix, and what operators and players should know:

- **The game's network code is not hardened.** Matches are played by the original game's network code, recompiled:
  not memory-safe, not sandboxed. What the other members of a room send reaches it through the relay unfiltered (the
  relay checks sizes, sessions and addresses only). Players should play with people and servers they trust.
- **The operator is trusted widely.** Whoever runs a server chooses each match's relay, sees players' addresses (in
  the proxy's and the relay's logs), names, room codes and results, and can show any text and links in launchers.
- **Results are declared by the players' launchers.** The service only cross-checks them: one player alone cannot
  change a rating against players who report, but anyone can void a match, and collusion or a modified game
  goes undetected.
- **The relay protocol is plaintext.** A room code is the only key of a room: someone on the network path who sees it
  can join that room while the code is valid (24 to 48 hours for a relay bound by a secret).

How to report a vulnerability: [SECURITY.md](SECURITY.md).

## Documentation

The index is [docs/README.md](docs/README.md): the specification, running a server step by step
([docs/operator-first-steps.md](docs/operator-first-steps.md)), writing your own, and the architecture of this
repository in ten minutes ([docs/architecture.md](docs/architecture.md)).

## Repository

| Path | What |
|---|---|
| [`docs/protocol/`](docs/protocol/overview.md) | **The specification, version 1.1** (1.1.13; 1.1 only adds to 1.0): overview (actors, trust, privacy, versioning, errors), server card and discovery, HTTPS API + [OpenAPI](docs/protocol/openapi.yaml), event WebSocket, match contracts 1 and 2, relay, activities, series, the stage draft, spectators, a player who leaves and the forfeit, presentation, operator integrations, rules for extending, writing your own server |
| `crates/kalamar-protocol` | the normative pieces as a library: match contract helpers, result validation, the result decision procedure |
| `crates/kalamar-service` | the reference service (HTTP API, events, matchmaking, decisions, ratings, admin command line) |
| `crates/kalamar-relay` | the UDP relay matches run through (relay protocol v1), standalone binary |
| `crates/kalamar-testkit` | headless launchers, a game simulator, and the **conformance suite** for any server |
| [`deploy/`](deploy) | the installer (`install.sh`), example configurations, systemd units, Caddy and nginx examples, Dockerfile |
| [`docs/operator-first-steps.md`](docs/operator-first-steps.md) | walk-through from an empty machine to a server launchers can add: names, relay (UDP), HTTPS proxy, tests |
| [`docs/LEGAL-NOTES.md`](docs/LEGAL-NOTES.md) | factual notes on content, data, licenses (not legal advice) |

## Run it locally (5 minutes)

Requirements: Rust 1.97 or newer.

```sh
cargo build --release --locked
./target/release/kalamar-relay --open --bind 127.0.0.1                           # UDP 127.0.0.1:47313
./target/release/kalamar-service --config deploy/kalamar.local.toml serve       # http://127.0.0.1:8080
```

In another terminal, play a simulated match (four simulated launchers in a private room, one of them cheating):

```sh
./target/release/kalamar-testkit simulate --api http://127.0.0.1:8080 --players 4 --cheat 1
```

`deploy/kalamar.local.toml` enables the `dev` login (anyone can log in as any username): **never expose it**. A
launcher points at the server with the line `127.0.0.1:8080` only if it allows plain HTTP on loopback (the protocol
allows `http://127.0.0.1` for testing).

**The conformance suite** needs a server whose clock it can move: set `test_clock = true` under `[features]` of that
file and raise its `[rate_limits]` as CI does (the `conformance` job of `.github/workflows/ci.yml`, or
`crates/kalamar-testkit/tests/reference.rs`), then:

```sh
./target/release/kalamar-testkit conformance --api http://127.0.0.1:8080 --max-wait 5
```

Without the test clock the suite skips the scenarios that must wait and the match scenarios after them fail with
`in_match`: the summary then says `NOT CONFORMANT`, even for the reference server.

## Install it with the installer

```sh
./deploy/install.sh          # questions in English or French (--lang), or every answer as an option (--help)
```

- It asks **what** to install (a whole server: the HTTP service and its relay; or one more relay for a server you
  already run), **how** (Docker with `docker compose` and optionally Caddy for HTTPS; systemd services built with
  cargo; or only the files), **which preset** (`friends`: the relay inside the service; `competitive`, `tournament`:
  the relay as a program of its own), the server's name, its public host name and your contact.
- `--dry-run` shows everything it would do and writes nothing. Nothing is installed before you confirm the summary;
  an existing binary, unit or Caddyfile is copied aside (`.bak-<date>`) before it is replaced, and a running service
  is restarted only after you confirm. It changes no firewall.
- At the end it prints, and keeps in `INSTALL-INFO.txt`, what players type, which ports to open, every place it
  wrote, how to change the configuration without a restart, the day-to-day commands, how to add a relay, and how to
  uninstall.
- What it downloads: in Docker mode, Docker pulls the base images (`rust`, `debian`, `caddy`, by tag: pin them by
  digest if you need a reproducible build) and cargo the crates of `Cargo.lock`; in systemd mode, cargo downloads the
  crates; in files mode, nothing.
- Its tests: `deploy/test-install.sh` (runs it in dry-run mode for every role, mode and preset, checks that nothing
  is written and that bad answers are refused; `shellcheck` too when installed).

## Operating a server

A step-by-step walk-through is in [docs/operator-first-steps.md](docs/operator-first-steps.md).

1. **Configure.** Start from a preset, `kalamar-service -c /etc/kalamar/kalamar.toml init --preset competitive --name
   "My community" --host play.example.org --operator Camille --contact mailto:admin@example.org` (`friends`: rooms
   between friends; `competitive`: ranked queues and ladders; `tournament`: a cup and best-of series; `full`: the
   reference file; `init --list`), or copy [`deploy/kalamar.example.toml`](deploy/kalamar.example.toml) to
   `/etc/kalamar/kalamar.toml` and change every `CHANGE` value. `kalamar-service --config ... check-config` validates
   it. Everything the launchers show comes from it: areas (`[features]`), login providers, the room form (`[rooms]`),
   the queues (`[[queues]]`: team sizes, rated or not, party sizes, mode/stage pools and rotations, selection,
   opening windows, entry requirements, smaller minimum teams for small communities), result policies, penalties,
   ladders, rating parameters, rules text, report categories.
2. **HTTPS.** The service speaks plain HTTP on `127.0.0.1:8080`; put Caddy ([`deploy/Caddyfile`](deploy/Caddyfile))
   or nginx ([`deploy/nginx.conf`](deploy/nginx.conf)) in front. Set `trusted_proxies` to the proxy's address: only
   then is `X-Forwarded-For` believed (its right-most untrusted entry), so clients cannot spoof their address.
3. **Relay.** Matches need UDP: run `kalamar-relay` on a public UDP port (47313 by default) and name it in
   `[[relays]]`; it cannot go through an HTTP proxy. A machine with 1 vCPU and 1 GB runs both programs.
   - **Bound to your server**: a relay started with `--room-secret-file` opens only room codes your service sealed
     with the same secret, so strangers cannot use it
     ([relay.md](docs/protocol/relay.md#42-relays-bound-to-one-service)).
   - **Several relays** (other regions, or a spare): one `[[relays]]` entry with an `id` each; a match goes where
     its slowest player has the best latency ([relay.md](docs/protocol/relay.md#41-several-relays-119)).
   - **One program for a few friends**: the preset `friends` runs the relay inside the service
     ([operator-first-steps.md](docs/operator-first-steps.md#one-program-or-two)).
   - **Players behind one address** (a LAN party) share per-address limits, 8 members of a room by default
     ([operator-first-steps.md](docs/operator-first-steps.md#4-run-both-programs)).
4. **Services.** [`deploy/systemd/`](deploy/systemd) has hardened units for both programs; or use the
   [`Dockerfile`](deploy/Dockerfile).
5. **Privacy.** Adapt [`PRIVACY-TEMPLATE.md`](PRIVACY-TEMPLATE.md), publish it, and set `privacy_url`. Players have
   export and erasure in the protocol, and the Kalamar launcher offers both in a server's Account tab; you can also
   perform them from the command line.
6. **Tell players** the host name (`play.example.org`): the launcher fetches
   `https://play.example.org/.well-known/kalamar-server.json` and shows your server's name, operator and features.

**Changing the configuration while it runs.** Edit the file, then `kalamar-service -c ... admin reload`: an invalid
file changes nothing, and the command prints the errors or what was applied. Sessions, rooms and matches go on, and
launchers redraw. A few settings wait for a restart (the listening address, `production`, the database and secret
files, the logins, `[rate_limits]`, `[[webhooks]]`, `[embedded_relay]`): the command names them
([operator-first-steps.md](docs/operator-first-steps.md#3-configure-the-service)).

**The relay inside the service** (`[embedded_relay]`) runs on a thread of its own, with its own limits (`max_rooms`,
`max_sessions`) and a line of counters in the log every `stats_interval_s`; a stop (Ctrl-C, SIGTERM) closes every
member's session as `kalamar-relay` does. Measurements:
[docs/operator-first-steps.md](docs/operator-first-steps.md#one-program-or-two). It is meant for a few friends; a
larger server runs `kalamar-relay` apart, on the same machine or another.

**Admin command line** (works while the service runs; bans and erasures apply within 30 s):

```sh
kalamar-service -c /etc/kalamar/kalamar.toml admin reload     # apply the edited configuration, no restart
kalamar-service -c /etc/kalamar/kalamar.toml admin users --search cam
kalamar-service -c ... admin ban u_3k9w2m7x1q --for 7d --reason "cheating"     # or --until 2026-12-01T00:00:00Z, or permanent
kalamar-service -c ... admin unban u_3k9w2m7x1q
kalamar-service -c ... admin reports                 # open moderation reports;  resolve-report <id> --note "..."
kalamar-service -c ... admin match m_9x2k...          # a match with its reports and decision
kalamar-service -c ... admin relays                  # each relay as the service sees it: reachable, load, room left
kalamar-service -c ... admin season-roll --name "Season 2"
kalamar-service -c ... admin export u_3k9w2m7x1q --out export.json
kalamar-service -c ... admin erase u_3k9w2m7x1q --yes
kalamar-service -c ... admin user-create --name tester   # prints tokens (test accounts, conformance --tokens)
kalamar-service -c ... admin password-reset u_3k9w2m7x1q # a one-time reset code for an e-mail/password account
kalamar-service -c ... admin operator-token --name bot --scopes read,results   # a token for your bot or stats site
```

**Backups:** the SQLite database (`database`), `secret_file` (without it, logins cannot find their accounts) and the
relay secrets (`relay.key` files: without them, the relays must be given new ones).
**Logs:** the service writes one structured line per request with the route template, status and duration — no ids,
codes, tokens or addresses (`KALAMAR_LOG`, `--log-format json`). The relay (alone or inside the service) writes a
line per member who joins or leaves: the member's name, address and port, and a tag of the room (8 hexadecimal digits
of a hash, never the room code); say so in your privacy notice (PRIVACY-TEMPLATE.md does).

### Updating and going back

Migrations run when the service starts (`kalamar-service migrate` runs them alone, the service stopped). **Back up
the database before starting a new version.** The admin commands never migrate: run against a database an older
service still serves, they stop and say so (stop that service, start the new one, then run them). A database
migrated by a newer version is refused, with its schema number and this program's.

Step by step, with the rollback: [docs/upgrading.md](docs/upgrading.md).

### Login methods

Every method is configuration only, and every one is off until you enable it (the example enables `guest`). The
trade-offs, step by step, are in [docs/operator-first-steps.md](docs/operator-first-steps.md#choosing-login-methods).

| Method | Configuration | Players need | You store |
|---|---|---|---|
| Just a name | `[auth.guest]` | nothing | nothing but the account (lost if the launcher's login is lost) |
| Any OAuth / OpenID Connect provider | `[[auth.oauth]]` (presets `discord`, `google`, `github`, `twitch`, `oidc` from an `issuer`, or every endpoint by hand), `[auth.discord]` | an account there | a keyed hash of the provider's user id |
| E-mail and password | `[auth.password]`, optionally `[auth.password.smtp]` | an address and a password | a keyed hash of the address, an Argon2id hash of the password |

OAuth: register an application with the provider, add the loopback redirect URIs (`http://127.0.0.1:53682/callback`,
`...53683...`), keep the client secret in a file only the service reads (`client_secret_file`) or an environment
variable (`client_secret_env`; Discord's section also reads `KALAMAR_DISCORD_CLIENT_SECRET`). The launcher creates a
PKCE pair and listens on loopback; the service checks the verifier itself, exchanges the code with its secret (over
HTTPS, no redirect followed, bounded answers) and reads the user id; nothing of the provider is stored but its keyed
hash. Every provider flow is tested against a stand-in provider on 127.0.0.1 only, **never against Discord, Google,
GitHub, Twitch or a real OpenID provider** (that needs real application credentials).

E-mail and password: four forms the current launcher already draws (log in, create an account, forgotten or new
password, change password). Without SMTP, a forgotten password needs a code from you:
`kalamar-service -c ... admin password-reset <user>` (one hour, single use; give it privately). With
`[auth.password.smtp]` (TLS required), codes go by e-mail and sign-ups confirm the address. The protocol forbids
requiring an e-mail address to play, so `check-config` refuses a server whose only login is the password one. Read
[LEGAL-NOTES.md](docs/LEGAL-NOTES.md#3-logins-and-what-they-imply) before turning it on.

### What the reference server offers

Ratings (Glicko-2), activities and best-of series, stage drafts, spectators, operator integrations, and what the
result cross-check guarantees: [docs/reference-server.md](docs/reference-server.md).

## Contributing

Contributions are welcome as pull requests: read [CONTRIBUTING.md](CONTRIBUTING.md) first. Bugs and ideas go to the
issue forms ([SUPPORT.md](SUPPORT.md)); security problems are reported privately ([SECURITY.md](SECURITY.md)). The
checks every pull request passes:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace        # unit and HTTP tests, and the conformance suite against three server offerings
```

The full list, with the repository checks, is in [CONTRIBUTING.md](CONTRIBUTING.md).

## Dependencies

All from crates.io, pinned by `Cargo.lock`.

| Crate | Why |
|---|---|
| `tokio` | async runtime (network, timers) |
| `axum` (+ its `hyper`, `tower`, `tokio-tungstenite`) | HTTP server, routing, WebSocket; `hyper`, `hyper-util` and `tower` directly too, for the HTTP server's header timeout and connection cap |
| `futures-util` | splitting WebSocket streams |
| `serde`, `serde_json`, `toml` | JSON API and TOML configuration |
| `rusqlite` (bundled SQLite) | storage with one file and no server; bundled so every platform builds the same |
| `tracing`, `tracing-subscriber` | structured logs |
| `clap` | command lines |
| `sha2`, `hmac`, `subtle`, `getrandom`, `base64` | token hashing, keyed identity hashes, constant-time comparison, secure randomness, PKCE encoding |
| `reqwest`, `rustls` (ring provider), `rustls-platform-verifier` | the OAuth provider calls (service) and the HTTPS client (testkit), TLS without OpenSSL |
| `argon2` (RustCrypto) | password hashes (Argon2id), only used with `[auth.password]` |
| `lettre` (rustls, no OpenSSL) | the optional SMTP client of `[auth.password.smtp]` |
| `tokio-rustls`, `tokio-tungstenite` | the testkit's `wss://` client |
| `rand` (relay only) | relay tokens and virtual addresses |
| `socket2` (relay only; already `tokio`'s) | the relay's sockets: one per worker on the same port on Linux (`SO_REUSEPORT`) |

## Sources and credits

**Origin.** All code here is original to the project and MIT-licensed; this repository is not a fork. The relay
speaks the engine's relay protocol, specified in `engine/docs/lan-relay-protocol.md` in the
[Kalamar repository](https://github.com/CyriacFR/Kalamar)
(`crates/kalamar-relay/src/wire.rs` is the reference in this repository); where that specification was ambiguous:
[crates/kalamar-relay/SPEC-NOTES.md](crates/kalamar-relay/SPEC-NOTES.md).

**Dependencies.** Every crate with its license, generated from the lock file and checked by CI:
[docs/third-party-dependencies.md](docs/third-party-dependencies.md). SQLite is bundled through `rusqlite`. The
installer's Docker mode pulls the `rust`, `debian` and `caddy` base images; the proxy examples are for Caddy and nginx.

**Methods and specifications the work relies on:** Glicko-2 (Glickman, "Example of the Glicko-2 system", 2012) for
the reference ratings (`crates/kalamar-service/src/rating.rs`, which tests the paper's example); the OWASP Password
Storage Cheat Sheet for the Argon2id parameters; OAuth 2.0 with PKCE (RFC 7636), loopback redirects for native apps
(RFC 8252) and OpenID Connect discovery for logins; RFC 3339 timestamps, the WebSocket protocol (RFC 6455) and the
requirement keywords of RFC 2119 in the specification; the Developer Certificate of Origin 1.1
([CONTRIBUTING.md](CONTRIBUTING.md#sign-off)).

**No game content.** No code, file or data of any game or of its publisher is included: game identifiers travel as
opaque strings that the players' own copies provide ([docs/LEGAL-NOTES.md](docs/LEGAL-NOTES.md)). The project is not
affiliated with, endorsed by or connected to the publisher of any game, and does not describe or connect to its online
services. Trademarks belong to their owners.

## License

MIT ([LICENSE](LICENSE)); the relay crate carries the same license
([crates/kalamar-relay/LICENSE](crates/kalamar-relay/LICENSE)). Every dependency and its license:
[docs/third-party-dependencies.md](docs/third-party-dependencies.md).
