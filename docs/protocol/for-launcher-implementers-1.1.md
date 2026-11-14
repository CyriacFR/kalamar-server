# From 1.0 to 1.1: what a launcher adds

A 1.0 launcher keeps working against a 1.1 server unchanged. To support 1.1, add the following, in any order; each
item is independent and gated by `features` (or by the match object itself).

## Must, to stay correct with 1.1 servers

1. **Unknown match states.** Treat `under_review` (and any unknown state) as "waiting": no launch, refetch on
   `match.updated`; show "waiting for a referee" for `under_review` ([operators.md](operators.md#3-pluggable-result-policies-including-a-human-referee)).
2. **Unknown origins.** `origin.kind = "activity"` (with `activity_id`): show the activity's title if known, play the
   match as any other.
3. **Send `Kalamar-Protocol: 1.1`** once you implement any of the items below.

## Should: capabilities and contract 2 ([match-contract-2.md](match-contract-2.md))

4. Send the extended `PUT /v1/me/game`: `contracts` (`[{"id": "lan-auto-match", "versions": [1, 2]}]` when you
   implement contract 2), `engine` (from the engine: rule keys, rounds, team limits, spectators; `loadout` from what
   you can apply), `stages` = the stages of the player's room list, `options` = the engine's `--list-match-options`
   output verbatim.
5. Contract 2 launch: `KLM_MATCH_RULES = game.rules` for **every** participant; `KLM_MATCH_ID = game.match_id`
   (may differ from `relay.room`); with `game.keep_open`, set `KLM_MATCH_CMD` to your command file and do not set
   `KLM_MATCH_EXIT`; use `{id}` in status and result paths.
6. Rounds: on `match.ready` whose `game.continue_from` is the round the running game just played, append `next
   id=<match_id> <rules>` (host) or `next id=<match_id>` (guest) to the command file instead of starting the game;
   append `quit` when the series ends or nothing continues in time.
7. Result files version 2: upload them verbatim (an `aborted` one too); when the engine wrote no result file, upload
   `{"version": 2, "match_id": ..., "error": <the status file's error code>}`.
8. Loadout: before launch apply `rules.loadout` through the save profile (`fixed`; for `allowed`/`denied` let the
   player pick a compliant owned item); report it in `POST /launch` `{"loadout": {...}}`; refuse with
   `launcher-loadout-unavailable` when impossible. Show `rules.loadout.summary`.
9. New errors: `409 incompatible_game` may carry `details.players[].missing` (show it); launcher error codes
   `launcher-loadout-unavailable`, `launcher-incompatible`.

## Should: generic activities ([activities.md](activities.md))

10. Area `activities`: list `GET /v1/activities` (title, kind, state, countdown, `join` button), open
    `GET /v1/activities/{id}` and render its view from the **closed component list**: `text`, `notice`, `fields`,
    `list`, `table`, `countdown`, `progress`, `bracket`, `series`, `match`, `user`, `form`, `actions`, `link`,
    `group`. Unknown component: its `fallback` text or nothing.
11. Actions: `POST /v1/activities/{id}/actions/{action}` with `{"values", "revision"}`; ask first when `confirm` is
    set; show `notice`; replace the view with the returned one; on `409 stale_view` refetch.
12. Events `activity.updated` (refetch the view if shown), `activity.notice` (toast), `series.updated`.
13. Safety: plain text only; links open only on click, with the host shown; no images; respect the size limits.
14. Icons: map the 24 icon names to your own icon set.

## Should: series ([series.md](series.md))

15. Area `series`: show the `series` card (sides, wins, games) from `GET /v1/series/{id}` and the match's `series`
    member ("game 2 of a best of 3").

## May: presentation ([presentation.md](presentation.md))

16. Areas `badges` and `profiles`: badges list, featured badge and title in `PATCH /v1/me`, `GET /v1/users/{id}/profile`
    rendered as a view; User objects may carry `badge` and `title`.
17. Ladders may carry `group` and `primary`.
18. 1.1.4, area `population`: `GET /v1/population` (no token) gives `online` and, when present, `in_match` and
    `searching`; show them on the server's page; fetch at most every 10 s ([api.md](api.md#population)).

## May: password accounts and clearer login errors (1.1.5, [api.md](api.md#password-accounts))

Nothing here is needed: a 1.1 launcher already draws the password forms from their fields (`string`, `secret`) and
sends them. Each item makes them nicer.

19. Providers with the same `group`: one card with tabs by `purpose` (`login` first, then `signup`, `reset`,
    `change`) instead of one card per form.
20. Fields: `autocomplete` to the input (`email` as an e-mail input, `current-password` / `new-password` for password
    managers, `one-time-code`); a show/hide button on `secret` fields; check `min_len` before sending.
21. Errors: show `text` when you have no text of your own for the code; translate `code_required` ("a code was sent:
    type it here") and focus the `code` field, `account_exists`, and `wrong_password` for a login ("wrong address or
    password", not "wrong room password").
22. Account page: when Me has `password`, a "change password" form calling `POST /v1/me/password`
    (`{"password", "new_password"}`; `204`, the other sessions end).

## May: the stage draft (1.1.6, [draft.md](draft.md))

Nothing here is needed: without it the match state `drafting` is "waiting" and the server plays your players' turns
(or tells them, with a requirement `draft`, that the operator wants a launcher that shows it).

23. Declare `"launcher": {"draft": [1]}` in `PUT /v1/me/game` once you render the draft.
24. Area `draft`: when the current match is `drafting` (or `pending_accept` with a `draft`), show the draft object as
    data: the teams and their players, the options with their state (banned in the color of the team that banned,
    picked highlighted, unavailable dimmed with their reason), whose turn it is, a countdown to
    `turns[current].deadline` against the server's clock, the turn log, the team's `votes` for a vote, the result.
25. Act with `POST /v1/matches/{id}/draft` (`{"turn", "choices"}`), only when `you.can_act`, with exactly the turn's
    `count` options of its `kind`; on `409 draft_turn` refetch the match; translate `not_your_turn`.
26. Treat `decline` during `drafting` as "leave the match" (ask first: a queue counts it as an offense), and
    `cancel_reason: left`.
27. Resume after a restart from `GET /v1/matches/current`: the draft object is the whole state. Unknown actions,
    reasons, deciders: text or nothing, never a blocked screen.

## May: the stands (1.1.7, [spectators.md](spectators.md))

Nothing here is needed: without it a launcher is never seated and never in a match with spectators.

28. Declare `"engine": {"spectator_seats": {"max", "modes", "verified_modes", "host"}}` (from the engine's `spectators`)
    and `"launcher": {"spectate": [1]}` once you run the seat; add `spectators_wait` to `rule_keys`.
29. Rooms: draw the stands from the room object (members with `team: spectator`, `spectators.seats`/`taken`/`who`),
    a button to sit there or come back to play (`PUT .../members/{id}` with `team`), `seat: spectator` on join.
30. When `you.team` is `spectator`: `POST .../launch` **before** starting the game; on `409 match_started` (or a
    player already `loading-stage`) do not start it and upload `launcher-spectator-late`; impose no loadout; upload the
    `spectated` result file; show "you watch" and the game's spectator controls with the player's own keys; no "your
    turn" sound; the draft read-only.
31. Show every match's `spectators` to its players; `GET /v1/spectate` as a list of watchable matches and series.
32. Translate `match_started`, `spectators_full`, `spectators_not_allowed`, `spectators_refused`, the missing
    items `launcher:spectate`, `spectators`, `spectators:<n>`, `spectator_mode:<mode>`, `spectator_host`.

## May: the counters of a result (1.1.8, [api.md](api.md#result-decision))

Nothing here is needed: the outcome keeps its members (`decision` `agreed`, verdicts `agreed`), the record is one
agreeing report as the game wrote it, and `summary` says when the counters were flagged.

33. Upload the result file verbatim, as before: never "fix" a counter, never recompute a score.
34. With `outcome.stats_check.flagged`, MAY show (advanced view) which counters differ between the reports
    (`gaps`: `field`, `team` or `user_id`, `values`); the result stands either way.

## May: several relays and an offer that changes (1.1.9, [relay.md](relay.md#41-several-relays-119))

Nothing here is needed: a launcher that ignores `meta.relays` plays on the relay of its match, as before.

35. On `meta.changed` (a 1.0 event; 1.1.9 gives it a `revision`), read `GET /v1/meta` again and redraw what comes from
    it (queues, the room form, activities, ladders): operators now change their offer while the server runs. A
    `revision` is opaque: compare it, never compute with it (it grows, also across restarts of the server).
36. When `meta.relays` lists two relays or more, MAY time one relay `HELLO` to each (resolve the name yourself, IPv4;
    2 s without the `CHALLENGE` is `null`) and send `"launcher": {"relay_rtt_ms": {"<id>": ms or null, ...}}` with
    `PUT /v1/me/game`: at most once per relay per 10 minutes, never while the game runs, only for the server the
    player is logged in to. Never choose a relay: start the game on `game.relay` of the match.

## Should: a player who leaves, and free after the report (1.1.10, 1.1.11, [leaving.md](leaving.md))

37. When the game is closed during a match, report `left-by-player` (not a crash). Started again during a match the
    game had already started, never start that match again: upload what the earlier game left (its result file, else
    its status file's error, else `launcher-crashed`).
38. Copy the engine's `in_play` from the status file into a `left-by-player` report.
39. With `you.free: true` in a match's view (servers that declare `meta.matches.free_after_report`), show the match as
    decided for the player's own part and let the player search again.
40. Show a decision `forfeit` with `outcome.forfeit` (`reason`, `team`, `players`); unknown to a launcher, it is a
    counted result with a winner, and `summary` explains it.

## Must: what `build` may hold (1.1.12, [api.md](api.md#put-v1megame--auth))

41. `build` in `PUT /v1/me/game` names a build, the same for every copy of it (a version, a commit, a hash of the
    build's contents), or is omitted: never a value made of local file sizes, times or paths.

## May: relays that say they are full (1.1.13, [relay.md](relay.md#43-relays-that-announce-their-load-1113))

42. Show `503 relay_capacity` on a room start as "every game server is full", with its `retry_after_s`.
43. Translate the engine errors `relay-full`, `relay-room-full`, `relay-source-limit` and `relay-source-full`
    ([match-contract-2.md](match-contract-2.md#7-reports-of-contract-2)); report them as any engine error.

## Never

Execute anything from a server, load an image or a URL by itself, disable TLS, let the game contact anything but the
match's relay address, show money or a shop.
