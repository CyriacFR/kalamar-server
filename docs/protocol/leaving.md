# Kalamar community protocol 1.1 — a player who leaves a match

What happens when a player crashes, closes the game or loses the connection in the middle of a match: what the game
does, what the launchers report, what a server decides, and what it can choose. Sections 3 and 4 are normative (1.1.10
and 1.1.11, also in [api.md](api.md#lifecycle) and [api.md](api.md#result-decision)); the rest describes the
supported game build's behavior and an operator's options.

## 1. What the game does

Observed with two copies of the supported game build in matches of two players (with and without a spectator):

- **In a 1v1 no game writes a record once a player is gone**: the game itself ends the match for everyone, there is
  no host migration and no `deemed_result`. `disconnect: forfeit` needs a record that shows the player
  `disconnected`, and witnesses only choose between players' records (they never make one count alone): neither can
  decide such a match. The policy member `leave` exists for it (section 4).
- **An interruption of the network longer than a few seconds ends the match** for both players, even when it comes
  back.
- **A game cannot join a match in progress** (spectators included): nobody can come back or replace a player.
- A game killed or closed once the result screens began changes nothing: both reports were already sent.
- When a player's computer goes down with its launcher, only the other player's `connection-lost` arrives: the match
  is void at the result deadline, and section 3 keeps the other player from being held until then.
- Not verified for these departures: more than two players (whether the others' games go on and record the leaver
  as `disconnected`), two machines, every mode.

## 2. What launchers report

| Situation | Report |
|---|---|
| the game's menu, *Leave match*, in the match while it is being played | `left-by-player` with `"in_play": true` (the engine's status, copied by the launcher) |
| the game closed during the match (the window's close button, the menu's entry that quits) | as *Leave match*, written by the engine before it exits; an engine that does not write `in_play`: `left-by-player` without it |
| a departure while the stage loads, or with another player already silent in a team match | `left-by-player` with `"in_play": false` |
| a departure after the session stopped (the other game gone first) | `connection-lost` (the match was lost before the player left) |
| the launcher's stop (the game killed by its launcher) | `left-by-player` without `in_play` |
| the game crashed (killed) | `launcher-crashed` |
| the other player vanished, or the connection broke | the engine's `connection-lost`: 15 s after the last traffic in a match of two players, 45 s with more (`KLM_MATCH_SILENT`) |
| a launcher started again during a match its game had started | no new game: the earlier game's result file if it wrote one, else its status file's error (with its `in_play`), else `launcher-crashed`, at once (after waiting for that game when it still runs) |

`left-by-player` and `launcher-cancelled` count as leaving a queue match (offense `left`, [api.md](api.md#penalties));
a crash and a lost connection do not.

## 3. Free after the report (1.1.10)

A server that declares `meta.matches.free_after_report` stops holding a participant once its terminal report is in:
the match waits for the others' reports as before, the decision still applies to it, and it may search again at once
([api.md](api.md#lifecycle)). Without it, the survivor of a 1v1 whose opponent's computer went down waited for the
result deadline. Launchers show a match whose view says `you.free: true` as decided for the player's own part and let
the player search; a launcher that does not know the member keeps showing the match until the decision, as before.

## 4. Forfeit (1.1.11)

A player who is losing could leave to avoid a rated loss, since such a match is void by default. A result policy's
`leave` member (`void` by default, `forfeit`, `forfeit_unrated`) lets an operator make the voluntary leaver lose,
while a crash or a lost connection stays void: no report can tell from which side such a failure came.

### 4.1 The rule

Under `leave` `forfeit` or `forfeit_unrated`, a match that has **no valid result** (step 3 of the
[decision](api.md#result-decision): in a 1v1 always, the other game stops with the session) is a forfeit of one team
(`completed`, decision `forfeit`, the other team the winner, `outcome.forfeit` `{"reason": "left", "team", "players"}`)
when:

1. a participant's **own** report is `left-by-player` with `"in_play": true`, and its mirrored status reached
   `in-match`;
2. every such participant is in the same team;
3. no other participant's terminal report was received before the first of them (the departure is the first thing
   that went wrong);
4. a participant of the other team reached `in-match`.

Else the match is decided as before (void). The rating: as any result under `forfeit` (a win and a loss), none under
`forfeit_unrated` (`unrated_reason: forfeit`) or `witness: required` (`no_witness`: a witness cannot confirm a match
without a record); `rated_requires_both_teams` does not apply (the losing team's own report is the proof, and the
winners reached the match). The offense `left` applies on top, as for any departure from a queue match. A match of two
players that one left in play is decided `grace_s` after the departure when the other never reports (its launcher went
away too); with more players the others' games may play on and record the match, so they are waited for as before.

### 4.2 Case by case

| Case | Decision |
|---|---|
| 1v1, A leaves in play (*Leave match* or its window closed), B's game stops with `connection-lost` (or `match-aborted`) | **B wins by forfeit**, rated; A gets the offense `left`; both free (A at the decision, B at its report) |
| 1v1, A leaves in play, B never reports (its launcher gone) | B wins by forfeit `grace_s` (120 s) after A's report; A held until then |
| 1v1, A leaves in play, B reported a crash (`launcher-crashed`) or a lost connection **before** | void: the match was already lost when A left; A's offense `left` stays |
| 1v1, B's game vanished and A left afterwards | the engine reports `connection-lost` for A: void, no offense |
| both players report a departure in play | void, both get the offense |
| A's game crashed (`launcher-crashed`), or A's network cut (`connection-lost` from both) | void, no offense: a crash or a cut is never a forfeit |
| A leaves while the stage loads (`in_play: false`, or its status never reached `in-match`) | void and the offense `left` (the match had not begun) |
| A's launcher or engine does not write `in_play` | void and the offense |
| a team match, A1 leaves in play and every other game stops without a record | A's team loses by forfeit |
| a team match, A1 leaves and the other games go on and agree on a record that shows A1 `disconnected` | **that record counts** (its winner, with `disconnect` deciding what a `disconnected` player changes) when a teammate of A1 reported it (step 5; else void); no forfeit by departure |
| a team match whose records show A1 playing to the end | void (A1's error contradicts them, step 5) |
| a series (best of N) | the game is lost by forfeit like any decided game; the series goes on to its next game (a leaver who does not launch it is a no-show there) |
| an unrated match | the forfeit is shown (counted, a winner), no rating changes |
| a spectator leaves | never a forfeit: a spectator's report is a witness's |

### 4.3 What a player can still do

- Only its own report makes a team lose, and only its own team: a report can harm its author's team, never the other's.
  A forged `in_play: true` costs the liar (and, in a team match, its teammates, as leaving for real would).
- A record that shows a player of the **other** team gone never explains that player's error, so the other way
  round is closed too: a cheater who quits and uploads a record showing the opponent `disconnected` (to win by the
  record, or by `disconnect: forfeit`) gets a void match against an opponent whose game reported the failure
  ([api.md](api.md#result-decision), step 5).
- A departure that looks like a crash or a lost connection stays void with no offense: nothing in the reports tells
  it from a real failure, and a forfeit on it would make honest players lose for their opponents' network. A server
  MAY watch who crashes or loses the connection unusually often (not part of the protocol).
- Two players who agree (one leaves on purpose to give the other a win) trade a rated result as they could by playing
  badly; `witness: required` makes such a forfeit unrated.

## 5. What an operator can choose (not part of the protocol)

| Choice | Options | Recommendation |
|---|---|---|
| A match that one player left in play | policy `leave`: `void` (the default), `forfeit`, `forfeit_unrated` | `forfeit` on rated queues (the reference presets `competitive` and `tournament` and the example configuration do); `void` where nothing is at stake |
| A departure early in the match | the same as any departure | a departure while the stage loads is never a forfeit (the offense only) |
| The leaver | the queue offense `left` (configurable: `penalties.counted`) | keep `left` counted; crashes are not counted |
| Replacing a player | not possible: the game cannot let anyone in after the start | — |
