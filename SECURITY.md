# Security

## Reporting a vulnerability

Please report vulnerabilities **privately**, never in a public issue or pull request. GitHub's private vulnerability
reporting is the only way to report one: the repository's **Security** tab, then **Report a vulnerability**
([how it works](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)).
Only the maintainers see the report until an advisory is published.

Give the affected version or commit, the component (service, relay, test kit, installer, the specification itself),
what an attacker can do and from where, and the steps to reproduce. Leave out real players' data, tokens and
addresses.

The project is maintained by volunteers in their free time: no response time is guaranteed. A confirmed
vulnerability is fixed in a release and published as a GitHub security advisory once operators have had time to
update, crediting the reporter if they wish.

Only the latest release, and the `main` branch, receive security fixes.

**In scope**: the reference service, the relay, the installer and the example deployment files, the test kit, and
weaknesses of the protocol itself (a flaw that every conformant server would share). **Out of scope**: servers run by
other people (report to their operator), the launcher and the game (the Kalamar repository has its own policy), and
abuse on a server (cheating, harassment): operators handle it with their moderation tools. Results reported by
players' machines are trusted only as far as the cross-checks go, by design (below, "Known limits").

## Security model

Who the server trusts, what one cheating participant can and cannot do, the residual risks an
operator accepts by publishing a server, and what was done about each finding of the security review:
[docs/security-model.md](docs/security-model.md). A risk that page lists as residual or accepted is known; a way
around a protection it or this page claims is a vulnerability to report as above.

## What the software protects, and how

| Asset | Protection |
|---|---|
| Access, refresh and WebSocket tokens | 256-bit random, stored only as SHA-256 hashes (domain-separated); refresh rotation with a 60 s reuse window, reuse after it revokes the session |
| Login identities | stored only as HMAC-SHA-256 under the server secret (`secret_file`); the provider's user id is never stored in clear |
| OAuth login (any provider) | authorization code + PKCE (S256) verified by the server before any provider call, single-use `state` valid 10 minutes and bound to its provider, loopback redirect URIs from an allow-list, client secret read from a file or a variable and never sent to launchers; provider calls over HTTPS only (loopback http on test servers), no redirect followed, 10 s and 64 KiB at most, provider tokens never stored or returned, failures answered `502 provider_error` repeating at most the standard OAuth error code; OpenID discovery checks the document's issuer |
| Passwords | Argon2id (OWASP parameters, 19 MiB x 2 by default, configurable; refused below the OWASP minimum in production), random salt per account, PHC strings rehashed on login when parameters change, verified in constant time by the library, computed outside the state lock with a bounded number at once; 10 to 128 characters by default, not the address, its local part or the display name; never logged, never returned, not in exports |
| E-mail addresses | normalized, stored only as HMAC-SHA-256 under the server secret (like login identities), never in an answer to another player, never logged; mails go only to an address the player has just typed |
| Account discovery | a wrong password and an unknown address get the same answer and one Argon2 check each (a dummy hash); reset requests answer alike; requests that would store a code for an existing address store a decoy otherwise, so the database work is the same |
| Password guessing | failures spend in-memory token buckets per client address and per account (unknown addresses alike); an account's bucket never blocks an address it logged in from (remembered in memory, 30 days); locks lift by themselves; a successful login or reset clears the account's failures |
| One-time codes | 50 random bits, stored as HMAC-SHA-256, single use, 15 minutes by e-mail or 1 hour from the operator, five tries; e-mailed codes limited per address and per hour; a reset or a change ends every other session |
| Outgoing e-mail | TLS required (STARTTLS or implicit, certificates checked), credentials from a file or a variable, sent by a background task (a request never waits for the mail server), no address or code in logs |
| Relay rooms | given only to a match's participants once everyone accepted, never logged (the relay logs an 8-digit hash tag); an open relay's codes are 130-bit random; a relay bound by a secret opens only **sealed codes** (65 random bits, the day, an 80-bit HMAC-SHA-256 tag: relay.md 4.2) and refuses others with `JOIN_NAK` 3 |
| Relay secrets | hexadecimal files, created exclusively and, on Unix, with mode 0600 from the first instant (`kalamar-relay --new-room-secret`, the embedded relay's at the first start, the server's `secret_file` too); read once at start and at each reload, kept in memory, compared in constant time, never printed (`Debug` shows the length); a missing or unreadable one stops the start. On Windows a new file takes the permissions of its folder: keep secrets in a folder only the service's account can read |
| Room codes | rate-limited joins (per account), passwords compared in constant time, kicked players stay out |
| Client addresses | `X-Forwarded-For` believed only from configured proxies; rate limits in memory only; addresses never logged by the service |
| Input | body size limits per route, strict JSON parsing, bounded strings and lists; malformed input gives 4xx, never 5xx (tested) |
| Results | validated against the match (checks V1 to V11) and cross-checked between players under a declared policy |

## Known limits (by design)

- Results are reported by players' machines. Collusion and modified games are not detectable by cross-checking
  (`docs/protocol/overview.md`, section 3.2).
- The relay forwards datagrams in clear and admits anyone who knows a room code (the relay adds no encryption; room codes are secret and unguessable).
- A relay bound by a secret keeps strangers from opening rooms on it, not participants: whoever holds a sealed code
  (every participant of its match, so anyone with an account who played there) can open or join that room for 24 to
  48 hours, and a code seen on the network (the `JOIN` is in clear) can be replayed for as long. Every relay that
  shares one secret can make codes for the others: give each relay its own (`check-config` warns about shared ones).
- The relay chosen for a match follows the latencies the players' launchers declare: a lying player can push a match
  towards a relay it prefers (latency for the others), never onto one another player declared unreachable while one
  every player reached exists.
- The `dev` login provider and the test clock are for test servers; a configuration with `production = true`
  refuses them.
- Password accounts: the keyed hashes of addresses and the password hashes are as safe as `secret_file` and the
  database together. With both stolen, addresses can be confirmed by guessing and weak passwords cracked offline
  (Argon2id makes it slow, not impossible). Without e-mail, addresses are not confirmed (anyone can use an address
  that is not theirs as a login name). Without SMTP, a sign-up says when an address is already taken (unavoidable
  without confirmation; rate-limited per client address).
- Locks are in memory: a restart clears them. An attacker with many client addresses (IPv6 ranges, botnets) is slowed
  by the per-account bucket only, and can keep an account locked for addresses it never logged in from while the
  attack lasts; the owner's known addresses and a reset still work. Someone who shares an address the account logged
  in from (the same home or LAN-party network) is limited by the per-address bucket only.
- Anyone who knows an account's address can spend the five tries of its pending reset code (the operator or the
  player then asks for a new one).
- The OAuth presets (Discord, Google, GitHub, Twitch) and the OpenID discovery follow the providers' documentation
  and were tested against local stand-ins only, never against the real services.

## Hardening checklist for operators

- Run behind HTTPS (`deploy/`), keep `bind` on loopback, set `trusted_proxies` exactly.
- Protect `secret_file`, the relay secrets and the database (backups encrypted at rest, kept apart from each other);
  keep OAuth client secrets and the SMTP password in files readable only by the service (`chmod 600`), never in the
  configuration. Give each relay its own secret.
- Password accounts: only with `production = true` behind HTTPS; keep the default Argon2 parameters or stronger; give
  reset codes only to a player you have identified, privately; rotate a client secret or the SMTP password at once if
  it may have leaked.
- Use the systemd units in `deploy/systemd/` (no privileges, read-only system, dedicated state directory).
- Keep access logs of your proxy off or short-lived; keep the software updated.
