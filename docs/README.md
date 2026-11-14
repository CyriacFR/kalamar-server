# Documentation

Everything in this repository, by reader. New here? Start with the [README](../README.md).

## Running a server

| Page | What |
|---|---|
| [Operator: first steps](operator-first-steps.md) | from an empty Linux machine to a server launchers can add: names, relay (UDP), HTTPS, tests, login methods |
| [README: operating a server](../README.md#operating-a-server) | the reference: configuration, reloads, the admin command line, backups, logs, updates |
| [What the reference server offers](reference-server.md) | ratings, activities and series, drafts, spectators, operator integrations, what results guarantee |
| [Upgrading](upgrading.md) | backups, building, checking the configuration, restarting, going back; what to know about 1.1.13 |
| [Privacy notice template](../PRIVACY-TEMPLATE.md) | the starting point of the notice every server publishes |
| [Legal notes](LEGAL-NOTES.md) | what the software stores, logins and what they imply, no money (not legal advice) |
| [Security](../SECURITY.md) | reporting a vulnerability, what the software protects and how, its known limits, the hardening checklist |
| [Security model](security-model.md) | who is trusted, what one cheater can and cannot do, the residual risks, the findings of the security review and what was done |
| [`deploy/`](../deploy) | the installer, example configurations, systemd units, Caddy and nginx examples, the Dockerfile |

## The protocol (version 1.1, revision 1.1.13)

Read the overview first; the other pages are the normative specification, with [openapi.yaml](protocol/openapi.yaml)
for the HTTPS API.

| Page | What |
|---|---|
| [Overview](protocol/overview.md) | actors, areas, trust, privacy, versioning, errors, the list of changes |
| [Adding a server](protocol/server-card.md) | server card, one-line form, discovery document |
| [HTTPS API](protocol/api.md) | every endpoint by area, the match object, the result decision |
| [Event stream](protocol/events.md) | the WebSocket: hello, heartbeats, events, resume |
| [Match contract 1](protocol/match-contract.md) and [2](protocol/match-contract-2.md) | what a match asks of the game, and how capabilities are negotiated |
| [Relay](protocol/relay.md) | choosing and binding relays; the wire protocol is `engine/docs/lan-relay-protocol.md` in the Kalamar repository |
| [Activities](protocol/activities.md), [series](protocol/series.md), [stage draft](protocol/draft.md), [spectators](protocol/spectators.md) | what a server can build on top of matches |
| [Presentation](protocol/presentation.md) | ratings, statistics, badges and profiles as data |
| [Operator extension points](protocol/operators.md) | webhooks, the operator API, referees |
| [A player who leaves](protocol/leaving.md) | what happens when a game stops in the middle of a match, and the forfeit |
| [Rules for extending](protocol/extending.md) | what a server may and may not ask of a launcher |

## Writing your own server or launcher

| Page | What |
|---|---|
| [Writing your own server](protocol/writing-your-own-server.md) | the smallest conformant server, worked examples, the conformance suite |
| [From 1.0 to 1.1](protocol/for-launcher-implementers-1.1.md) | what a launcher adds to support 1.1 |
| [Relay notes](../crates/kalamar-relay/SPEC-NOTES.md) | where the relay specification was ambiguous and what this relay does |

## Contributing

| Page | What |
|---|---|
| [Contributing](../CONTRIBUTING.md) | rules, prerequisites, tests, changing the protocol, style, sign-off |
| [Instructions for coding assistants](../AGENTS.md) | what an AI assistant and its user must know: the map, the checks CI runs, the rules, AI-assisted contributions |
| [Architecture in 10 minutes](architecture.md) | the crates, a request's path, a match's life, where to start reading |
| [Glossary](glossary.md) | the words of the protocol and of this repository |
| [Third-party dependencies](third-party-dependencies.md) | every crate with its license (generated) |
| [The relay](../crates/kalamar-relay/README.md) | building, running and configuring `kalamar-relay` on its own |
