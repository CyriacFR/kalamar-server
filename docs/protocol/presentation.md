# Kalamar community protocol 1.1 — ratings, statistics, badges and profiles as data

1.0 already presents ratings, leaderboards and statistics as opaque data ([api.md](api.md#ratings)). 1.1 adds badges
and titles of the server's own design and a fully server-designed profile page, so any rating system, any number of
ratings (per mode, per format, per season, per clan), named ranks, rewards and free seasons can be shown without the
launcher knowing them. Areas: `badges`, `profiles`. Nothing here may reuse names, art or rewards of the game: badges
and titles are the community's own.

## 1. Badges and titles

A badge is something a player earned on this server (a tournament win, a season rank, a streak); a title is a short
text a player may show next to their name.

```json
{"id": "spring-cup-2026", "name": {"en": "Spring cup champion"}, "description": {"en": "Won the 2026 spring cup."},
 "icon": "trophy", "color": "#D4AF37", "rarity": {"en": "1 player"}, "earned_at": "2026-04-20T21:00:00Z"}
```

`icon` and `color` follow [activities.md](activities.md#5-icons-and-colors) (no images).

| Endpoint | Answer |
|---|---|
| `GET /v1/users/{user_id}/badges` | `{"items": [Badge], "featured_badge": "<badge id>" or null, "title": localized text or null}` (visible like statistics: `public_profile`) |
| `PATCH /v1/me` with `{"featured_badge": "<id>" or null, "title": "<title id>" or null}` | the Me object (1.1 adds `featured_badge` and `title`, both ids, to it) |
| `GET /v1/titles` | `{"items": [{"id", "text": localized text, "unlocked": bool, "hint": localized text}]}` |

The User object MAY carry `badge` (the featured Badge's `id`, `icon`, `color`) and `title` (localized text), shown next
to display names; launchers ignore them when absent.

## 2. Profiles

`GET /v1/users/{user_id}/profile` — *auth* — answers a **view** ([activities.md](activities.md#3-components)): any
components the server chooses (a view whose members other than `sections` are optional) (rating tables per mode, a season history, recent series, badges as `user` and `list`
components, charts as `table`s). Privacy rules of 1.0 apply (`public_profile`). A launcher shows it on a player's
page; it MAY also keep its 1.0 rendering of `ratings` and `stats` when the area is declared.

## 3. Several ratings, any system

- A server declares as many **ladders** as it wants (one per mode, per format, per clan, a seasonal one, a fun one);
  queues and activities name the ladder they count for.
- The rating view of 1.0 (`value`, `display`, `tier`, `progress`, `provisional`, `last_change`, `fields`) is enough
  for Elo, Glicko, TrueSkill, points, win streaks, ladders with promotion matches, or anything else: the launcher never
  computes, it shows.
- 1.1 adds two optional members to a ladder: `group` (localized text, to group ladders in lists, for example a mode
  or a season) and `primary` (boolean: the ladder to show next to a name).
- Seasons are free: a server may run several at once (one per ladder) by giving each ladder its own `season`.
