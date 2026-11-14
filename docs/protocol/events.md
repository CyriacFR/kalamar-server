# Kalamar community protocol 1.1 — event stream

One WebSocket per launcher carries everything the service pushes: match found, room changes, friend requests,
presence, results. It is part of the core. Requests always go over HTTPS ([api.md](api.md)); the WebSocket only
carries pushes (and heartbeats), so a launcher that loses it loses no data: it resumes, or refetches `GET /v1/state`.

## 1. Connecting

`GET <api>/v1/events` with a WebSocket upgrade (RFC 6455; `wss://` for an `https://` API). Authentication, either:

- `Authorization: Bearer <access token>` on the upgrade request (preferred; native launchers can set it), or
- `?ticket=<ticket>` from `POST /v1/events/ticket` — *auth* — which answers `{"ticket": "...", "expires_at": "..."}`:
  single-use, valid 60 s, for clients that cannot set headers on a WebSocket upgrade. Access tokens never go in a
  URL.

Optional query parameters: `resume=<stream id>` and `last_seq=<n>` ([Resuming](#4-resuming)).

A refused upgrade is an ordinary HTTP error (`401`, `403 banned`, `429`). A server MAY limit the connections per
account (the reference: 4) and closes the oldest with `bye` reason `replaced` when the limit is passed.

## 2. Frames

Text frames, one JSON object each, at most 64 KiB. Two kinds:

**Events** carry a sequence number:

```json
{"seq": 17, "type": "room.updated", "at": "2026-11-11T20:15:03Z", "data": { ... }}
```

**Control messages** have no `seq`:

| `type` | Direction | Members | Meaning |
|---|---|---|---|
| `hello` | server → client | `stream`, `resumed`, `last_seq`, `heartbeat_s`, `resume_window_s`, `protocol` | first message after the upgrade |
| `ping` | both | `t` (optional, echoed) | heartbeat |
| `pong` | both | `t` | answer to `ping` |
| `ack` | client → server | `seq` | optional: the client has processed events up to `seq` (the server MAY drop them from its replay buffer) |
| `bye` | server → client | `reason` | the server is closing the stream: `replaced`, `session_revoked`, `banned`, `shutdown`, `protocol_error`. Unknown reason: reconnect with backoff |

Clients send only control messages; anything else is ignored (a server MAY close with `bye` `protocol_error` after
repeated garbage).

## 3. Heartbeat

`hello.heartbeat_s` (the reference: 20) is the interval. The server sends `{"type": "ping", "t": <number>}` every
`heartbeat_s` seconds; the client answers `{"type": "pong", "t": <same>}`. Either side MAY also use WebSocket ping
frames. A side that receives nothing (no frame of any kind) for 3 × `heartbeat_s` considers the connection dead and
closes it: the client reconnects, the server keeps the stream for resumption.

Presence: a player is `online` while one of their streams is connected, and for 60 s after the last one closes.

## 4. Resuming

Each stream has an id (`hello.stream`) and numbers its events from 1 with no gaps. The server keeps the events of a
stream for `resume_window_s` (the reference: 120 s, at most 256 events) after its connection is lost.

To resume, the client reconnects with `?resume=<stream>&last_seq=<last seq it processed>`:

- if the server still has every event after `last_seq`, it answers `hello` with `resumed: true` and the same
  `stream`, then replays those events in order, then continues live;
- otherwise it answers `hello` with `resumed: false` and a **new** `stream`; the client MUST then fetch `GET
  /v1/state` (and whatever it displays) before relying on events.

A stream belongs to the session that opened it; resuming another session's stream starts a new stream. A client
reconnects with exponential backoff (1 s, doubling, at most 30 s, with jitter) and resumes when it can.

## 5. Event types

`data` is the full current object (not a diff) unless said otherwise. A launcher ignores types it does not know.

| `type` | Area | `data` | When |
|---|---|---|---|
| `meta.changed` | core | `{}`; 1.1.9: optionally `{"revision": n}`, the new `meta.revision` | the server's offer changed (for example its configuration was reloaded): refetch `GET /v1/meta` and whatever the launcher shows from it (queues, the room form, activities, ladders). Sessions, streams, rooms and matches are not affected |
| `me.updated` | core | Me | display name, settings or restrictions changed (for example a ban or a cooldown) |
| `session.revoked` | core | `{"reason": "logout" \| "erased" \| "banned" \| ...}` | sent just before `bye` |
| `match.found` | queues | Match (`pending_accept`) | matchmaking found a match: show accept / decline |
| `match.updated` | core | Match | any change: an accept, a launch, a status, a deadline; 1.1.6: every change of the match's draft (a vote in progress only to the voter's team); 1.1.7: a place in the stands taken or left |
| `match.ready` | core | Match (`ready`, with `game`) | launch now (1.1.6: also the end of a draft) |
| `match.cancelled` | core | Match (`cancelled`; its `cancel_reason` says why) | before any launch |
| `match.void` | core | Match (`void`, with `outcome`) | close the game if it is still running before the stage |
| `match.completed` | core | Match (`completed`, with `outcome`) | show the result |
| `rating.changed` | ratings | `{"changes": [Rating change], "ratings": [Rating view]}` | after a rated match |
| `queue.ticket` | queues | `{"ticket": Ticket or null, "reason": "cancelled" \| "matched" \| "requeued" \| "queue_closed" \| "party_changed" \| null}` | the player's ticket changed or ended |
| `queue.cooldown` | queues | `{"until": "...", "offence": "decline"}` | a penalty started |
| `room.updated` | rooms | Room | any change of the player's room |
| `room.invite` | rooms | `{"room_id", "code", "from": User, "expires_at"}` | an invitation |
| `room.closed` | rooms | `{"room_id", "reason": "empty" \| "idle" \| "kicked" \| "admin"}` | the player is no longer in the room |
| `party.updated` | parties | Party | any change of the player's party |
| `party.invite` | parties | `{"party_id", "from": User, "expires_at"}` | an invitation |
| `party.closed` | parties | `{"party_id", "reason": "left" \| "kicked" \| "disbanded"}` | the player is no longer in the party |
| `friend.request` | friends | Request (incoming) | a new request |
| `friend.added` | friends | Friend | a request was accepted (either side) |
| `friend.removed` | friends | `{"user_id"}` | a friendship ended |
| `friend.presence` | presence | `{"user_id", "presence"}` | a friend's presence changed |
| `activity.updated` | activities (1.1) | `{"activity_id", "revision"}` | an activity's view changed for this player |
| `activity.notice` | activities (1.1) | `{"activity_id", "notice"}` | a message for this player |
| `series.updated` | series (1.1) | Series | a series the player is in changed (1.1.7: or watches from its stands) |

1.1.7: every `match.*` event of a match also goes to its spectators ([spectators.md](spectators.md)), with their own view (`you.team: spectator`). A spectator who leaves its place gets one last `match.updated` without `you`: the match is no longer its.

Ordering: events of one stream arrive in the order the server produced them. A launcher that receives a `match.*`
event for a match id it does not know fetches `GET /v1/matches/{id}`.

## 6. Example

```
client: GET /api/v1/events  (Upgrade: websocket, Authorization: Bearer kat1....)
server: {"type":"hello","stream":"es_4f...","resumed":false,"last_seq":0,"heartbeat_s":20,"resume_window_s":120,"protocol":"1.1"}
server: {"seq":1,"type":"match.found","at":"2026-11-11T20:15:03Z","data":{"id":"m_9x...","state":"pending_accept",...}}
client: {"type":"ack","seq":1}
server: {"type":"ping","t":1794346577}
client: {"type":"pong","t":1794346577}
   ... connection lost after seq 3 ...
client: GET /api/v1/events?resume=es_4f...&last_seq=3
server: {"type":"hello","stream":"es_4f...","resumed":true,"last_seq":5,...}
server: {"seq":4,...}
server: {"seq":5,...}
```
