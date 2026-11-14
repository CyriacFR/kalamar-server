# Security model of the reference server and relay

What the reference service (`kalamar-service`) and relay (`kalamar-relay`) defend against, what they do not, and what
an operator should know. It follows a security review of the whole project; each finding that concerns the server or
the relay is listed in [section 4](#4-findings-of-the-security-review) with what was done about it. The protocol's
own rules are in [protocol/](protocol/overview.md). How to report a vulnerability, and the operator's hardening
checklist: [SECURITY.md](../SECURITY.md). Upgrading a server: [upgrading.md](upgrading.md).

## 1. Who is trusted

| Party | What the server takes from it |
|---|---|
| The operator (and whoever runs the machine) | everything: it chooses the relay of each match, sees addresses, names, room codes and results, decides disputes |
| A player's launcher and game | claims only: its reports, statuses and game declaration are compared with the others', never believed alone |
| A referee (an account listed in `[spectators] referees`, or seated by the operator API) | its witness report confirms a result for every team (`protocol/spectators.md`) |
| A spectator who is not a referee | at most a tie-break where the operator's policy asks for one; never a confirmation: it may be a player's second account |

A cheater is assumed to control its own launcher completely (any report, any status, at any time), to have several
accounts (guest accounts cost one request where they are enabled), and to sit in the stands with one of them where
the places allow it.

## 2. Results

The decision procedure is `protocol/api.md`, "Result decision". Two rules carry most of the weight:

- an error report never supports a result; it voids the match unless the record explains it (a failure in the match,
  of a player the record shows gone, recorded by that player's own team);
- a match is rated only when each team confirmed the record with a valid result of its own, or a referee's record
  equals it.

The table of what one participant can do alone is in `protocol/api.md` (after step 8). In short, a cheater alone:

- **cannot** change a rating against players who report, nor win a counted result against a player whose game
  reported a failure;
- **can** void any match it plays in (an error report: nobody can tell a failure on purpose from a real one), and so
  avoid a rating change;
- **can** get a counted, unrated result against a player who played and then sent nothing (`silence:
  consent_after_play`);
- **can**, where the operator chose it, win a counted result on one report: `quorum: any` (the casual policy of the
  example configuration), `witness_from: any` with places in the stands open to anyone or to friends (its second
  account decides the tie), `rated_requires_both_teams: false` (then also rated). A counted result also counts in a
  series and a tournament. `check-config` warns about these combinations on rated queues and activities.

Players who collude (a majority of a match reporting the same false record, or two players trading wins) are out of
reach of any cross-check; a modified game lies to every member alike. Moderation (reports, bans) is the answer.

## 3. Residual risks

Risks the reference server and relay accept, and why. An operator who publishes a server should know them.

1. **Self-reported results.** See section 2. A player can always void a match (and avoid a rating change) by
   reporting an error or by leaving its game in a way that looks like a crash; the forfeit covers only a
   departure the leaver's own launcher reports. In a tournament a void series is drawn by lot, so a player who voids
   every game still has an even chance: use `decider = "auto_then_referee"` where that matters.
2. **The game is not memory-safe and not sandboxed.** The original game's network code, recompiled, receives what the
   other members of a room send through the relay, unfiltered. The relay only checks sizes, sessions and addresses; it
   cannot protect a player from another player of the same room. Players should play with people and servers they
   trust.
3. **The operator is trusted.** It chooses the relay of each match, sees players' addresses (in its proxy's and
   relay's logs), display names, room codes and results, and can show any text and links. A launcher decides what it
   lets a server start on its player's machine.
4. **The relay protocol is plaintext.** A room code is the only access control of a room: someone on the network path
   who sees it can join the room for as long as the code is valid (24 to 48 hours for a sealed code). Off-path
   injection is blocked (64-bit nonce, 96-bit session and token).
5. **Shared addresses.** The per-address limits count an IPv4 address or an IPv6 /64 (configurable): players behind
   one address (a household, a carrier's shared address) share them: the relay's 24 sessions and 8 members of a room
   per source (a whole 4 v 4 behind one box), 20 new accounts a day, the login and request rates. Raise them for a
   larger LAN party behind one address (`--max-members-per-source`, `[embedded_relay] max_members_per_source`,
   `[rate_limits]`; [operator-first-steps.md](operator-first-steps.md#4-run-both-programs)).
6. **Logs.** The relay logs, for each join, leave and refusal, the member's name, its IP address and port and a tag of
   the room (never the code); the proxy logs addresses as configured. The service logs no address, token, password or
   report body. Say it in the privacy notice (`PRIVACY-TEMPLATE.md`).
7. **What an erased account leaves.** A keyed hash of its login identity while a ban, a cooldown or the penalty window
   runs, and with its open reports for 30 days at most (section 4, M6); its lines in other players' matches stay,
   anonymized; the audit log keeps its id for `retention.audit_days`.
8. **Run behind the shipped proxy.** The service cuts slow clients and caps connections itself, but TLS, request
   logging policy and anything beyond HTTP/1.1 limits are the proxy's (`deploy/Caddyfile`, `deploy/nginx.conf`).
   `server.trusted_proxies` must list only the proxy.
9. **Accepted findings** (no change, for the reasons given):
   - the relay's HELLO and JOIN limiter shares 4096 slots between sources: a flood of spoofed sources (about 82 000
     datagrams a second) delays legitimate HELLOs; it never amplifies (a reply is never larger than its request, 0.5
     of the bytes);
   - any member of a room may invite (where `rooms.invites` allows it) and an invitation skips the room's password:
     inviting is a member's right in the reference rooms; an operator who wants otherwise limits invitations to
     friends (`[rooms] invites_friends_only`) or turns them off (`invites = false`);
   - a refresh token stolen and used within 60 s of its rotation still gives a new pair (the reuse window that lets a
     launcher retry after a network failure); a use after the window ends the session;
   - the choice among several relays follows the latencies the players' launchers declare: a lying participant can
     cost the others latency, never a relay another player declared unreachable while one every player reached
     exists (`protocol/relay.md`, section 4.1);
   - without SMTP, password sign-up says whether an address is registered (documented in `operator-first-steps.md`);
   - display names may hold Unicode format characters (bidirectional controls): the server refuses control characters
     only; launchers isolate server text when they show it;
   - with two reports, the record kept is the host's when it agrees: a host may inflate its own counters, which decide
     nothing, are flagged beyond the tolerance, and no badge reads them;
   - the existing mode of a secret file is not checked (the service creates its own 0600; a file the operator made is
     the operator's);
   - the Docker install trusts `X-Forwarded-For` from the private ranges: its port 8080 is reachable only from the host
     and the compose network.
10. **A relay on several threads, and its load.** The relay's protections are the same with any number of
    workers: the limits (sessions, rooms, members per room, per source) and the last place of a room are decided for
    one JOIN at a time under one admission lock, races included (`crates/kalamar-relay/tests/concurrency.rs`); a reply
    is never larger than its request. The load a bound relay announces goes only to whoever holds its secret (a
    question keyed by it, within 60 s, never twice, at most 20 a second, an answer no larger than the question); an
    open relay answers nothing more than before. The load is the relay's own word (the service believes the relays it
    is bound to), and it travels in clear: counts and rates, no address, name or room. On Windows two datagrams of one
    member a few microseconds apart may leave in the other order (UDP allows it).

## 4. Findings of the security review

Server and relay findings only (the launcher's and the engine's are handled in the Kalamar repository). The ids (H1,
M3, L-S4 ...) are the review's; code comments and tests cite them as "security review M5". "Fixed": the attack the
review proved is a regression test (`crates/kalamar-service/tests/security.rs`, `crates/kalamar-relay/tests/`) that
failed before the fix.

| Finding | Status | What changed |
|---|---|---|
| H1: a forged record wins a rated match when the opponent's launcher honestly reports an error | fixed | errors never support a result; a duel's record never explains the other player's error; rating needs each team's own confirmation (`protocol/api.md`, steps 5, 6, 8) |
| M3: a cheater's second account in the stands "witnesses" a forged result (`witness_from = "any"`) | fixed | such a witness never rates a result (step 8); the presets `competitive` and `tournament` count referees only; `check-config` warns about a rated queue or a tournament whose policy takes any spectator as a witness while its places are open to non-referees |
| M7: guest login on in every preset, competitive and tournament included | fixed | `[auth.guest] ranked = false` keeps guest accounts out of rated queues (requirement `account`) and tournaments; set in the presets `competitive` and `tournament`; `check-config` warns while guests may play ranked (the default is `ranked = true`) |
| M8: in a tournament, the first-listed player of a pairing advances by voiding every game | fixed | a void series is drawn by lot (`activities.rs`): nothing in the reports tells who caused a void, so no side may profit from one. A player who voids every game still has an even chance against an opponent who wanted to play: a tournament that needs better uses `decider = "auto_then_referee"` (a void goes to a referee) |
| M6: erasing an account and signing up again clears cooldowns, offenses and reports | fixed | at the erasure, a running cooldown, the offenses of the penalty window and the open reports stay with the login identity's keyed hash (`identity_holds`, migration `0008_security`) for as long as they run, the reports for `retention.erased_hold_days` (30) at most; a new account with the same login takes them over. Reports about an erased account are kept anonymized (`ON DELETE SET NULL`) until resolved. Guest accounts hold nothing (they can never log in again). `PRIVACY-TEMPLATE.md`, section 5, says it |
| M1 (server part): the launcher's `build` identifies the installation | fixed | the reference server no longer stores `build` (migration `0008_security` removes the stored values); `protocol/api.md` says what a launcher may send |
| M5: per-address limits keyed by the full address (one IPv6 /64 is 2^64 clients); `max_users` exhausted by one client | fixed | every per-address limit counts the client's network: IPv4 /32 and IPv6 /64 by default (`rate_limits.ipv4_prefix`, `ipv6_prefix`; 24 for a whole IPv4 /24); new accounts are limited per network (`rate_limits.accounts_per_ip_per_day`, 20; a guest login is a new account); a guest account whose last session ended (it can never be logged in to again) is erased; one network holds at most 16 pending OAuth logins |
| L-S4: only the first `X-Forwarded-For` header line was read | fixed | every line counts, in order; the right-most address that is not a trusted proxy is the client. The Docker install's trust of the private ranges is kept (accepted): its port 8080 is published on 127.0.0.1 only, or not at all behind its Caddy, so only the host and the compose network reach it |
| M4: one address fills a bound relay (1024 sessions) with codes any two accounts mint; a participant closes their own room to the others | fixed for the relay, reduced for rooms | the relay limits each source (an IPv4 address, an IPv6 /64; loopback exempt) to 24 sessions and to 8 members of one room (`--max-sessions-per-source`, `--max-members-per-source`), refused with the codes clients know (`RELAY_FULL`, `ROOM_FULL`). The default is 8 so that a whole 4 v 4 or a LAN party behind one box plays with no setting. With 12 members per room, one source can no longer fill a room: 4 places stay for the others, so a duel or a 2 v 2 always has room; in a larger match a participant who fills 8 places can still keep the others out (it could void that match anyway). The service limits room matches started per player (`rate_limits.room_starts_per_hour`, 30). Idle sessions still end after 30 s, and their places return. Not done: sealing the expected member count in the code (a protocol change for little: a participant can always void their own match) |
| L-S5: the shipped relay unit and Docker files ran an open relay | fixed | `kalamar-relay` refuses to start without `--room-secret-file` unless `--open` says the relay is meant to be open; the unit reads its secret through `LoadCredential` and gains `ProtectKernel*`, `ProtectClock`, `LockPersonality`, `SystemCallArchitectures=native`, an empty capability set and `UMask=0077`; the Docker example passes the secret |
| L-S6: the database created with the default umask; the relay and the service under one account | fixed | a new database file is created with mode 0600 (SQLite gives its journal the same mode; an existing file keeps its own); `deploy/install.sh --mode systemd` runs the relay as an account of its own (`kalamar-relay`) whose group is the service's, so it reads the relay secret (mode 0640) and nothing else of `/var/lib/kalamar` (mode 0750). Secret files are created 0600 as before; an existing secret file's mode is not checked (accepted: the operator's own files) |
| L-S1: event WebSocket frames bypassed every rate limit (one connection sent 44 772 `ack` a second, each taking the service's lock) | fixed | a connection's own frames have a budget (20 a second, 100 at once); beyond it the connection is closed (1008), and a launcher reconnects and resumes |
| L-S2: no header or body read timeout, no connection cap (slowloris without the proxy) | fixed | the service runs its own HTTP/1.1 accept loop (hyper) with a header read timeout (`limits.header_timeout_s`, 10), a body read timeout (30 s, `408`) and at most `limits.max_connections` (4096) open HTTP connections; WebSocket event streams have their own limits (`events.max_connections_per_user`) |
| L-S3: a report's text (a `mode` of up to 64 KiB with line breaks) went into the opponent-visible `invalid_reasons` and the text log | fixed | a report's text is quoted at most 32 characters, with control and bidirectional characters replaced (`decision.rs`, `shown`) |
| Webhook whose `secret_file` cannot be read, silently dropped (information) | fixed | `check-config` and the service's start read every webhook secret file; a webhook left out is logged |

The same review's findings about the launcher and the engine are fixed or accepted in the Kalamar repository
(`launcher/docs/security-model.md` there).
