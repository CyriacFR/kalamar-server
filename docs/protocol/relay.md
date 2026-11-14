# Kalamar community protocol 1.1 — relay

Matches run over the game's own LAN mode, tunneled through a **UDP relay**. The relay's wire protocol ("relay
protocol, version 1") is specified in `engine/docs/lan-relay-protocol.md` in the
[Kalamar repository](https://github.com/CyriacFR/Kalamar); this repository contains a complete implementation, [`crates/kalamar-relay`](../../crates/kalamar-relay)
(`src/wire.rs` is the reference for the bytes on the wire), whose behavior follows that specification. This page
summarizes what a service operator and a launcher need, and how a service
allocates relay rooms.

## 1. The relay in one paragraph

One UDP endpoint (default port **47313**). A client (the game) sends `HELLO`, receives a stateless `CHALLENGE`
cookie, sends `JOIN` with a **room code** (1 to 32 characters from `A-Z 0-9 - _`) and a display name (at most 32
bytes), and gets a session id, a token and a virtual IPv4 address in `10.13.0.0/16`. From then on the game's LAN
datagrams travel in `DATA` messages, and the relay forwards them only between members of the same room (unicast to a
virtual address, or broadcast to the room). The relay tells each member the others' names (`MEMBER` messages); the
game uses them to put players in teams by name. Keepalives every 5 s, idle timeout 30 s. The relay never reads
payloads, never sends more to an unproven address than it received from it, and limits rates per address, member and
room.

## 2. What the relay does not do

- It has **no accounts**: anyone who knows a room code can join that room. An open relay (the default) has no link
  to the service at all; a relay bound to a service (section 4.2) only checks that a code was issued with its secret.
- It keeps nothing on disk and forgets a room when its last member leaves.
- It does not authenticate members beyond the per-session token (protection against off-path injection only).

## 3. Room allocation

Since the room code is the only admission control, the service makes it a secret of the match:

- A service allocates **one room per match** (contract 2: one per game session, whose rounds share it), with a code
  that cannot be guessed. For an open relay that is a code of at least 120 bits of entropy drawn from a
  cryptographically secure generator (the reference server: 26 characters of Crockford base 32, 130 bits, upper
  case). For a relay bound to the service it is a **sealed code** (section 4.2): 65 random bits and an 80-bit tag
  that only the holders of the relay's secret can compute; what it guarantees, and what it does not, is said there.
  In contract `lan-auto-match` version 1 the room code is also the game's match id (`game.match_id`).
- The room code is sent only to the participants of the match, only inside `game` of the match object, and only
  from state `ready` on (after every participant accepted). It is never shown in public views or histories, and
  never written to logs, by the service or by the relay (a relay that needs to follow a room through its log lines
  writes a value derived from the code that does not let anyone join it: the reference relay writes 8 hexadecimal
  digits of its SHA-256).
- A service never gives the code of a room to another match or session.

## 4. Choosing the relay of a match

A service names the relay of each match in `game.relay` (`host`, `port`, `room`). It MAY:

- run its own relay (the expected setup: [`deploy/`](../../deploy) runs `kalamar-relay` next to the service);
- name an external relay run by someone else (any implementation of relay protocol version 1);
- choose among several relays per match (for example by region).

The default relay announced in the discovery document and in `GET /v1/meta` is informational: launchers use the
relay of each match.

### 4.1 Several relays (1.1.9)

A server with several relays lists them in `meta.relays` (`id`, `host`, `port`, optional `region` and `label`). The
choice stays the server's, per match; what a launcher MAY add is a measurement:

- **Measuring.** For each relay of the list the launcher resolves the host itself (IPv4, as for a match), sends one
  relay protocol `HELLO` (64 bytes) from a fresh UDP socket and times the `CHALLENGE` that answers its nonce: that
  round trip, in milliseconds, is the relay's value; no answer within 2 seconds is `null`. A `HELLO` joins nothing
  and creates no state on the relay. A launcher measures at most once per relay per 10 minutes, never while the game
  runs, and only relays of the server the player is logged in to.
- **Declaring.** The values go in `launcher.relay_rtt_ms` of `PUT /v1/me/game`, every relay of the list at once.
- **Choosing.** How a server weighs them is its own business. A match keeps its relay for its whole life and a
  series for all its games (the game session of contract 2 lives in one relay room); a server never moves a match
  that was given its `game` block.

The reference server (`server.relay_selection`): `latency` (the default) gives a match the relay whose slowest player
is the fastest. Only the match's players count, never its spectators; a player's declaration counts only when it
names every relay that may be chosen and reached at least one of them (a relay it leaves out is unknown, not
unreachable, so a partial declaration says nothing); values are taken between 1 ms and 2000 ms (the time a launcher
waits), and `null` is a relay the player could not reach. Latencies within 25 ms are equal, and the relay with fewer
matches under way then wins, then the configuration's order (also the rule when nobody's declaration counts, and the
whole rule of `least_busy`); `first` always takes the first. With several relays it also sends each a `HELLO` every
`server.relay_probe_s` seconds: a relay that missed three in a row gets no new match while another one answers.

**Limit.** The measurements are the launchers' own: a participant can lie. Declaring a relay slow or unreachable
keeps the match off it (a player behind a firewall needs exactly that), so one player can push a match towards the
relay it prefers among those the others reached; what one player cannot do is put a match on a relay another player
declared unreachable while a relay every player reached exists. The cost of a lie is latency for the others, never a
match on a relay a declared player cannot reach.

### 4.2 Relays bound to one service

Relay protocol 1 has no accounts: an open relay serves whoever knows its address (section 2). A relay MAY hold a
secret that the service also holds, so that it opens only the rooms that service issued for it. Nothing changes for
the game or the launcher: the proof is the room code, which stays an opaque string of 32 characters from `A-Z 0-9`
(the game's limit is 32).

A **sealed room code** is Crockford base 32, upper case: 13 random characters `R` (65 bits), 3 characters `D` (the
issuing day: days since the Unix epoch modulo 32768, 15 bits, most significant first), and 16 characters `T`, the
first 80 bits of `HMAC-SHA-256(secret, "kalamar-room-v1" || R || D)` over the ASCII characters. A bound relay answers
a `JOIN` whose room is not a sealed code of its secret, or whose day is not today, yesterday or tomorrow (UTC), with
`JOIN_NAK` reason 3 (room), and creates nothing. Test vector: secret `0123456789abcdef0123456789abcdef` (those 32
ASCII bytes), `R = K7Q2MXA9P84DZ`, 2026-11-12T00:00:00Z gives `K7Q2MXA9P84DZM91NYAXD5NJAJ56RNH3`.

What a sealed code **guarantees**:

- It cannot be made or guessed without the secret: forging one means finding an 80-bit tag (and the 65 random bits
  keep two codes of one day apart). An open relay's 120-bit rule (section 3) is replaced by this.
- It is tied to the secret it was sealed with: a relay with another secret refuses it.
- It opens nothing once its window is over: it is accepted from 24 to 48 hours after it was issued (its day and the
  next, plus the day before for clocks behind).

What it does **not** guarantee:

- **Whoever holds a code can use it until its window ends.** Every participant of the match receives it (any player
  with an account on the server can be one), and can open or join that room again during those 24 to 48 hours, for
  example after the match ended. Section 3 (sent only to participants, never logged) is what keeps
  others from learning it.
- **A code seen on the network can be replayed** during the same window: it travels in clear, in the `JOIN`.
- **Whoever holds the secret can make codes** for every relay that holds the same secret. When several relays share
  one secret, the person running any of them can open rooms on all of them. A service SHOULD give each relay its own
  secret (one `secret_file` per `[[relays]]` entry), so that a relay's operator can make codes only for that relay.

A shorter window (an hour rather than a day) was considered and not taken: the rounds of a contract 2 game session
keep one room for as long as the session lasts, and after a relay restart every member joins its room again, so a
window shorter than an evening would refuse legitimate joins.

The reference programs: `kalamar-relay --room-secret-file <file>` (the file: hexadecimal, at least 32 digits;
`--new-room-secret <file>` writes a new one, readable by its owner only on Unix), `secret_file` (or `secret_env`) on
the service's `[[relays]]` entry, and `[embedded_relay]`, which runs the relay on a thread of the service and creates
its secret at the first start; the `[[relays]]` entry that names it takes that secret, and the service refuses to
start (`check-config` too) when a bound embedded relay is announced without it. The service reads every relay secret
at start and at each reload and keeps it in memory; the rounds of one game session keep their sealed room while it is
accepted.

### 4.3 Relays that announce their load (1.1.13)

A relay bound to a service (section 4.2) MAY tell that service how loaded it is, so that the service sends new
matches where there is room. Nothing changes for the game or the launcher. The exchange is the relay's own protocol on
the relay's own UDP port; no other port is opened:

- **The key** is `HMAC-SHA-256(secret, "kalamar-relay-status-v1")`, from the relay's room secret (never the secret
  itself). Only the holders of a relay's secret (the relay and its service) can ask it or read its answer as genuine.
- **The question**, `STATUS_QUERY` (type `0x20`), 128 bytes: the common header, a random nonce (u64), the service's
  clock in Unix milliseconds (u64), zero bytes up to offset 112, and a tag, the first 16 bytes of
  `HMAC-SHA-256(key, bytes 0..112)`.
- **The answer**, `STATUS` (type `0x21`), 128 bytes, to the question's source: the common header, the question's
  nonce, the relay's clock in Unix milliseconds, then format 1: workers (u8), the period of the rates in seconds (u8),
  the workers' mean load in percent (u8), sessions, rooms, the caps of sessions and rooms (0: no cap), datagrams
  received and sent per second, bytes received and sent per second, datagrams dropped per second, JOINs refused during
  the period and those of them refused for want of room (u32 each, saturating), the uptime in seconds (u32), the
  members per room (u8, then 3 zero bytes), each worker's load in percent (32 bytes, `0xFF` past the last worker; more
  than 32 workers: every byte is the mean), and the tag of bytes 0..112 under the key. The reference relay's
  `crates/kalamar-relay/src/status.rs` holds the layout and its tests.
- **What the relay answers.** Only a question whose tag verifies, whose clock is within 60 seconds of the relay's, and
  whose nonce it has not answered before; at most 20 a second, counted against the source's HELLO rate. Anything else
  gets nothing at all. An answer is never larger than its question, so a recorded question replayed from a forged
  source gets nothing and nothing is ever amplified. **An open relay answers nothing**: to it these are datagrams of
  an unknown type; so does a relay that does not know the query.
- **What it reveals**: counts and rates, never an address, a name or a room. It travels in clear (someone on the path
  sees the traffic itself anyway). Neither `GET /v1/meta` nor `GET /v1/population` carries it: the load is for the
  operator.

**Choosing with the load (reference server, `[relay_load]`).** The service asks every bound relay every `status_s`
seconds (5; 0: never). A relay's **load** is the largest of: its sessions against its cap, its
rooms against its cap (counting, when larger, the members and matches the service itself sent there and that have not
joined yet, and the match being placed), and its workers' mean load. A relay is **full** at `full_at_percent` (90)
or when the match would not fit under its caps, and stays full until it is back at `resume_at_percent` (75) or under;
it is **silent** when it answered before and missed its last three questions. A new match goes, among the relays the
reachability check of section 4.1 does not hold for down:

1. to a relay with room, or one that announces nothing (open, not knowing the query, `status_s = 0`); when there is none, to a silent
   one; when there is none either, **nowhere**: a room start gets `503 relay_capacity`
   (`retry_after_s` 30) and changes nothing, a queue forms no match and its tickets keep waiting, a tournament's
   next series starts once a relay has room, and two players paired by a duel activity wait first in line;
2. with `latency`, among those whose slowest counted player is within `latency_tolerance_ms` (30) of the best one;
3. to the one with the most room left, in steps of 10 % (a relay that announces nothing counts as half full), then
   the fewer matches under way, then the configuration's order (`first`: the first relay with room).

When no candidate announces its load, steps 2 and 3 are the rule of section 4.1. A match keeps its relay for its whole
life and a series all its games: a relay that fills up is never asked to give a match back. `kalamar-service admin
relays` shows each relay as the running service sees it: reachable or not, bound or open, and the last load it
announced. The same choices apply to the relay this service runs (`[embedded_relay]`), asked on this machine.

**Limit.** The load is the relay's own word, sent to its own service: a relay can lie about itself, and the service
believes it (it runs the relay, or trusts who does, section 4.2). A full relay still opens the rooms of matches
already given to it: the caps and the per-source limits (section 6) are what a relay enforces itself.

## 5. Network requirements

The relay needs inbound **UDP** on its port (47313 by default) reachable from every player. It cannot sit behind an
HTTP reverse proxy: give it a public address and an open UDP port, or a UDP-capable load balancer that keeps the
source address and port of each client stable for the session. Players need outbound UDP to it. The relay works over
IPv4 (the game is IPv4-only); it may also listen on IPv6 for other clients.

## 6. Running a relay without a service

The relay is useful alone: players can share a room code by hand (a "relay-only" server, see
[server-card.md](server-card.md#1-the-one-line-form)). Matches arranged that way have no accounts, no result checking
and no ratings. The reference relay runs open only when told so (`kalamar-relay --open`): without `--room-secret-file`
or `--open` it refuses to start, so that a relay meant for a service is never left open by mistake.

**Per-source limits (reference relay).** A source is an IPv4 address or an IPv6 /64 (loopback is never limited): it
holds at most 24 sessions of the relay (`--max-sessions-per-source`) and 8 members of one room
(`--max-members-per-source`), refused with the codes clients already know (`JOIN_NAK` 5 `RELAY_FULL`, 4 `ROOM_FULL`).
1.1.13: such a refusal carries detail 1 in byte 17 of the `JOIN_NAK` (reserved, zero, before), which a 1.1.13 engine
shows its player ("too many players from the same Internet connection"); a full room or relay carries 0. Clients that
do not read the byte see the codes as before.
One source cannot fill the relay, nor take all 12 places of a room: 4 stay for the others, so a duel or a
2 v 2 always has room; in a larger match a participant who fills 8 places can still keep the others out, which voids
only a match that participant could void anyway. Players behind one address (a household, a LAN party, a carrier's
shared address) share these limits: 8 members of a room is a whole 4 v 4 behind one box.

**Capacity (reference relay, 1.1.13).** The relay handles datagrams on several threads (`--workers`, 0 = one per
processor: on Linux each with its own socket on the same port, `SO_REUSEPORT`, the kernel keeping each player on one
of them; elsewhere sharing one socket). Its caps of sessions and rooms (`--max-sessions`, `--max-rooms`) are 1024
sessions per worker and a quarter as many rooms by default; **0 is no cap**: the per-source limits and the HELLO and
JOIN rates then still bound what one address holds. What a machine carries is measured in the operator guide
([operator-first-steps.md](../operator-first-steps.md#capacity-of-a-relay)).
