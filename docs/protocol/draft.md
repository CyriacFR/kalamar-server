# Kalamar community protocol 1.1 — the stage draft ("pick and ban")

Between "match found" and the launch, teams may ban and pick the stages (and the modes) of their match, turn by turn.
The **server** runs the whole phase: it declares the options, the turns and who may act, resolves every action,
plays the turns of teams that cannot or do not act, and writes the result into the match's rules. The **launcher**
shows the state it is sent and posts one player's action. Area id: `draft` (in `features`); launcher capability:
`launcher.draft` in `PUT /v1/me/game`. Everything here is additive and optional: a 1.1.x launcher that knows none of
it still plays every match ([section 7](#7-launchers-without-the-capability)).

The draft is generic: a server may invent any sequence of bans, picks and draws over any pool of game identifiers,
decided by a captain, by a vote or by anyone of a team, simultaneous or in turn, once per match or once per series —
without a launcher update. What stays fixed is what the game can do: one mode and one stage per match, set through
the match contract ([match-contract-2.md](match-contract-2.md)); the launcher never needs to understand a format.

## 1. Lifecycle

```
 queue:   pending_accept --all accept--> drafting --draft done--> ready --> running --> completed | void
 room / series / activity:               drafting --draft done--> ready --> ...
                                            |  a player declines (abandons)
                                            v
                                         cancelled (cancel_reason: left)
```

- `drafting` is a new [match state](api.md#lifecycle). The match object has no `game` block yet (as in
  `pending_accept`) and carries a `draft` member. A launcher that does not know the state treats it as waiting
  (1.1 launchers: "unknown state: no launch, refetch on `match.updated`").
- A match of a queue shows its `draft` (state `waiting`) already in `pending_accept`, so players see the format
  before they answer. The draft starts when everyone accepted; a room's, a series' or an activity's match starts in
  `drafting` at once.
- When the draft is over the server writes the result into `rules.stage` (and `rules.mode` when modes were drafted),
  builds the `game` block with them, and the match becomes `ready` (`match.ready`) with its usual launch deadline.
- **The phase never waits indefinitely**: every turn has a deadline; when it passes the server acts
  ([section 5](#5-what-the-server-does-when-nobody-acts)). The longest draft lasts the sum of its turns.
- A player abandons the draft with `POST /v1/matches/{id}/decline` (the same endpoint as the accept phase): the match
  is `cancelled` with `cancel_reason: "left"`. A server SHOULD treat it like a decline: the leaver gets the offense
  `left` (queue matches), the others search again with their priority, a room opens again, a series replays the game.

## 2. Declarations

**Area.** A server that may draft declares `draft` in `features` of `GET /v1/meta` and of the discovery document.

**Queues.** A queue whose matches are drafted carries `draft` (else `null`):

```json
"draft": {"id": "veto", "label": {"en": "Stage veto"}, "summary": {"en": "Each team bans one stage, then a stage is
drawn among the rest."}, "required": false, "turn_s": 20, "pool": {"stages": [], "modes": []}}
```

`label` and `summary` are shown with the queue; `required: true` means only launchers that declare the capability
may enter (the queue's `requirements` then list `{"id": "draft", "met": ...}` and `POST .../tickets` answers `403
not_eligible` with `details.requirements`); `pool` is the operator's fixed pool, empty when the pool comes from the
queue's own pool or from the players' games.

**Rooms.** A server MAY offer drafts in the room form with the reserved option key `draft` (type `enum`, nullable):
each value is a draft as above (`id`, `label`, `summary`, `required`, `turn_s`, `pool`), `null` means no draft. With a
draft, the draft decides the stage (and the mode if it drafts modes); the room's `stage` option is not used. A room
whose draft is `required` refuses to start when a member's launcher cannot show it: `409 incompatible_game` with
`details.players[].missing: ["launcher:draft"]`.

**Series and activities.** Their matches carry a `draft` like any match; a series object MAY carry `draft`:
`{"preset": "<id>", "stages": [<order of a once-per-series draft>], "played": [<stages of the decided games>]}`.

## 3. The draft object

In every participant view of the match (`GET /v1/matches/{id}`, `GET /v1/matches/current`, `GET /v1/state`, every
`match.*` event), and in the public view of an ended match (without `you` and without votes):

```json
"draft": {
  "version": 1,
  "state": "running",
  "preset": "veto",
  "title": {"en": "Stage veto"},
  "summary": {"en": "Each team bans one stage, then a stage is drawn among the rest."},
  "decider": "captain",
  "tiebreak": "captain",
  "turn_s": 20,
  "first": "bravo",
  "options": [
    {"id": "<stage id>", "kind": "stage", "state": "banned", "by": "bravo", "turn": 0, "order": null, "reason": null},
    {"id": "<stage id>", "kind": "stage", "state": "available", "by": null, "turn": null, "order": null, "reason": null},
    {"id": "<stage id>", "kind": "stage", "state": "unavailable", "by": null, "turn": null, "order": null, "reason": "game"}
  ],
  "turns": [
    {"index": 0, "action": "ban", "kind": "stage", "teams": ["bravo"], "count": 1, "state": "done",
     "started_at": "2026-11-11T20:15:03Z", "deadline": null, "ended_at": "2026-11-11T20:15:09Z",
     "choices": [{"team": "bravo", "ids": ["<stage id>"], "by": "players"}], "acted": []},
    {"index": 1, "action": "ban", "kind": "stage", "teams": ["alpha"], "count": 1, "state": "current",
     "started_at": "2026-11-11T20:15:09Z", "deadline": "2026-11-11T20:15:29Z", "ended_at": null,
     "choices": [], "acted": []},
    {"index": 2, "action": "random", "kind": "stage", "teams": [], "count": 1, "state": "pending",
     "started_at": null, "deadline": null, "ended_at": null, "choices": [], "acted": []}
  ],
  "current": 1,
  "teams": {
    "alpha": {"players": ["u_a", "u_c"], "actors": ["u_a"], "captain": "u_a", "auto": false},
    "bravo": {"players": ["u_b", "u_d"], "actors": ["u_b"], "captain": "u_b", "auto": false}
  },
  "you": {"team": "alpha", "role": "captain", "can_act": true, "choice": null},
  "result": {"mode": null, "stage": null, "stages": []}
}
```

| Member | Meaning |
|---|---|
| `version` | 1. A launcher that does not know the version shows the match as waiting and does not act |
| `state` | `waiting` (planned, the accept phase runs), `running`, `done`, `cancelled`. Unknown: `running` |
| `title`, `summary` | localized text: the format's name and one sentence explaining it |
| `decider` | how a team acts: `captain` (one player), `vote` (every player of the team votes), `any` (the first player of the team to answer). Informative: `you.can_act` says who may act. Unknown: show nothing about it |
| `tiebreak` | ties of a vote: `captain` or `random` (informative) |
| `turn_s` | the time of a turn (informative: `turns[].deadline` is authoritative) |
| `first` | the team that got the role "first" (by draw, rating, or the previous game), or `null` |
| `options[]` | every option, in the order to show: `id` a game identifier (resolved from the player's own game data, as everywhere), `kind` `stage` or `mode`, `state` `available`, `banned`, `picked`, `unavailable` (unknown: `unavailable`), `by` the team that banned or picked it (`null` for the server's draw), `turn` the turn that did, `order` the order of picks (1 is the match's), `reason` why unavailable: `game` (a player's game cannot load it), `played` (already played in the series), `combination` (not offered with the mode or stage chosen); unknown reasons are shown as "unavailable" |
| `turns[]` | every turn in order. `action` `ban`, `pick`, `random` (the server draws `count` options), `last` (the only option left is played); unknown: show `action` as text. `teams`: the acting teams (two for a simultaneous turn, none for `random`/`last`); `count`: options each acting team chooses; `state` `pending`, `current`, `done`, `skipped` (nothing left to choose); `choices`: what was chosen, by team (`team: null` for the server), with `by`: `players`, `vote`, `timeout` (the time ran out), `server` (nobody of the team can act, or a draw); `acted`: the teams that already chose in the current turn (their choice stays hidden until the turn ends) |
| `current` | index of the current turn, or `null` |
| `teams` | per team: `players` (user ids), `actors` (who may act for it now), `captain` (or `null`), `auto` (no launcher of the team can draft: the server plays its turns). With `decider: "vote"`, the caller's **own** team also has `votes`: `[{"user_id", "ids"}]` for the current turn |
| `you` | the caller's part: `team`, `role` (`captain`, `voter`, `any`, `none`), `can_act` (the caller may act on the current turn now), `choice` (the caller's vote, or the team's hidden choice, on the current turn) |
| `result` | when done: `mode` and `stage` (or `null`: the origin's mode, or the game's choice of stage), `stages` (all picks in order for a once-per-series draft) |

The result is part of the match for good: history, operators and webhooks see the draft of every match.

**Spectators** (1.1.7, [spectators.md](spectators.md#4-the-match-object)) see the draft read-only: `you` is `{"team": "spectator",
"role": "none", "can_act": false, "choice": null}`, no team's `votes`, and a simultaneous turn's choices stay hidden until both
teams chose. Their `POST .../draft` answers `403 not_your_turn`.

## 4. Acting

### `POST /v1/matches/{match_id}/draft` — *auth*

Body: `{"turn": 1, "choices": ["<id>"]}` — the index of the turn acted on and exactly `count` distinct ids of
available options of the turn's `kind`. Answer `200`: the match (participant view).

| Error | When |
|---|---|
| `404 not_found` | not a participant |
| `409 match_state` | the match has no draft, or it is over |
| `409 draft_turn` | `turn` is not the current turn, or the caller's team already chose in it (stale view: refetch) |
| `403 not_your_turn` | the turn is the other team's, or the caller may not act for the team (another captain, a launcher without the capability); `text` explains |
| `422 validation_failed` | `/choices`: wrong count, an unknown, unavailable or repeated id |
| `429 rate_limited` | too many actions (the reference: 60 a minute per player) |

Rules (normative for servers):

- Only a player the turn allows acts, and only during that turn; a team's choice is final once made (captain, any)
  or once its vote is settled.
- **Idempotent**: an identical action repeated (same turn, same ids) answers `200` without change, including after
  the turn ended.
- **Votes** (`decider: "vote"`): each actor of the team votes `count` ids and may change the vote while the team has
  not settled. The team settles when every actor voted or when exactly `count` ids have a strict majority of the
  actors; the tally keeps the ids with the most votes, ties broken by `tiebreak`. At the deadline the votes cast
  decide; with none, [section 5](#5-what-the-server-does-when-nobody-acts) applies.
- **Nothing is revealed early**: votes in progress are shown only to their own team; in a simultaneous turn a team's
  choice is hidden from the other team until both chose (`acted` only says that it chose). A server SHOULD send a
  vote's progress only to the voter's team.
- A ban never takes the last available option of its kind: a turn asking for more gets fewer (`count` shows the
  count in effect), or is `skipped`.
- In a simultaneous `pick`, an option both teams pick goes to the first team listed; the other gets a draw.

### Events

Every change is pushed as `match.updated` (the full participant view, per player); the end of the draft as
`match.ready`; an abandoned draft as `match.cancelled`. A launcher that reconnects reads the match back from `GET
/v1/matches/current` (or `GET /v1/state`): the object is the whole state, nothing else is needed to resume.

## 5. What the server does when nobody acts

- When a turn's `deadline` passes, the server acts for every acting team that has not: the votes cast if any, else
  the preset's rule — a random choice among what remains, or nothing (a lost ban; a missing pick is drawn at the
  end). The choice shows `by: "timeout"`.
- A team where **no** launcher declared the capability has its turns played at once by the server (`by: "server"`,
  `teams.<team>.auto: true`); its players wait without a deadline of their own.
- With a captain, when the captain's launcher is not connected at the start of a turn, the next connected player of
  the team who can draft acts for that turn.
- A server MAY count a timed-out turn as an offense `draft_timeout` of the players who could have acted, through its
  declared [penalties](api.md#penalties) (`counted`); the reference counts it for queue matches only, and only when
  the operator lists it.
- When every turn is played and a kind has no pick, the server draws one among what remains (a last `random` turn
  shows it).

## 6. Pools and capabilities

- Options are game identifiers. A server drafts stages only for a match of contract version 2 (every participant
  declared it: launchers tell the game a stage only through that contract) whose host's engine accepts the rule key
  `stage`; modes can be drafted with either version. It offers only stages the designated host's game can be told
  (`can_select_stage`) and every participant's game can load: with `engine.stage_list: "all"` the stages each player
  declared, with `host_save` the host's list ([match-contract-2.md](match-contract-2.md#3-assigning-only-feasible-contracts)).
  A stage outside them is `unavailable` with `reason: "game"` (or left out), so a draft never ends on a stage that
  would stop with `stage-unavailable`. Modes likewise from each player's declared `modes`.
- In a series, stages already played MAY be `unavailable` with `reason: "played"`.
- When nothing is left to draft (no option a player's game can load), the match simply has no draft.

## 7. Launchers without the capability

A launcher declares that it shows drafts in `PUT /v1/me/game`:

```json
"launcher": {"draft": [1]}
```

(the versions of the draft object it renders). Without it:

- by default the server plays that player's team's turns (if nobody in the team can draft) and the match goes on:
  a 1.1.x launcher sees `drafting` as "waiting", then `match.ready` as usual;
- an operator who wants every player to see the draft marks it `required`: such a player cannot enter the queue
  (`403 not_eligible`, requirement `draft`), start such a room (`409 incompatible_game`, missing `launcher:draft`)
  or join such an activity (`403 not_eligible`), with a reason the launcher shows.

## 8. For launcher implementers

1. Declare `"launcher": {"draft": [1]}` once you render the draft object (and keep sending it after each login and
   each new event stream).
2. Map `POST /v1/matches/{id}/draft` to the area `draft`, `decline` during `drafting` to "leave".
3. When the current match is `drafting` (or `pending_accept` with a draft in state `waiting`), show the draft: the
   two teams and their players, the options with their state (banned ones marked with the color of the team that
   banned them, picked ones highlighted, unavailable ones dimmed with their reason), whose turn it is, the countdown
   to `turns[current].deadline` computed against the server's clock (`GET /v1/meta` `time`), the turn log, and the
   result. Show an action only when `you.can_act`; select exactly `turns[current].count` options of its `kind`.
4. With `decider: "vote"`, show the team's `votes` and the player's own `choice`; with `captain`, name the captain.
5. Render unknown actions, reasons and deciders as text or nothing; never block the match: on any doubt, show the
   match as waiting — the server always ends the draft.
6. A sound or a notification when `you.can_act` turns true is welcome; nothing else is required.

## 9. For server writers

- Keep the draft in the match: it is the single source of truth (the reference stores it as JSON with the match and
  restores it after a restart; deadlines keep running).
- Validate every action as section 4 says; answer the full participant view so a launcher can draw at once.
- Decide the "first" team as your format says (a draw, ratings, the previous game) and resolve team names in the
  turns before you send them: launchers only see `alpha` and `bravo`.
- Write the result into `rules` and the game's rules string (`stage=<id>`), then make the match `ready`.
- The conformance suite (area `draft`) checks: the declaration (`draft.declaration`); a whole draft played by two
  launchers that declare the capability, an action refused to the other team (`403 not_your_turn`) and on another
  turn (`409 draft_turn`), the draft's invariants at every step, the result in `rules` and in the game block
  (`draft.play`); a draft nobody plays ending by itself (`draft.timeout`, test clock or a short `turn_s`);
  launchers without the capability (`draft.no-capability`: played for, or refused when `required`); a player who
  leaves (`draft.leave`).

## 10. The reference server's configuration

Presets in `[[drafts]]`, named by queues (`draft = "<id>"`), rooms (`rooms.drafts`) and activities (`draft`, for
every game of their series). Turns are tokens:

| Token | Meaning |
|---|---|
| `ban:<team>:<n>` | the team bans `n` options |
| `pick:<team>:<n>` | the team picks `n` options (the first pick is the match's; more picks give a series' order) |
| `random[:<n>]` | the server draws `n` options among what remains |
| `last` | the only option left is played (a draw if several remain) |
| `strike` | alternate single bans from `first` until one option is left, then `last` |
| `...@mode` | the same for modes (`@stage` is the default) |

Teams: `first`, `second`, `alpha`, `bravo`, `both` (simultaneous, hidden), `winner`, `loser` (of the previous game of
a series; `second` and `first` for the first game). Other settings: `first_game_turns`, `first` (`random`,
`higher_rated`, `lower_rated`, `loser`, `winner`, `alpha`, `bravo`), `decider`, `tiebreak`, `turn_s`, `timeout`
(`random` or `skip`), `stages`, `modes`, `series` (`per_game` or `once`), `exclude_played`, `required`. The example
configuration ([deploy/kalamar.example.toml](../../deploy/kalamar.example.toml)) ships the default — one ban per team,
then a random stage among the rest, 20 s a turn, captains — and commented formats:

```toml
[[drafts]]                       # the default: a veto
id = "veto"
label = { en = "Stage veto" }
turns = ["ban:first:1", "ban:second:1", "random"]
turn_s = 20

[[drafts]]                       # stage strike over a short pool
id = "strike"
label = { en = "Stage strike" }
turns = ["strike"]
first = "lower_rated"
stages = ["<stage id>", "<stage id>", "<stage id>", "<stage id>", "<stage id>"]

[[drafts]]                       # blind bans by team vote
id = "blind"
label = { en = "Blind bans" }
turns = ["ban:both:2", "random"]
decider = "vote"

[[drafts]]                       # the mode, then the stage
id = "mode-then-stage"
label = { en = "Mode, then stage" }
turns = ["ban:first:1@mode", "ban:second:1@mode", "random@mode", "ban:second:1", "pick:first:1"]

[[drafts]]                       # a series where the loser picks
id = "loser-picks"
label = { en = "The loser picks" }
first_game_turns = ["ban:first:1", "ban:second:1", "random"]
turns = ["ban:winner:1", "pick:loser:1"]

[[drafts]]                       # a series' stage order, drafted once
id = "series-order"
label = { en = "Stage order" }
turns = ["ban:first:1", "ban:second:1", "pick:first:1", "pick:second:1", "last"]
series = "once"
```

The reference also sends the webhook `match.drafted` (the public view of the match) when a draft ends, and limits
draft actions per player (`rate_limits.draft_actions_per_min`).

## 11. What an operator can invent, and what stays fixed

Without any launcher change an operator can choose: the pools (stages, modes, any subset of the players' games), the
number and order of bans, picks and draws, simultaneous hidden turns, who starts (a draw, ratings, the previous
game), who decides in a team (a captain, a vote, anyone), the time per turn and what happens when it runs out,
whether modes are drafted before or after stages or not at all, once per match or once per series, played stages
excluded, and whether the draft is required. A server written from scratch may go further — any rule that ends in a
list of options with states and a list of turns is renderable — as long as it fills the object of section 3.

Fixed by the game and the protocol: one mode and one stage per match, game identifiers only (no names, no images
from the server), two teams, turns acted by teams (`alpha`, `bravo`), the actions `ban`, `pick`, `random`, `last`
(others are shown as text), and the server as the only judge.
