# kalamar-relay

A small UDP relay for the LAN mode of the supported game build's runtime (relay protocol version 1:
`engine/docs/lan-relay-protocol.md` in the Kalamar repository; `src/wire.rs` is the reference in this repository).
Game instances on different networks send their LAN datagrams to the relay; it forwards them only between members of
the same room. One static binary, no accounts, no database, no persistence.

## Build

    cargo build --release -p kalamar-relay   # target/release/kalamar-relay(.exe) at the workspace root
    cargo test -p kalamar-relay              # unit, state-machine, fuzz-style and loopback UDP tests

Rust stable (the workspace asks for 1.97; the relay alone builds with 1.80 or newer). Builds on Windows and Linux;
the only difference is the sockets of the workers (below).

    cargo run --release -p kalamar-relay --example relay_bench -- --relay 127.0.0.1:47399 --rooms 25,50,100

measures a relay under the traffic of real matches (`examples/relay_bench.rs` says how; the figures are in the
server's operator guide, "Capacity of a relay").

## Run

    kalamar-relay --room-secret-file relay.key      # 0.0.0.0:47313, default limits, bound to its service
    kalamar-relay --open --bind 127.0.0.1           # an open relay, for local tests only
    kalamar-relay --open --max-members 8 --max-rooms 20 --log-level debug
    kalamar-relay --room-secret-file relay.key --max-members-per-source 12  # a LAN party with spectators, one box
    kalamar-relay --room-secret-file relay.key --workers 1                  # one thread, every datagram in order

Without `--room-secret-file` the relay refuses to start unless `--open` says it is meant to be open to anyone.

Every flag but `--new-room-secret` also exists as an environment variable `KALAMAR_RELAY_<NAME>` (for example
`KALAMAR_RELAY_PORT`, `KALAMAR_RELAY_MAX_MEMBERS`, `KALAMAR_RELAY_LOG_LEVEL`); `kalamar-relay --help` lists them all.
Stop it with Ctrl-C (or SIGTERM on Linux): it tells every member the relay is shutting down.

Players set, in the game's environment: `KLM_LAN=relay:<host>:47313` and `KLM_LAN_ROOM=<room code>`
(upper-case letters, digits, `-` and `_`, 1 to 32 characters). Use a **long random room code**: it is the only access
control, anyone who knows it can join that room.

## Which port to open

**UDP 47313** (or the port you chose), inbound, to the machine running the relay. Nothing else; TCP is not used.

- *Home PC:* allow the program in the Windows firewall for the networks you use (private network), and forward UDP
  47313 on your router to the PC. Players use your public address (or a dynamic DNS name). The relay never opens
  ports on its own.
- *Small VPS (Linux):* open UDP 47313 in the provider's firewall and, if used, `ufw allow 47313/udp`. How many players
  a machine carries, and why the network is usually the limit before the processor: the server's operator guide,
  "Capacity of a relay". Example systemd unit:

      [Unit]
      Description=Kalamar relay
      After=network-online.target

      [Service]
      LoadCredential=relay.key:/etc/kalamar/relay.key
      ExecStart=/usr/local/bin/kalamar-relay --port 47313 --room-secret-file ${CREDENTIALS_DIRECTORY}/relay.key
      DynamicUser=yes
      Restart=on-failure
      NoNewPrivileges=yes
      ProtectSystem=strict
      ProtectHome=yes

      [Install]
      WantedBy=multi-user.target

If you restart the relay, players reconnect by themselves within a few seconds and keep their virtual IP address.

## Defaults

| Setting | Default | Flag |
|---|---|---|
| threads handling datagrams | one per processor (`0`; `1`: one thread) | `--workers` |
| members per room | 12 (2 to 254) | `--max-members` |
| rooms | a quarter of the sessions (256 with one worker); `0`: no cap | `--max-rooms` |
| sessions (all rooms) | 1024 per worker; `0`: no cap | `--max-sessions` |
| receive buffer of each socket | 256 KiB (`0`: the system's) | `--recv-buffer-kb` |
| sessions of one source (an IPv4 address, an IPv6 /64; loopback unlimited) | 24 | `--max-sessions-per-source` |
| members of one room from one source | 8 | `--max-members-per-source` |
| HELLO / JOIN per source IP | 20 per second each | `--hello-rate`, `--join-rate` |
| DATA per member | 2000 datagrams and 4 MiB per second | `--member-pps`, `--member-bytes` |
| DATA per room | 12 000 datagrams and 16 MiB per second | `--room-pps`, `--room-bytes` |
| keepalive / idle timeout | 5 s / 30 s | `--keepalive`, `--idle-timeout` |
| cookie secret rotation | 1 hour | `--cookie-rotation` |
| statistics line | every 30 s | `--stats-interval` |

**Several players behind one address.** The per-source limits count an IPv4 address (or an IPv6 /64), so players who
share one public address (the same home box, a LAN party, a school or a carrier's shared address) share them. The
defaults let a whole 4 v 4 (8 members of one room) and 24 players in all play from behind one box with no setting; the
ninth member of one room from the same address is refused with `ROOM_FULL` and detail 1 (its game asks again every 10 s,
then stops: a game of protocol 1.1.13 with `relay-source-limit`, which its launcher shows as "too many players from the
same Internet connection in this match"; an older one with `network-unavailable`). For more from one address (a LAN
party with spectators, several matches at once), raise `--max-members-per-source` (12, the room's size, or 0 for no
limit; `KALAMAR_RELAY_MAX_MEMBERS_PER_SOURCE`) and `--max-sessions-per-source`. The relay inside the service takes the
same limits in `[embedded_relay]` (`max_members_per_source`, `max_sessions_per_source`).

What the limits still guarantee: one address can never hold more than 24 sessions of the relay (so it cannot fill a
relay bound to its service with codes any two accounts can mint), nor more than 8 of the 12 places of a room. A duel
or a 2 v 2 therefore always has room whatever one participant does; in a larger match, a participant who fills 8
places from one address can still keep the others out, which only voids a match that participant could void anyway
(with an error report). A higher limit lets one address hold more of the relay.

## Several processors

`--workers N` threads handle the datagrams (0, the default: one per processor). On Linux each has a socket of its own on
the same port (`SO_REUSEPORT`): the kernel hands each player's datagrams to one of them, always the same while the
player's address and port stay the same, so a member's datagrams leave in the order they came. On Windows they share one
socket and take its datagrams in turn: two datagrams of one member a few microseconds apart may then leave in the other
order (UDP allows it; the game's transport handles it; the load bench saw none). What threads share is per room: a
datagram takes the lock of its own room only; joins, leaves and timeouts take one admission lock, so that the limits and
the last place of a room are decided for one JOIN at a time. Nothing changes on the wire.

**0 is no cap.** `--max-sessions 0` and `--max-rooms 0` remove the caps: the machine is then the limit, and what one
address can hold stays bounded by the per-source limits and the HELLO and JOIN rates. The default caps (1024 sessions
per worker, a quarter as many rooms) are a safety limit: measured, one worker relays about 50 000 datagrams a
second, some 1 700 duels or 60 matches of 4 v 4 (480 sessions); what keeps a service from overfilling a relay is the
load the relay reports to it. A relay whose network is the limit wants a lower `--max-sessions` (100 Mbit/s carries
about 60 matches of 4 v 4). The figures, and how they were measured: the server's operator guide, "Capacity of a relay".

**The load, for the service.** A relay bound to a service (`--room-secret-file`) answers that service's status
question (protocol 1.1.13, `relay.md` section 4.3): sessions, rooms, caps, rates and each worker's load, on its UDP
port, keyed by its secret, never larger than the question. An open relay answers nothing more than before.

## Logs

Structured `tracing` lines on standard output: one per join, leave, timeout, address change and refusal, and one
statistics line (datagrams received, sent, forwarded, dropped per reason, sessions, rooms, then the last seconds'
rates and each worker's share of time spent working). `--log-level` or
`KALAMAR_RELAY_LOG_LEVEL` takes `error`, `warn`, `info`, `debug`, `trace` or a tracing filter. **Payloads and room
codes are never logged**: a join or leave line carries a tag of the room (8 hexadecimal digits of its SHA-256, enough
to follow one room through the lines), the member's display name and virtual address, and its IP address and port
(what an operator needs against abuse: say so in your privacy notice).

## Bound to one service

`--room-secret-file relay.key` binds the relay to the service that holds the same secret: it opens only room codes
sealed with it (`JOIN_NAK` reason 3 for any other), so strangers who know its address cannot open rooms on it.
`--new-room-secret relay.key` writes a new secret (the file must not exist; on Unix it is created with mode 0600).
Whoever holds a sealed code (every player of its match) can still use that room for 24 to 48 hours, and whoever holds
the secret can make codes: give each relay its own (the protocol's `relay.md`, section 4.2).

## What the relay can and cannot see

It forwards opaque game datagrams. It sees, while running: the IP addresses and ports of the players, the room codes,
the display names, the virtual addresses, and the size and timing of datagrams. The game datagrams travel in clear
between a player and the relay; the relay adds no encryption, and neither reads nor logs payloads. Rooms are
isolated: nothing crosses from one room to another. The relay keeps everything in memory only: no accounts, nothing
written to disk, everything forgotten when it stops. It does not defend against a network attacker who can read the
traffic on the path (section 7.4 of the relay protocol's specification).

Abuse resistance: the handshake is stateless until the client proves its address with a cookie (HMAC, rotated secret);
the relay never sends to an address that has not completed it, never answers with more bytes than it received before
that, and rate limits sources, members and rooms.

## Notes

`SPEC-NOTES.md` lists where the specification and the engine's relay disagree or are ambiguous, and what was chosen.
`scripts/interop.ps1` runs the real client (the game runtime's `--probe-lan-peer` mode, `-Exe <runtime.exe>`) against
the relay on 127.0.0.1.

License: MIT.
