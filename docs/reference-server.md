# What the reference server offers

The protocol lets each server choose its offering. This page tours what the reference server ships, with the
configuration section and the specification page of each part. Every option is in
[`deploy/kalamar.example.toml`](../deploy/kalamar.example.toml).

## Ratings (one example)

The protocol prescribes no rating formula: servers show ratings as presentable data. This server's choice:
**Glicko-2** per ladder and season (team matches rate each player against a composite opponent: the opposing team's
mean, shifted by the player's distance to their own team's mean), **placement** (no value or tier shown until
`placement_games` matches), a **shown value** = rating − 2 × deviation (it rises as the system becomes sure of a
player), **tiers of the server's own design** over that value (Rookie, Bronze, Silver, Gold, Platinum, Diamond, Master, all
configurable), **seasons** (`admin season-roll`: rating pulled halfway back to 1500, deviation at least 250, placement
again). Unrated queues balance teams with a ladder's ratings (`balance_ladder`). Matchmaking: the oldest ticket
anchors a search whose rating window widens with waiting time; teams shrink step by step to `min_teams` after
`shrink_after_s`; parties stay on one team; the most even split is chosen.

## Activities and series

Protocol 1.1 lets a server invent activities that launchers render as views of data components (text, lists,
tables, brackets, forms, buttons, countdowns, series cards): no launcher change, no code, no images. This server ships
two kinds in `[[activities]]`: `tournament` (players register; at `size` entrants a single-elimination bracket starts,
each bracket match a best-of-N series; it reopens after `reopen_after_s`) and `duel_series` (the first two waiting
players play a best-of-N). Series reuse one relay room so launchers of match contract 2 can play the next game without
restarting the game (`continue_from`). Play them with simulated launchers:
`kalamar-testkit conformance --api http://127.0.0.1:8080 --areas activities,series`. Specification:
[activities.md](protocol/activities.md), [series.md](protocol/series.md),
[match-contract-2.md](protocol/match-contract-2.md), [extending.md](protocol/extending.md).

## Stage drafts ("pick and ban")

`[features] draft = true` and `[[drafts]]` presets put a draft between "match found" and the launch: teams ban and
pick stages (and modes) turn by turn, by captain, vote or anyone, with a deadline per turn after which the server
acts; the result goes into the match's rules. Queues (`draft = "<id>"`), rooms (`rooms.drafts`, a form option) and
activities (every game of their series, per game or once per series, "the loser picks") name a preset. Launchers
that do not show drafts still play: the server plays their team's turns, unless the preset is `required`. The
example configuration ships the default (one ban per team, then a random stage, 20 s a turn) and five commented
formats. Specification: [draft.md](protocol/draft.md); conformance: `--areas draft`.

## Spectators ("the stands")

`[features] spectators = true` opens places for spectators: up to two per match, never players,
always shown to the players. Rooms offer them in their form (`[rooms.spectators]`: places, who may sit, whether the
owner may host from the stands) and a member moves into the stands and back like a team change; queues and activities
open them per match or per series (`spectators = { seats, who }`: anyone, friends of the players, referees), taken
from `GET /v1/spectate` before the launch (the game cannot let anyone in after the start). A place is given only when
every participant's launcher declares the spectator seat; otherwise the refusal says what is missing. A spectator's
result file is a witness report: a result policy may let it break a disagreement (`witness = "tiebreak"`) or require a
referee's for rated results (`"required"`, `witness_from = "referees"`); the operator API seats a referee in a match.
Players may refuse to be watched (`[spectators] opt_out`). Specification: [spectators.md](protocol/spectators.md);
conformance: `--areas spectators`.

## Results between two machines

Reports agree when they agree on what decides the match (winner, stage, every player's team and disconnection); the
counters each game keeps are its own and may differ slightly between machines. A result policy's `stats` member
(`flag` by default, nothing to configure) compares the counters with a wide tolerance and flags a larger gap in the
outcome and the log without voiding; `ignore` and `strict` exist. A record whose winner clearly contradicts its own
scores is refused (check V11). The record kept, and so the statistics, is the agreeing report closest to the others:
the host's in a duel. Specification: [api.md](protocol/api.md#result-decision).

## Operator integrations

Signed webhooks (`[[webhooks]]`), an operator API for a Discord bot, a statistics site or a referee
(`[[operator_tokens]]`, paths under `/api/v1/operator/`), referee policies (`decider = "referee"`), badges, titles and
profiles (`[[badges]]`, `[[titles]]`): see [operators.md](protocol/operators.md),
[presentation.md](protocol/presentation.md) and the commented examples in `deploy/kalamar.example.toml`.
`[features] population = true` publishes `GET /api/v1/population`: how many players are online, in a match
and searching, as three numbers (no names), computed at most every 10 seconds.

## Results and what they guarantee

Each launcher uploads the game's own result file; the service compares them under the declared policy (reference
`standard`: more than half of the players must back a result — matching reports, and players who reached the match
and sent nothing; an error report never backs a result and voids it unless the record explains it; a strict
majority wins a disagreement; a 1v1 disagreement is void; ratings change only if each team confirmed the result with
a report of its own). One player alone cannot change a rating against players who report, nor win against a player
whose game reported a failure; any player can void a match. Players who **collude**, or a **modified game** that lies
to everyone, are out of reach of any cross-check: moderation is the remedy. See
[api.md](protocol/api.md#result-decision) ("What one participant can do alone") and
[overview.md](protocol/overview.md#32-what-the-service-can-know--and-what-it-cannot).
