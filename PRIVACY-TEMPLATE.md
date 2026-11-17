# Privacy notice — TEMPLATE

> This is a **template** for operators of a Kalamar server. It is not legal advice. Replace every `[bracketed]`
> part, remove what does not apply to your configuration, and check it against the law that applies to you (for
> example the GDPR in the European Union). Publish it at the address you set as `privacy_url`.

**Server:** [server name] — **Operator (controller):** [name or association, address if required] —
**Contact:** [e-mail or form] — **Last updated:** [date]

## 1. What this server is

[Server name] is a community server for the Kalamar launcher. It lets players find each other, play matches of their
own copy of the supported game, and keep ratings and statistics. It is not affiliated with any game publisher.

## 2. What we store, and why

| Data | Purpose | Legal basis (GDPR) | Kept |
|---|---|---|---|
| Account id, display name, creation date, settings | providing your account | contract (art. 6(1)(b)) | until you erase your account |
| A keyed hash of your login identity ([Discord / Google / GitHub / Twitch / guest / e-mail address / ...]); never your provider id, and never your e-mail address in clear | finding your account when you log in | contract | until you erase your account [while a ban runs: see 5] |
| [If you offer e-mail and password accounts:] an Argon2id hash of your password (with its own random salt; we cannot read your password) | checking your password | contract | until you change it or erase your account |
| [Same case:] one-time codes (stored only as a keyed hash), [the e-mails we send you: a code, a notice that your password changed] | resetting a forgotten password[, confirming your address] | contract | codes: [15 minutes by e-mail / 1 hour from us], then deleted |
| [Same case:] the network addresses your account logged in from, and failed attempts | protecting your account against password guessing | legitimate interest (security) | in memory only, at most [30] days, lost at each restart |
| Hashes of your login tokens, session dates | keeping you logged in securely | contract | until the session expires (at most [30] days) |
| Friend code, friends, friend requests, blocks | the social features you use | contract | until removed or account erased |
| Matches: rules, participants, member names, the result files your launcher uploaded, statuses, decisions | deciding results fairly, history, disputes | contract and legitimate interest (fair play) | raw result and status files [30] days; decisions and statistics until you erase your account |
| Ratings, statistics | rankings and leaderboards | contract | until you erase your account |
| What your launcher says about your game engine: match contract versions, the modes and stages your copy offers, the engine's capability list, and [if you run several relays] the network latency your launcher measured to each of our relays | offering you only matches your game can play [and choosing the relay closest to a match's players] | contract | replaced at each login; until you erase your account |
| A hidden score of how your game hosted the matches it hosted (quick starts raise it; matches it made fail, and a game that crashed or that you closed while hosting, lower it) | giving the host's role of a match to players whose games host well | legitimate interest (matches that start) | until you erase your account |
| Penalties, reports you file or that concern you, bans | moderation | legitimate interest (a safe community) | [reports: until resolved + 365 days] |
| A log of administrative actions (a ban, an erasure, a password reset by the operator): the action, its time and the account id concerned, never a name | accountability of moderation | legitimate interest | [365] days, also after the account is erased |

We do **not** store your IP address in our database. Our server and reverse proxy [do not keep / keep for at most
[N] days] access logs containing IP addresses, for security. Rate limiting works in memory only.

**During a match**, your game's traffic goes through our relay [at relay.example.org; and through the relay of
[partner], who runs one of our relays in [region]]. The relay does not read it (it adds no encryption of its own)
and stores nothing on disk, but its log records, for each player who joins or leaves a room: the name your game
uses in the match (your display name), your IP address and port, a session number and a tag of the room (a short
hash, never the room code), with the time. We use it to investigate abuse and network problems, and keep it [as long
as our system journal keeps logs: N days]. [If your launcher measures its latency to our relays (when we run
several), each relay also sees your IP address then, without logging it.]

**Presence** (online, in queue, in match) is shown only to your friends, and you can hide it. **Statistics and
history** are visible to [all players of this server] unless you turn "public profile" off (then only friends). You
can leave the **leaderboards** in your settings. [If `population` is on:] Anyone can see **how many** players are
online, in a match and searching: numbers only, never who.

If you log in with **[Discord / Google / GitHub / Twitch / our club's account]**, we receive your user id there
(stored only as a keyed hash) and [your public name there, used once as your initial name]. We ask for nothing else
([Discord: `identify`; Google: `openid`; GitHub: your public profile; Twitch: `openid`]): not your e-mail, your
contacts or your messages. That provider learns that you log in to this server; its own privacy policy applies to
it.

[If you offer e-mail and password accounts:] If you create an account with an **e-mail address and a password**, we
never store your address itself: only a keyed hash of it, enough to find your account when you type it again. [We
send e-mails only when you ask for them in the launcher (a code to create your account or reset your password) and
when your password changes; through [mail provider, country], who processes your address for us.] [We send no
e-mail: to reset a forgotten password, write to [contact]; we will check it is you before giving you a code.]

## 3. Who receives it

[Nobody else.] [Our hosting provider [name, country] processes the data on our behalf.] [Our mail provider [name,
country] receives the address and the content of the e-mails we send you.] Other players see what the protocol shows
them: your display name, your friend code if you share it, your matches and statistics as above; never your e-mail
address.

## 4. Your rights

From the Kalamar launcher (Community, your server's Account tab, "Your data") you can at any time save a copy of all
the data we hold about you as one JSON file, and **erase** your account. [With another launcher that lacks these
buttons: write to [contact] and we export or erase it for you.] You can also write to [contact] to exercise your rights of access, rectification, erasure, restriction,
objection and portability, and you may complain to your data protection authority ([e.g. CNIL in France]).

## 5. Erasure and bans

Erasing your account deletes it with your friends, sessions, ratings, statistics and penalties [, your password hash
and any pending code]; your lines in other players' matches are anonymized. Reports other players filed about you
stay, anonymized, until they are resolved and then [365] days, like any report.

To prevent evading a sanction by erasing the account and creating it again, we keep a keyed hash of your login
identity [(your provider id, or your e-mail address)] — never the identity itself, and nothing that names you — with
only what still runs when you erase your account:

- a ban: until it ends;
- a queue cooldown, and the penalties of the last [24 hours] that make the next one longer: until they end;
- the reports about you that are still open: at most [30] days.

If you create an account again with the same login during that time, it takes these over; otherwise the hash is
deleted when the last of them ends. A guest account (a name only) leaves nothing behind: it can never be logged in to
again.

## 6. Security

Tokens and login identities are stored hashed; the API is served over HTTPS only. [Passwords are stored only as
Argon2id hashes; repeated failed logins are slowed down; changing or resetting your password ends your other
sessions. If our database were ever stolen, we would tell the data protection authority and, when the risk is high,
you: do not reuse your password from another site.] Results are reported by players' launchers and cross-checked; see
the server rules for what this does and does not guarantee.

## 7. Children

[This server is meant for players aged [13/15/16] or over / State your policy.]

## 8. Changes

We will announce changes to this notice [in the launcher's server description / on [page]].
