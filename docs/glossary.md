# Glossary

The words of the protocol and of this repository, in alphabetical order. The specification defines them precisely;
this page is a quick reference.

**Activity** — something a server invents and describes as data (a tournament, a challenge, a themed evening); the
launcher draws its *view* from a closed list of components, never from code sent by the server
([activities.md](protocol/activities.md)).

**Area** — a part of the protocol a server may implement (`rooms`, `queues`, `parties`, `friends`, `ratings`...),
declared in `GET /v1/meta`. Every server implements the *core*; a launcher never calls an undeclared area.

**Conformance suite** — `kalamar-testkit conformance`: scenarios played through the public API that check a server
for the areas it declares. A server is conformant at its level when they all pass.

**Core** — what every server implements: discovery, `GET /v1/meta`, at least one login, the account, the event
stream and the match lifecycle.

**Discovery document** — `/.well-known/kalamar-server.json`: the server's name, operator, API address, protocol
versions and relay, fetched when a player adds a server by its host name.

**Embedded relay** — a relay running inside the service on a thread of its own (`[embedded_relay]`), for small
servers: one program to start.

**Forfeit** — a match lost by the team of a player who left it while it was being played (1.1.11, the result policy's
`leave` member); a crash or a lost connection is never a forfeit ([leaving.md](protocol/leaving.md#4-forfeit-1111)).

**Launcher** — the program the player uses; it starts the game for one match and talks to the service. The Kalamar
launcher is one; anyone can write another from the specification.

**Match contract** — what a match asks of the game and how the launcher passes it on (`lan-auto-match`, versions 1
and 2: [match-contract.md](protocol/match-contract.md), [match-contract-2.md](protocol/match-contract-2.md)).

**Match object** — what the service gives each launcher for a match: contract, relay, room code, member names, rules.

**Member name** — a player's name inside one match (1 to 10 characters, unique in the match).

**Operator** — whoever runs a server, responsible for its community, moderation and legal compliance.

**Population** — the optional `GET /v1/population`: how many players are online, in a match and searching (numbers
only).

**Relay** — the UDP forwarder matches run through (`kalamar-relay`): no accounts, nothing on disk; it forwards the
game's datagrams only between members of one room.

**Result file** — what the game writes at the end of a match (mode, stage, teams, points, statistics), uploaded
unchanged by each launcher.

**Result policy** — how a server decides a match from the reports (how many must agree, what a spectator's report
weighs, whether leaving a match in play loses it), declared in advance; `standard` is the reference one.

**Room code** — the secret, unguessable name of a match's room on a relay. A *sealed* code carries a tag made with the
secret a relay shares with one service; a relay bound by a secret opens only sealed codes.

**Series** — several matches between the same participants (best of 3, first to N), possibly in one game session
([series.md](protocol/series.md)).

**Server card** — a small JSON file that adds a server to a launcher ([server-card.md](protocol/server-card.md)).

**Service** — the HTTPS part of a server (`kalamar-service` is the reference one): accounts, rooms, queues, results,
ratings, moderation.

**Spectator** — a member of a match's game session seated in the stands instead of a team; a spectator's result file
is a witness report ([spectators.md](protocol/spectators.md)).

**Stage draft** — teams banning and picking stages turn by turn before a match ([draft.md](protocol/draft.md)).

**Status file** — what the game rewrites at each change of state during a match; the launcher mirrors it to the
service.

**Supported game build** — the one game build the match contract targets. The protocol names its settings but holds
none of its content: mode and stage identifiers are opaque strings that come from the players' own copies.
