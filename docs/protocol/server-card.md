# Adding a server: server card, one-line form, discovery (protocol 1.1)

A player adds a community server to a launcher in one of three ways. All three end with the same three values the
launcher stores: **name**, **api** (the HTTPS base URL of the service) and **relay** (host and UDP port).

1. typing a **host name** (the one-line form): the launcher fetches the **discovery document** from that host;
2. opening or pasting a **server card**: a small JSON file the operator publishes or sends;
3. typing the three values by hand (advanced).

## 1. The one-line form

```
host[:port]
```

- `host` is a DNS name (letters, digits, `-` and `.`, at most 253 characters, compared in lower case) or an IPv4
  address. IPv6 literals are not part of version 1.0 (the supported game build's network side is IPv4-only).
- `port` is the **HTTPS** port of the discovery document, default 443.
- A launcher MAY tolerate a pasted `https://` prefix and one trailing `/`; anything else after the host (a path, a
  query, a user name) is refused.

The launcher then fetches `https://<host>[:<port>]/.well-known/kalamar-server.json`.

**The port of a line is always the HTTPS port of the discovery document**, never the relay's UDP port: the relay
and its port come from the discovery document (`relay`) and from each match. A launcher MUST NOT guess a relay
port from the line. Operators who run both on one host publish the discovery document on 443 and give players
the host name alone.

**Relay-only lines.** A line `host[:port]` may also name a bare relay (UDP port, default
47313). A launcher that fails to fetch a discovery document MAY offer to add the line as a *relay-only* server (no
service, match codes copied between players), and only after the player confirms it: the port is then read as the
relay's UDP port. It MUST say that it did so and that no service features are available.

## 2. The discovery document

`GET https://<host>[:<port>]/.well-known/kalamar-server.json` — no authentication, `Content-Type:
application/json`, at most 64 KiB. A server SHOULD send `Cache-Control: max-age=300` or longer, and SHOULD allow
cross-origin reads (`Access-Control-Allow-Origin: *`) since the document is public.

```json
{
  "kalamar_server": 1,
  "name": "Thursday Night Club",
  "description": {"en": "A small community for Thursday nights.", "fr": "Une petite communauté du jeudi soir."},
  "api": "https://play.example.org/api",
  "relay": "relay.example.org:47313",
  "protocol": {"versions": ["1.0", "1.1"]},
  "features": ["rooms", "queues", "parties", "friends", "presence", "ratings", "leaderboards", "stats", "moderation"],
  "operator": {"name": "Camille", "contact": "mailto:admin@example.org"},
  "rules_url": "https://play.example.org/rules",
  "privacy_url": "https://play.example.org/privacy",
  "region": "eu-west",
  "languages": ["fr", "en"]
}
```

| Member | Type | Required | Meaning |
|---|---|---|---|
| `kalamar_server` | integer | yes | format of this document; `1`. A launcher refuses other values. |
| `name` | string, 1-64 characters | yes | the server's name as shown in the launcher |
| `description` | [localized text](api.md#localized-text) | no | one paragraph |
| `api` | string | yes | base URL of the API: `https://host[:port][/path]`, at most 200 characters, no user info, no query, no fragment. The endpoints of [api.md](api.md) are relative to it (`<api>/v1/meta`). It need not be on the same host as the document. |
| `relay` | string | yes, or `null` | the default relay, `host[:port]` (UDP, default port 47313). Informational: every match names its own relay ([relay.md](relay.md)). `null` when the service names relays per match only. |
| `protocol.versions` | array of strings | yes | protocol versions the service speaks (`"1.0"`, `"1.1"`) |
| `features` | array of strings | yes | the optional areas declared ([overview.md](overview.md#2-areas-and-the-minimal-core)); MUST equal `features` of `GET /v1/meta` |
| `operator.name` | string, at most 64 characters | yes | who runs the server (a person, an association, a pseudonym) |
| `operator.contact` | string, at most 200 characters | yes | a `mailto:` or `https:` URI where the operator can be reached (abuse, privacy requests) |
| `rules_url` | string (https URL) or `null` | yes | the community's rules for humans |
| `privacy_url` | string (https URL) | yes | the privacy notice |
| `region` | string, at most 32 characters | no | free text (`"eu-west"`) shown as a hint |
| `languages` | array of language tags | no | languages the community uses |

Unknown members are ignored ([overview.md](overview.md#51-extensibility-rules-both-sides)). `x-` members are the
operator's.

A launcher validates the document, shows the player the name, operator, region, features and the links, and asks for
confirmation before saving the server. It then calls `GET <api>/v1/meta` ([api.md](api.md#meta)), which is
authoritative for everything the service offers; the discovery document is only the entry point.

## 3. The server card

A card is the same information in a file the operator hands out (`*.kalamar.json`, at most 64 KiB):

```json
{"kalamar_server": 1, "name": "Thursday Night Club", "api": "https://play.example.org/api",
 "relay": "relay.example.org:47313"}
```

| Member | Required | Meaning |
|---|---|---|
| `kalamar_server` | yes | `1` |
| `name` | yes | as in the discovery document |
| `api` | yes, or `null` | the API base URL; `null` for a relay-only server |
| `relay` | yes, or `null` | `host[:port]` of the default relay |
| `discovery` | no | `host[:port]` of the discovery document, when the launcher should refresh the card from it |

Any other member of the discovery document MAY appear in a card and has the same meaning. A card with a non-null
`api` is checked like a typed host: the launcher calls `GET <api>/v1/meta` before saving it.

## 4. What the launcher stores

| Value | From |
|---|---|
| `name` | discovery document or card (the player MAY rename it locally) |
| `api` | discovery document or card |
| `relay` | discovery document or card; refreshed from `GET /v1/meta` |
| tokens | the login ([api.md](api.md#authentication)), stored in the operating system's credential store when possible |

The relay of each match comes from the match object, not from the stored value ([match-contract.md](match-contract.md)).
The launcher resolves relay host names to IPv4 addresses itself at each launch: the game only takes an address.

## 5. Security notes for launchers

- Fetch the discovery document over HTTPS with certificate validation; never follow a redirect to a non-HTTPS URL;
  stop after 3 redirects; time out after 10 s.
- Show the `api` host to the player when it differs from the typed host.
- Treat every string as untrusted text (no HTML, no markdown rendering of server strings unless sanitized).
