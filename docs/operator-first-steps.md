# Operator: first steps

From an empty Linux machine to a community server that Kalamar launchers can add by name. About an hour the first
time. The README's "Operating a server" section is the reference; this page is the walk-through.

**What you will run**: `kalamar-service` (the HTTPS API, behind a reverse proxy) and `kalamar-relay` (the UDP relay
the games talk through). **What you need**: a machine with a public IPv4 address (1 vCPU and 1 GB are enough for
both programs and a small community; [Capacity of a relay](#capacity-of-a-relay) says how far it goes), a domain
name, and the right to open one UDP port.

> **The relay is UDP. It does not go through your reverse proxy.** Caddy and nginx carry the HTTPS API and the
> WebSocket; the game's traffic goes straight to the relay's UDP port (47313 by default). Open that port in the
> firewall and in your provider's security group, to the relay directly. A proxy or CDN that only forwards HTTP (or
> TCP) cannot carry it, and a domain behind such a proxy cannot name the relay: give the relay its own name that
> resolves to the machine's real address (for example `relay.example.org`), or an IPv4 address.

## 1. Names

Create two DNS records pointing at the machine (IPv4, `A` records; the game is IPv4 only):

- `play.example.org`: the API and the discovery document, behind HTTPS;
- `relay.example.org`: the relay (UDP). It can be the same machine; do not put it behind an HTTP proxy.

Players will type `play.example.org` in the launcher. Nothing else.

## 2. Build or copy the programs

On a machine with Rust 1.97 or newer:

```sh
cargo build --release --locked -p kalamar-service -p kalamar-relay
sudo install -m 755 target/release/kalamar-service target/release/kalamar-relay /usr/local/bin/
```

Or build the container image of `deploy/Dockerfile` (it holds both programs; its header shows the two `docker run`
lines).

> **Shortcut.** `./deploy/install.sh` asks what to install (a whole server or one more relay), how (Docker, systemd
> or only the files) and which preset, does sections 2 to 5 for you, and ends with a summary of where everything is
> and how to change it. The sections below explain what it sets up, and how to do it by hand.

## 3. Configure the service

```sh
sudo mkdir -p /etc/kalamar
# a short file for your kind of community (init --list names the presets: friends, competitive, tournament, full)
sudo kalamar-service --config /etc/kalamar/kalamar.toml init --preset competitive --data-dir /var/lib/kalamar --no-relay-secret --name "My community" --host play.example.org --operator Camille --contact mailto:admin@example.org
# ... or the full reference file:  sudo cp deploy/kalamar.example.toml /etc/kalamar/kalamar.toml
sudoedit /etc/kalamar/kalamar.toml          # read it; in the reference file change every value marked CHANGE
# competitive and tournament bind a relay of its own by a secret: made here as root (mode 0600), handed to both
# programs in section 4. --no-relay-secret keeps init from making another one in the data folder
sudo kalamar-relay --new-room-secret /etc/kalamar/relay.key
```

With the units of section 4, the `[[relays]]` entry of the file names the copy systemd hands to the service:
`secret_file = "/run/credentials/kalamar-service.service/relay.key"`. That file exists only while the service runs,
and `check-config` reads every secret the configuration names: run it once the service is started (section 4).

`init` takes `--data-dir /var/lib/kalamar` (the systemd units' state folder) or `/data` (the container image), and
`--bind`, `--trusted-proxies` for a container; it applies them to every preset, `full` included. It also creates the
relay secret the file names (mode 0600 on Unix, an existing one is kept), so that `check-config` passes at once;
`--no-relay-secret` leaves that to you, for a secret that must belong to another account.

A preset is an ordinary configuration: every option of `deploy/kalamar.example.toml` can be added to it.

The lines that matter first:

| Key | Value |
|---|---|
| `server.api_url` | `https://play.example.org/api` (the public HTTPS address of the API) |
| `server.bind` | `127.0.0.1:8080`: plain HTTP on loopback, only the proxy reaches it |
| `server.production` | `true`: refuses the `dev` login and the test clock |
| `server.database`, `server.secret_file` | under the service's state folder (`/var/lib/kalamar/...` with the systemd unit). **Back both up**: without the secret, logins cannot find their accounts |
| `server.trusted_proxies` | the proxy's address (`127.0.0.1/32`, `::1/128`): only then is `X-Forwarded-For` believed |
| `server.privacy_url`, `server.rules_url`, `server.operator` | your privacy notice (adapt `PRIVACY-TEMPLATE.md`), your rules, your name and contact |
| `[features]` | the areas your community wants; everything else stays hidden in launchers |
| `[auth.guest]`, `[[auth.oauth]]`, `[auth.password]` | at least one login: see [Choosing login methods](#choosing-login-methods) below |
| `[[relays]]` | `host = "relay.example.org"`, `port = 47313` |
| `[rooms]`, `[[queues]]`, `[[activities]]` | your offering: formats, modes, stages, rotations, rated or not, tournaments |

Everything the launchers show comes from this file.

**Changing it later, without a restart.** Edit the file, then `kalamar-service --config /etc/kalamar/kalamar.toml
admin reload`: it prints what was applied, or the errors (an invalid file changes nothing). `systemctl reload` (the
unit sends SIGHUP) reloads too but reports nothing: the outcome is only in the journal. Players stay logged in, rooms
and matches go on (each keeps the result policy it was created with), launchers redraw. The command names what still
waits for a restart: the listening address and API path, `production`, the database and secret files,
`trusted_proxies`, the logins, `[rate_limits]`, `[[webhooks]]`, `[embedded_relay]` (and `[[relays]]` while a change of
`[embedded_relay]` waits).

**Relays bound to your server.** The presets bind the relay to your server with a secret: a relay started with
`--room-secret-file relay.key` opens only room codes sealed with it, and the service seals every code of that relay
(`secret_file` in its `[[relays]]` entry). Strangers who know the relay's address cannot open rooms on it. What it does
not prevent: whoever holds a room code (every player of its match) can use that room for 24 to 48 hours, and whoever
holds a relay's secret can make codes for that relay ([relay.md](protocol/relay.md#42-relays-bound-to-one-service)).
The service reads every relay secret when it starts (a missing or unreadable one stops it, with the file's name) and
at each reload.

### One program or two

The preset `friends` runs the relay inside the service (`[embedded_relay]`, on a thread of its own): start `serve`,
open UDP 47313 to the machine, and skip the separate relay of the next section. Its secret, `data/relay.key`, is
created at the first start (back it up with the rest); the `[[relays]]` entry that names this machine takes it, and
`check-config` refuses a configuration where the relay is announced without it (every match would fail).

The presets `competitive` and `tournament` run `kalamar-relay` as a program of its own, bound by a secret file
(`init` creates `relay.key` in the data folder unless given `--no-relay-secret`; `kalamar-relay --new-room-secret
<file>` makes one anywhere): start the relay with `--room-secret-file <file>` and name the same secret in the
`secret_file` of the service's `[[relays]]` entry (section 4 does both with systemd).

Why: on its own thread the relay does not wait for the API's work (timed with a `HELLO` every 100 ms while three
conformance suites worked the API, the service restricted to one worker thread: 0.3 ms at the median and 0.4 ms at
p99, the same as `kalamar-relay` apart; `cargo run --release -p kalamar-relay --example hello_latency` repeats the
measurement). It still shares the process and the machine's processors: for more than a few dozen players at once,
or for events where every millisecond is watched, run `kalamar-relay` apart.

**A second relay.** Another region, a spare, or more players than one relay carries: on that machine create its own
secret (`kalamar-relay --new-room-secret relay-2.key`) and start `kalamar-relay --room-secret-file relay-2.key`; copy
`relay-2.key` to the service's machine (readable by the service only), add a second `[[relays]]` with its host, an
`id` and `secret_file` pointing at that copy, then `admin reload`. One secret per relay: with a shared secret, whoever
runs one relay can open rooms on the others (`check-config` warns). Launchers measure their latency to each relay and
every match gets the one that suits its players (only the players' measurements count, bounded to 1-2000 ms); a relay
that stops answering gets no new match while another one answers. A relay bound to your server also tells it its load
(protocol 1.1.13): among the relays close enough in latency for the match's slowest player (within 30 ms of the best,
`[relay_load] latency_tolerance_ms`), the match goes to the one with the most room left, never to a full one while
another has room; when every relay is full, a room start says so to the player (`relay_capacity`) and queues wait.
`kalamar-service --config /etc/kalamar/kalamar.toml admin relays` shows each relay as the service sees it: reachable,
its sessions and rooms against its caps, each worker's load, its traffic, and whether it takes new matches.

### Choosing login methods

You may offer several at once; launchers show every one. All are off until enabled (the example turns on `guest`).

| | Just a name (`[auth.guest]`) | OAuth / OpenID Connect (`[[auth.oauth]]`) | E-mail and password (`[auth.password]`) |
|---|---|---|---|
| Player | types a name | clicks, logs in at the provider in the browser | an address and a password, no third party |
| Lost access | the account is lost with the launcher's login | the provider's own recovery | a reset code: from you, or by e-mail with SMTP |
| You store | the account only | a keyed hash of the provider's user id | a keyed hash of the address, an Argon2id hash of the password, hashed one-time codes |
| You run | nothing | an application registered with the provider, its secret in a file | the password forms; optionally an SMTP account; resets by hand without it |
| Risk you carry | an account costs one request: bans, cooldowns and reports are evaded with a new one, and anyone can bring a second account (in the stands too). Keep guests out of ranked play with `ranked = false` (the presets `competitive` and `tournament` do); `check-config` warns otherwise | the provider's availability and terms | a database of password hashes: protect it and the secret file, and plan for a breach (LEGAL-NOTES, section 3) |

**OAuth.** One `[[auth.oauth]]` entry per provider, with a `preset` (`google`, `github`, `twitch`, `discord`) or
`preset = "oidc"` and the `issuer` of any OpenID Connect provider (Keycloak, Authentik, your website's SSO), or
every endpoint by hand with the path of the user id in the userinfo answer (`subject = "data.0.id"`). At the
provider, register a "desktop" or "native" application whose redirect URIs are the loopback addresses of
`redirect_uris` (`http://127.0.0.1:53682/callback` and `...:53683/callback` by default). Put the client secret in a
file readable only by the service (`client_secret_file`) or in an environment variable (`client_secret_env`); never
in the configuration. Never change an `id` once players use it. `check-config` reads every secret it names.

**E-mail and password.** `[auth.password] enabled = true` adds four forms (log in, create an account, forgotten or
new password, change password) that the current launcher already shows. The protocol forbids requiring an e-mail
address, so keep `guest` or an OAuth provider too (`check-config` refuses otherwise). Two ways to reset a forgotten
password:

1. **Without e-mail** (the default): the player writes to you; check it is them (as you would for any request on an
   account), run `kalamar-service -c /etc/kalamar/kalamar.toml admin password-reset <user>` and give the code
   privately (it works once, for an hour). The forms say so, with your `operator.contact`. Addresses are then not
   confirmed: anyone can sign up with an address that is not theirs (it is only a login name).
2. **With `[auth.password.smtp]`**: codes go by e-mail (15 minutes), sign-ups confirm the address first, and the
   player is told when the password changes. You need an SMTP account (your provider's, or a transactional mail
   service); TLS is required (`starttls` on 587 or `implicit` on 465), the SMTP password goes in `password_file` or
   `password_env`. The mail provider then processes addresses for you (say so in your privacy notice).

Failed logins slow down per account and per client address (in memory; `lockout` settings): an attacker cannot lock
a player out of an address they logged in from before, and a lock lifts by itself minutes after the failures stop.
A reset ends every other session of the account. Turn it on only with `production = true` behind HTTPS
(`check-config` warns otherwise), and adapt your privacy notice (`PRIVACY-TEMPLATE.md`).

None of these flows has been run against a real OAuth provider or a real mail server, only against local stand-ins.

## 4. Run both programs

With the hardened units of `deploy/systemd/`:

```sh
sudo cp deploy/systemd/kalamar-service.service deploy/systemd/kalamar-relay.service /etc/systemd/system/
# the service reads the relay secret through a drop-in (skip these two lines with the preset friends)
sudo mkdir -p /etc/systemd/system/kalamar-service.service.d
printf '[Service]
LoadCredential=relay.key:/etc/kalamar/relay.key
' | sudo tee /etc/systemd/system/kalamar-service.service.d/relay-key.conf
sudo systemctl daemon-reload
sudo systemctl enable --now kalamar-relay kalamar-service
sudo kalamar-service --config /etc/kalamar/kalamar.toml check-config   # as root: it reads the credential
curl -s http://127.0.0.1:8080/.well-known/kalamar-server.json      # the discovery document, from the machine
```

The relay unit listens on `0.0.0.0:47313` (UDP); the service unit on the `bind` address. Open UDP 47313 in the
firewall (for example `sudo ufw allow 47313/udp`) and nothing else besides 80/443 for the proxy. With the preset
`friends`, skip `kalamar-relay.service`: the relay runs inside the service.

The units run each program as a dynamic account of its own, which cannot read the other's files, so systemd (247
or newer) hands each of them the root-only secret of section 3 (`LoadCredential`): the relay unit has the line, and
refuses to start without the file (unless `--open` says the relay is meant to be open to anyone); the service gets it
from the drop-in above, at `/run/credentials/kalamar-service.service/relay.key`, the path its `[[relays]]` entry
names. One more relay on another machine is one more secret file and one more `LoadCredential` line in the drop-in.
The two units, the drop-in and a relay with one worker per processor were run this way on one Debian machine.

Without a recent systemd, run each program as a system account of its own whose groups let the relay read that
secret and nothing else (what `deploy/install.sh --mode systemd` does); the secret is then
`/var/lib/kalamar/relay.key`, in the relay's `--room-secret-file` and the service's `secret_file` alike:

```sh
sudo useradd --system --user-group --home-dir /var/lib/kalamar --no-create-home --shell /usr/sbin/nologin kalamar
sudo useradd --system --gid kalamar --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin kalamar-relay
sudo install -d -m 750 -o kalamar -g kalamar /var/lib/kalamar
sudo -u kalamar kalamar-relay --new-room-secret /var/lib/kalamar/relay.key       # mode 0600, owner kalamar
sudo chmod 640 /var/lib/kalamar/relay.key                                        # the relay's group reads it
# kalamar-service.service: replace DynamicUser=yes by  User=kalamar  and  Group=kalamar
# kalamar-relay.service:   replace DynamicUser=yes by  User=kalamar-relay  and  Group=kalamar, remove LoadCredential,
#                          and ExecStart=... --room-secret-file /var/lib/kalamar/relay.key
```

The service creates its database readable by its own account only (mode 0600), so the relay's account, which faces
the internet, reads the relay secret and nothing else of `/var/lib/kalamar`.

Admin commands then run as that account, so that the files they create stay readable by the service:
`sudo -u kalamar kalamar-service -c /etc/kalamar/kalamar.toml admin ...`.

**Several players behind one address (the same box, a LAN party).** The relay lets one source (an IPv4 address, an IPv6
/64) hold at most 8 members of a room and 24 sessions in all, so that one address cannot fill the relay or take every
place of a room. Players who share one public address share that limit: a whole 4 v 4 behind one box plays with no
setting, but the ninth member of a room from the same address is refused (`ROOM_FULL`): a game of protocol 1.1.13 stops
with `relay-source-limit` and its launcher tells the player "too many players from the same Internet connection in this
match" (an older game says `network-unavailable`). For a larger LAN party (spectators too, or several matches at once
from one address), raise it: add `--max-members-per-source 12` (a room's size; `0` removes the limit) and, if needed, a
higher `--max-sessions-per-source` to the relay's `ExecStart` line (`sudo systemctl edit --full kalamar-relay`, then
`sudo systemctl restart kalamar-relay`), or `max_members_per_source = 12` (and `max_sessions_per_source`) in
`[embedded_relay]` for the relay inside the service (a restart applies it). The other per-address limits of the service
(new accounts, logins, requests) are in `[rate_limits]`.

## 5. Put HTTPS in front

Caddy gets and renews the certificate by itself: install it, copy `deploy/Caddyfile` to `/etc/caddy/Caddyfile`,
replace `play.example.org`, reload. With nginx, use `deploy/nginx.conf` and an ACME client (certbot). Both examples:

- send `/.well-known/kalamar-server.json` and `/api/*` to `127.0.0.1:8080`;
- carry the WebSocket upgrade (`/api/v1/events`) with read timeouts above the service's 20 s pings;
- **replace** `X-Forwarded-For` with the client's address (never append what the client sent);
- keep access logs off (they contain IP addresses, which are personal data);
- serve your `/privacy` and `/rules` pages as static files.

Check from another machine:

```sh
curl -s https://play.example.org/.well-known/kalamar-server.json
curl -s https://play.example.org/api/v1/meta | head -c 300
```

## 6. Check the relay from outside

The relay answers only its own protocol. From two shells on a player's PC with a built game, the engine's probe acts
as the game's LAN module does through the relay (`engine/docs/lan-relay-protocol.md` in the Kalamar repository,
"Checking a relay"):

```powershell
$env:KLM_LAN = 'relay:<the relay IPv4>:47313'; $env:KLM_LAN_ROOM = 'TEST'; <game>.exe --probe-lan-peer
```

Each prints `probe-lan-peer: passed` once the two have reached each other. Or play a private match with a friend
through your server. If players' games stop with `network-unavailable`, the UDP port is not reachable; with
`relay-full`, the relay is at its cap (`admin relays`, and section [Capacity of a relay](#capacity-of-a-relay)).

## 7. Test with simulated players

On a **test** instance (it creates accounts and matches, files a report and erases accounts), from any machine:

```sh
kalamar-testkit conformance --api https://test.example.org/api --max-wait 30
kalamar-testkit simulate --api https://test.example.org/api --players 4 --provider guest
```

With only an OAuth login, create test accounts with `kalamar-service ... admin user-create --name tester` and pass
their tokens with `--tokens`. The suite also checks every OAuth provider up to the PKCE check (no provider is
contacted) and, with password accounts whose sign-up needs no e-mailed code, signs up, logs in and changes a password
with addresses under `example.invalid` (it erases them afterwards).

## 8. Tell players, and run the community

- Give players the line `play.example.org`. The launcher shows your server's name, operator, rules and privacy link
  before they confirm.
- Moderation: `kalamar-service -c /etc/kalamar/kalamar.toml admin ...` (users, bans, reports, a match with its
  reports, season roll, export and erasure of a player's data). Bans and erasures apply within 30 s.
- Players' data rights: export and erasure are part of the protocol, and the Kalamar launcher offers both in a
  server's Account tab. For a player who writes to your contact instead: `admin export <user> --out export.json`,
  `admin erase <user> --yes`.
- Upgrades: **back up the database first**, then follow [upgrading.md](upgrading.md): database migrations run at
  start, and a database newer than the binary is refused, with both schema numbers.
- Backups: the database, the secret file and the relay secrets (`relay.key`).

## Capacity of a relay

How many players one relay carries, measured with the relay's load bench
(`crates/kalamar-relay/examples/relay_bench.rs`), and what the measurement does not say.

**What a match sends** (measured in the game's network log of real matches through a relay): each player sends
**15 datagrams a second to each other player** (unicast), **158 bytes** of payload on average (45 to 269), 190 bytes
with the relay's header. The relay forwards each to one member: it sends what it receives.

| Traffic through the relay | Datagrams a second, each way | Each way (IPv4 and UDP headers counted) | Per hour, each way |
|---|---|---|---|
| a 1 v 1 match | 30 | 0.05 Mbit/s | 24 MB |
| a 4 v 4 match | 840 | 1.5 Mbit/s | 660 MB |
| one player of a 4 v 4 | 105 | 0.18 Mbit/s | 82 MB |

Ethernet framing adds about 15 %. **The network is usually the first limit**: 100 Mbit/s carries about 60 matches of
4 v 4 at once (480 players), 1 Gbit/s about 600; a provider that meters traffic counts about 0.66 GB out (and as much
in) per hour of 4 v 4. Small cloud machines often also cap the packets a second (tens to hundreds of thousands each
way): a 4 v 4 is 840 each way.

**Processor, measured.** Rooms of 8 members at the rate above, on one desktop PC (8 cores, Windows) at rest:
the relay on 4 cores (8 logical processors), the bench's simulated members on the other 4, everything over loopback.
Latency is the time from a member's send to the other member's receive through the relay; the bench alone, member to
member without a relay, measures 0.04 to 0.08 ms at the median.

| Relay | 4 v 4 matches with no loss and a p99 under 5 ms | Datagrams relayed a second, at most | Latency at 40 matches (median / p99) |
|---|---|---|---|
| `--workers 1` | 40 (320 players); at 60, 1.7 % lost | 51 000 | 0.16 / 1.3 ms |
| `--workers 2` | 80 (640 players); at 100, 0.1 % lost | 90 000 | 0.08 / 0.26 ms |
| `--workers 4` | 140 (1 120 players), the most the bench sends: p99 0.5 ms, the workers at 81 % | more than 117 600 (about 145 000 at saturation) | 0.07 / 0.17 ms |
| `--workers 8` | as with 4 (8 logical processors on 4 cores add processor time, not capacity) | more than 115 000 | 0.07 / 0.18 ms |

Each datagram relayed costs 20 to 28 microseconds of processor here (its receive and its send on Windows loopback,
where a send also pays for waking its receiver). One worker relays about 50 000 datagrams a second at saturation,
**some 60 matches of 4 v 4 or 1 700 of 1 v 1**; workers add up well (two: 1.8 times one; four: about 2.8 times, on
Windows where they share one socket). No datagram left out of order in any run. The same runs with another program
using 4.5 cores of the PC at the same time (the relay at a lower priority) gave about three quarters of these rates
and a p99 of a few milliseconds. Memory: about 0.45 KB per session (1.8 MB with 8 sessions, 3.6 MB with 4 000),
never the limit.

**From there to a machine** (extrapolated, not measured): 36 000 datagrams a second per worker (what four workers
reached each, the lower figure), a virtual processor of a small cloud machine counted as half a core of the test PC, a
relay kept under 70 % of its workers' time, and the service sharing the smallest machines:

| Machine | `kalamar-relay` | 4 v 4 at once (players), by processor | 1 v 1 at once, by processor | By the network: 100 Mbit/s / 1 Gbit/s |
|---|---|---|---|---|
| 1 vCPU, 1 GB, with the service | 1 worker, sharing the processor | about 7 (56) | about 200 | 60 / 600 matches of 4 v 4 |
| 2 vCPU, 2 GB | 2 workers | about 25 (200) | about 700 | the same |
| 4 vCPU, 4 GB | 4 workers | about 50 (400) | about 1 500 | the same |
| 8 vCPU, 8 GB | 8 workers | about 100 (800) | about 3 000 | the same |

What loopback does not say: on a real network a send costs less (no receiver to wake on the same machine), and Linux
handles a datagram in less time than Windows; neither was measured here, so the processor's figures are on the low
side. They say nothing of the provider's network: its bandwidth, its limit of packets a second, and the latency between
your players and the relay (the relay adds the path player, relay, player: put relays near your players, one per
region, `[[relays]]` with a `region`). The default caps (1024 sessions per worker, `--max-sessions`) are a safety
limit, not the processor's: one worker saturates near 480 sessions in 4 v 4 matches here, and what keeps your service
from sending more is the load the relay reports (`[relay_load]`: the workers' time spent working). Where the network is
the limit, set `--max-sessions` to what it carries (100 Mbit/s: about 480 sessions in 4 v 4 matches). To measure your
own machine, run the bench against a relay started for it:

```sh
kalamar-relay --open --bind 127.0.0.1 --port 47399 --hello-rate 0 --join-rate 0 --max-sessions 0 --max-rooms 0
cargo run --release -p kalamar-relay --example relay_bench -- --relay 127.0.0.1:47399 --rooms 20,40,80 --threads 4 --senders 2
```

`--hello-rate 0 --join-rate 0` because every simulated member joins from one address; the relay's statistics line
shows each worker's load (`load_pct=`) as the bench runs.

## Checklist

- [ ] DNS: `play` (HTTPS) and `relay` (UDP, real address, no HTTP proxy)
- [ ] `check-config` passes with `production = true`
- [ ] UDP 47313 open to the relay; 80/443 to the proxy; 8080 closed from outside
- [ ] `trusted_proxies` set to the proxy only
- [ ] privacy notice and rules published, `privacy_url` set (it names your login methods and any mail provider)
- [ ] OAuth client secrets and the SMTP password in files readable only by the service (`chmod 600`), not in the
      configuration
- [ ] backups of the database, of `secret_file` and of the relay secrets (`relay.key`, mode 0600)
- [ ] one secret per relay; `check-config` prints no warning about relays
- [ ] proxy access logs off or short-lived
- [ ] conformance suite passed on a test instance

See also [SECURITY.md](../SECURITY.md) (hardening checklist) and [LEGAL-NOTES.md](LEGAL-NOTES.md).
