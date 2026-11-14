# Kalamar community protocol 1.1 — activities and views

Version 1.1 lets a server offer **anything that leads to matches** — a single-elimination tournament, a league with
stage pick and ban, a best-of-five final, a daily challenge, a one-against-three handicap, king of the hill, clan
scrims, a weekend event — without any change to the launcher. The server describes each activity as **views**:
declarative screens made of typed **data components**. The launcher renders them generically, sends the player's
**actions** back to the server, and refreshes on events. Area id: `activities` (in `features`).

Everything here is additive: a 1.0 launcher ignores `activities` and still plays the matches an activity creates,
because they are ordinary [match objects](api.md#match-object) (with `origin.kind = "activity"`).

## 1. Safety rules (normative, both sides)

A view is **data, never code**:

- no HTML, no markdown, no script, no style sheet, no template language: every text is plain text shown as is;
- no URL is ever fetched automatically. The only URLs are in `link` components (and `rules.url`, `privacy_url`):
  `https://` only, shown with their host name, opened in the system browser **only** when the player clicks;
- **no images in 1.1.** Icons are names from the closed list of [section 5](#5-icons-and-colors), drawn by the
  launcher itself. Rationale: images fetched from a server leak players' addresses to whoever hosts them, let any
  server put arbitrary content in front of players, and cost bandwidth; a later version may add images through an
  explicitly bounded mechanism (size, format, hash-addressed, fetched only from the API host);
- every size is bounded ([section 6](#6-limits)); a launcher that receives a view over the limits shows an error card
  instead of it;
- actions only call the server's own API (`POST /v1/activities/{id}/actions/{action}`); a view cannot make the
  launcher call anything else, start the game, change a setting or read a file.

## 2. Activities

### `GET /v1/activities` — *auth*

```json
{"items": [{
  "id": "spring-cup",
  "kind": {"en": "Tournament"},
  "title": {"en": "Spring cup"},
  "subtitle": {"en": "8 players, single elimination, best of 3"},
  "icon": "trophy",
  "color": "#3A7BD5",
  "order": 10,
  "state": "open",
  "starts_at": "2026-11-14T18:00:00Z",
  "participants": {"count": 5, "capacity": 8},
  "join": {"action": "register", "label": {"en": "Register"}},
  "you": {"joined": false},
  "revision": 12
}]}
```

| Member | Meaning |
|---|---|
| `id` | the activity's id |
| `kind` | localized text naming the kind, for display only (the launcher never branches on it) |
| `title`, `subtitle`, `icon`, `color`, `order` | presentation (`icon` from [section 5](#5-icons-and-colors); lower `order` first) |
| `state` | `upcoming`, `open` (players can join), `running`, `finished`, `cancelled`. Unknown: show as `running` |
| `starts_at`, `ends_at` | optional times (a countdown) |
| `participants` | optional `count` and `capacity` |
| `join` | optional: the main participation action, for a "join" button in lists (`action` is an action id of the view) |
| `you` | `joined` (the caller takes part) |
| `revision` | increases at every change of the activity's view |
| `spectators` | 1.1.7, optional: places in the stands of the activity's series (`seats`, `who`, `label`), or `null` ([spectators.md](spectators.md#32-queues-activities-series)) |

### `GET /v1/activities/{activity_id}` — *auth*

The activity's current **view** for the caller (it may differ per player: own score, own actions).

```json
{
  "id": "spring-cup",
  "revision": 12,
  "title": {"en": "Spring cup"},
  "state": "running",
  "sections": [ {"type": "text", "text": {"en": "Single elimination ..."}}, {"type": "bracket", "rounds": [ ... ]} ],
  "actions": [ {"id": "check_in", "label": {"en": "Check in"}, "style": "primary", "enabled": true} ]
}
```

`sections` are components ([section 3](#3-components)) shown top to bottom; `actions` are buttons shown with the
view (components may also carry their own actions).

### `POST /v1/activities/{activity_id}/actions/{action_id}` — *auth*

Body: `{"values": {<form values>}, "revision": 12}` (`values` only for actions bound to a form; `revision` is the
view the player acted on, optional). Answer `200`:

```json
{"view": { ... the new view ... }, "notice": {"level": "success", "text": {"en": "Registered."}},
 "match_id": null, "series_id": null}
```

Every member is optional: `view` (show it), `notice` (a transient message), `match_id` / `series_id` (the action
created or joined one: the launcher shows it). Errors: `404 not_found` (activity or action), `409 stale_view` (the
`revision` is out of date: refetch), `409 action_unavailable` (the action is disabled now; `message` says why),
`422 validation_failed` (form values, with `field`), `403 not_eligible`, and every error of
[api.md](api.md#error-codes).

### Events

| `type` | `data` | When |
|---|---|---|
| `activity.updated` | `{"activity_id", "revision"}` | the view changed for this player: refetch it when shown |
| `activity.notice` | `{"activity_id", "notice"}` | a message for this player (`notice` as above) |
| `series.updated` | [Series](series.md) | a series the player is in changed |

Matches of an activity arrive as usual (`match.found`, `match.ready`, ...), with `origin: {"kind": "activity",
"activity_id"}` and, inside a series, `series` ([series.md](series.md)).

## 3. Components

Every component is an object with `type` and optional `id` (stable within the view) and `x-` members. Fields named
`text`, `title`, `label`, `subtitle` are [localized text](api.md#localized-text).

| `type` | Members | Rendering |
|---|---|---|
| `text` | `text`, `style`? (`body` by default, `heading`, `caption`, `muted`) | a paragraph |
| `notice` | `level` (`info`, `success`, `warning`, `error`), `text` | a colored box |
| `fields` | `items`: `[{"label", "value", "hint"?}]` (`value`: localized text, string or number) | a key-value list |
| `list` | `items`: `[{"title", "subtitle"?, "trailing"?, "user"?: User, "icon"?, "action"?: Action}]`, `empty`? (text) | a list; `trailing` is a short value on the right (localized text, string or number) |
| `table` | `columns`: `[{"key", "label", "type"}]` (types of [leaderboards](api.md#leaderboard)), `rows`: `[{"cells", "user_id"?, "highlight"?}]`, `caption`? | a table |
| `countdown` | `label`, `until` (time) | a live countdown; the launcher refetches the view at zero |
| `progress` | `label`, `value`, `max` | a bar |
| `bracket` | `rounds`: `[{"title", "matches": [{"id", "slots": [{"label", "user"?, "score"?, "winner": bool}], "state", "match_id"?, "series_id"?}]}]` | an elimination tree (the next round's match *k* is fed by matches 2*k* and 2*k*+1 of this round); a match's `state` is `waiting`, `running` or `completed` (unknown: `waiting`) |
| `series` | `series_id`, `title`? | the [series](series.md) card (score, games), fetched by id |
| `match` | `match_id`, `title`? | a match card (the launcher's own match view) |
| `user` | `user`: User, `subtitle`?, `badges`?: [Badge](presentation.md#1-badges-and-titles) | a player card |
| `form` | `id`, `fields`: [Field](api.md#common-objects)`[]`, `submit`: Action | a form; submitting calls the action with `values` |
| `actions` | `items`: Action`[]` | a row of buttons |
| `link` | `label`, `url` (`https://`) | a link showing its host; opened in the browser on click only |
| `group` | `title`?, `children`: component`[]`, `collapsed`? | a titled group (nesting at most 4 levels) |

**Action**: `{"id", "label", "style": "primary" | "secondary" | "danger", "enabled": bool, "disabled_reason"?,
"confirm"?: localized text, "form"?: form id}`. With `confirm` the launcher asks first; with `form` the action submits
that form's values.

**Field types for forms** are those of 1.0 (`string`, `integer`, `boolean`, `enum`, `teams`, `mode`, `stage`) — a
stage pick and ban can be a `form` with a `stage` field whose `values` are the stages still in play, and a `ban`
action. Since 1.1.6 the area `draft` ([draft.md](draft.md)) does it inside the match itself, with turns, deadlines,
votes and a dedicated screen; an activity's matches and series use it when the server says so.

**Unknown component types**: a launcher shows the component's `fallback` text if it has one (any component MAY carry
`fallback`: localized text), else skips it; it never fails the whole view. An unknown member is ignored; an unknown
`style` or `level` renders as the default.

## 4. How an activity reaches matches

The activity's server code creates matches exactly as rooms and queues do (with the 1.0 accept phase or without),
optionally grouped into [series](series.md). The match object carries everything the launcher needs; nothing about
the activity's logic (brackets, standings, picks) is ever needed to play.

## 5. Icons and colors

`icon` is one of: `trophy`, `swords`, `shield`, `star`, `crown`, `flag`, `clock`, `calendar`, `users`, `user`,
`target`, `fire`, `bolt`, `medal`, `ribbon`, `map`, `dice`, `lock`, `check`, `info`, `warning`, `chat`, `chart`,
`heart`. A launcher draws them with its own icon set; an unknown name shows no icon. `color` is `#RRGGBB`, a hint the
launcher may adapt for contrast.

## 6. Limits

| What | Most |
|---|---|
| a view (JSON) | 256 KiB |
| components in a view (all levels) | 500 |
| nesting of `group` | 4 levels |
| items of a `list`, rows of a `table` | 500 |
| actions in a view | 50 |
| a localized text entry | 2000 characters (titles and labels: 120) |
| bracket | 8 rounds, 128 matches in a round |

A server MUST stay within them; a launcher MAY refuse a view that does not.
