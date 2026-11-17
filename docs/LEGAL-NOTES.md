# Legal notes

Factual notes for operators, contributors and counsel. **This is not legal advice**; it describes what the software
contains and does.

## 1. Content

- This repository contains the protocol specification, a reference service, a UDP relay and a test kit, all the
  project's own code under the MIT License, plus dependencies fetched from crates.io, all under permissive licenses
  (no GPL or LGPL crate): every crate with its license is in
  [third-party-dependencies.md](third-party-dependencies.md), generated from the lock file. SQLite is bundled through
  `rusqlite` and is in the public domain.
- It contains **no game content**: no game files, no extracted data, no list of the game's modes, stages, weapons or
  gear, no artwork, no fonts. Game identifiers travel as opaque strings that the players' own copies provide.
- It describes and implements **only this project's community service**. It does not describe, imitate or connect to
  any online service of a game publisher. Matches use the game's own local-network mode, tunneled by the relay.
- The match contract names the settings the engine reads (`KLM_*` environment variables): technical identifiers of
  the engine project.

## 2. Operating a server

An operator runs a service for their own community and is responsible for it: moderation, rules, the terms of their
hosting, and the law that applies to them — in particular data protection (an operator is typically the controller
of the players' personal data under the GDPR in the European Union).

What the reference service stores is minimized by design (protocol `overview.md`, section 4; `PRIVACY-TEMPLATE.md`):
a display name, a keyed hash of the login identity (never a provider's user id, e-mail or password in clear; with
password accounts, an Argon2id hash of the password, see section 3), hashes of tokens, social data the player
creates, matches and their reports, ratings, statistics, a hidden score of how each player's game hosted its matches
(to choose a match's host) and moderation records.
IP addresses are not stored in the database; rate limiting works in memory; the example proxy configurations turn
access logs off. Players have export and erasure rights in the protocol; the reference service implements both (and
the operator's command line can perform them on request).

`PRIVACY-TEMPLATE.md` is a starting point to adapt, not a finished notice.

## 3. Logins and what they imply

**OAuth providers** (Discord, Google, GitHub, Twitch, any OpenID Connect provider). An operator who enables one
registers their own application with the provider and accepts that provider's developer terms; the provider learns
that its user logs in to the operator's server. The service asks for the narrowest scopes of each preset (Discord
`identify`, Google `openid`, GitHub none, Twitch `openid`), stores only a keyed hash of the provider's user id and,
if configured, uses a public name once as the initial display name. These flows have been tested against local
stand-in providers only, never against the real services.

**E-mail and password accounts** turn the operator into the keeper of personal data with real breach potential. In
practical terms, under the GDPR (other laws differ; this is not legal advice):

- **What is stored.** A keyed hash (HMAC-SHA-256 under `secret_file`) of the normalized address, never the address
  itself, even with outgoing e-mail: every mail answers a form where the player has just typed the address. An
  Argon2id hash of the password. Keyed hashes of one-time codes, for at most a day. In memory only: the client
  addresses an account logged in from (30 days or until restart) and the failure counters. A keyed hash of an
  address is still personal data (with the secret it can confirm a guessed address); a password hash too.
- **Legal basis.** Performance of the contract (art. 6(1)(b)): the address and the password are how the player logs
  in and recovers the account. Security measures (locks, the remembered addresses) rest on legitimate interest
  (art. 6(1)(f)). Do not use the addresses for anything else (no newsletter): the server cannot even read them back.
- **How long.** Until the account is erased (the player can do it from the protocol, you with `admin erase`); while a
  ban of an erased account runs, the keyed hash of its address is kept to stop ban evasion (say so in the notice).
  Codes expire within the hour (15 minutes by e-mail) and are purged.
- **Security (art. 32).** HTTPS only (`production = true`), the default Argon2 parameters or stronger, `secret_file`
  and the database protected and backed up separately and encrypted, the SMTP password and OAuth secrets in files
  only the service reads, the software kept up to date, reset codes given only to a player you have identified.
- **Processors (art. 28).** With `[auth.password.smtp]`, your mail provider receives the address and the message of
  every mail sent: choose one you can sign a data processing agreement with, and name it in your privacy notice.
  Your hosting provider is a processor too.
- **A breach (art. 33 and 34).** If the database leaks (with or without `secret_file`), you must assess the risk; when
  it is a risk to players, notify your supervisory authority within 72 hours of knowing and,
  if the risk is high (likely when both the database and the secret leak), tell the players, so they change any
  password they reused elsewhere. Keep a record of every breach, even minor ones. Rotating `secret_file` makes every
  login identity unknown: do it only together with a plan for players to recover their accounts.
- **Rights.** Export (`GET /v1/me/export`, `admin export`) says how the address and the password are kept but holds
  neither; erasure deletes the password hash, the address hash and every pending code.
- **Without e-mail** the addresses are not confirmed (an address is only a login name); without SMTP a forgotten
  password goes through you (`admin password-reset`), so check who is asking.

The protocol forbids requiring an e-mail address to play: the reference server refuses a configuration whose only
login is the password one.

## 4. Results and fairness

Results are reported by the players' own games; the service cross-checks them but cannot see the match. Collusion
and modified games are outside what cross-checking can detect (`overview.md`, section 3.2). Operators should say so
in their rules and handle disputes by moderation.

## 5. No money

The protocol has no payment, no shop and no currency, and a server must not ask a launcher to handle money
(`extending.md`, section 5).

## 6. Licenses and authorship

Everything in this repository is the project's own work under the MIT License ([LICENSE](../LICENSE)). Most of the
code and documentation is written by AI coding agents under the maintainer's direction.
