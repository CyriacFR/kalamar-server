# Contributing

Thank you for helping. This repository holds an **open protocol** and its reference implementation; both must stay
usable by people who will never read this code. Security problems are reported privately
([SECURITY.md](SECURITY.md)).

**Conduct.** Be respectful, and keep to the subject: the code, the protocol, the documentation. Maintainers may edit
or close issues and pull requests, and block accounts, that are abusive. Abusive content on GitHub is reported to
GitHub with the **Report content** link of the issue, comment or profile.

## Ground rules

- **No game content, ever**: no game files, extracted data, lists of the names of modes or stages, artwork or trademarks
  beyond what is needed to say which game build the match contract targets (call it "the supported game build").
  Mode and stage identifiers are opaque strings that come from the player's own game.
- **No publisher protocol**: this project does not describe or reimplement any online service of the game's
  publisher.
- **Privacy first**: do not add data collection; do not log tokens, codes, ids of players, IP addresses or payloads.
- **No money**: no payment, shop, currency or paid feature in the protocol or the reference server.
- **No help to cheat**: nothing that lets a player impose a false result, hide from the cross-checks, or see what
  other players' launchers keep private. Known limits of the cross-checks are documented (`docs/protocol/overview.md`,
  section 3.2), not exploited.
- Code, comments, documentation, commit messages, log and error messages are in **English** (one English: American
  spelling). Text the service sends to players exists in English and French in the code; see "Languages" below.
- Copy code only under MIT, BSD, ISC, zlib or Apache-2.0, keep its notice, and say where it comes from. Code under a
  copyleft license may be read to understand a behavior, never copied.

## Prerequisites

| Tool | Version | For |
|---|---|---|
| [Rust](https://rustup.rs) through rustup, with `clippy` and `rustfmt` | 1.97 or newer (the workspace's `rust-version`) | everything |
| A C compiler | the platform's usual one (MSVC Build Tools on Windows, `gcc` or `clang` on Linux) | the bundled SQLite and the TLS library |
| [Python](https://www.python.org) | 3.8 or newer | the repository checks in `scripts/` |
| bash | 4 or newer | `deploy/install.sh` and its test (Linux, or Git Bash on Windows) |

The server builds on Linux and Windows; CI runs the tests on both.

## Workflow

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace                   # unit, HTTP, and the conformance suite in three server offerings
python scripts/check-english.py                   # text is in English outside translations (scripts/english-only.json)
python scripts/check-links.py                     # relative links of the Markdown files
python scripts/check-content.py                   # nothing derived from a game, no key or secret (scripts/content-rules.json)
python scripts/third-party-inventory.py --check   # docs/third-party-dependencies.md matches Cargo.lock
bash deploy/test-install.sh                       # after a change to the installer
```

CI runs the same on Linux and Windows, plus the conformance suite against a running reference server and a release
build of the three programs.

- Tests must be deterministic: use the manual clock (`kalamar_service::clock::ManualClock`, `test_clock` in
  configuration) rather than sleeping; bind to 127.0.0.1; never contact a real service.
- Every new endpoint answers malformed input with the protocol's error shape (4xx); add it to
  `crates/kalamar-service/tests/http.rs` (`malformed_input_never_answers_5xx`).
- Database changes are new migration files in `crates/kalamar-service/migrations/` (never edit an applied one) and
  an entry in `db::MIGRATIONS`.
- Keep dependencies few; justify any new one in the README's "Dependencies" table, and regenerate the inventory
  (`python scripts/third-party-inventory.py`).

Testing your own server, or this one by hand: the README's "Run it locally" and the conformance suite
(`kalamar-testkit conformance --help`).

## Changing the protocol

`docs/protocol/` is the contract other launchers and servers implement.

1. Open an issue describing the need and the change; say whether it is additive (minor version) or breaking (major).
2. Update `api.md` (and `events.md`, `match-contract.md` as needed) **and** `openapi.yaml` in the same change, plus
   the "Changes" table of `overview.md`.
3. Respect the extensibility rules: new members are optional, enums are open, nothing removed in a minor version.
4. Implement it in the reference service and add a conformance scenario in `crates/kalamar-testkit`.

## Proposing a change

1. **Open an issue first** for anything big: a protocol change, a new dependency, a new kind of data stored, a new
   login method, a change to what is logged.
2. **One topic per pull request**, as small as it can be. Branch from `main` and rebase on it before review.
3. **Tests come with the change**: a test that fails without it, and a conformance scenario for protocol behavior.
4. **Docs come with the change**: the specification, the README or `docs/operator-first-steps.md` for operators,
   `PRIVACY-TEMPLATE.md` when what is stored changes, `SECURITY.md` when what is protected changes.
5. Fill in the pull request template; CI must pass. A maintainer reviews; part of a pull request can be taken, and the
   reason for what is left out is written on the pull request.

**Commit messages** are plain English sentences in the imperative, without `feat:`, `fix:` or other prefixes, that
say what the change does: `Free a player of a match once its report is in`. A commit about an issue
ends its subject with its number: `... (#123)`.

## AI-assisted contributions

Welcome, under the same rules. The person who opens the pull request is its author: you have read the whole diff, you
ran the checks yourself, and the pull request says in your own words what was run and what was not. An assistant
never invents a test result or a conformance summary, and never pastes game content or real players' data into the
work. `Co-Authored-By` lines are welcome, never required; the `Signed-off-by` line is yours (below). The instructions
coding assistants read, with what they cannot guess from the code, are in [AGENTS.md](AGENTS.md)
([CLAUDE.md](CLAUDE.md) points to it).

## Languages

Labels and texts that an operator configures are maps of language codes (`{ en = "...", fr = "..." }`): any language
works without a code change. The service's own texts (password forms, refusals, e-mails, decisions) exist in English
and French in the code (`auth/password.rs`, `auth/mail.rs`, `matches.rs`...); adding a language there touches every
one of them, so open an issue first. Prose and comments use American spelling; identifiers on the wire keep the
spelling they were published with (`cancelled`, `match.cancelled`, `launcher-cancelled`, `offence`). The installer (`deploy/install.sh`) asks its questions in English or French.

## Sign-off

The project asks for no contributor license agreement. Each commit is signed off under the
[Developer Certificate of Origin 1.1](https://developercertificate.org/): by adding a `Signed-off-by: Your Name
<you@example.org>` line (`git commit -s`), you state that you wrote the change or have the right to submit it under
the project's license. A pseudonym you use consistently is fine.

## Layout

A ten-minute tour: [docs/architecture.md](docs/architecture.md).

- `crates/kalamar-protocol`: pure functions of the specification (no I/O).
- `crates/kalamar-service/src/api/*`: one module per area of `api.md`; `matches.rs` the match engine;
  `matchmaking.rs` ticket grouping; `rating.rs` the reference rating system; `events.rs` the WebSocket;
  `state.rs` shared state (one lock, never held across an `await`).
- `crates/kalamar-testkit`: `client.rs` (API and events), `sim.rs` (the simulated game), `conformance.rs` (scenarios).
- `crates/kalamar-relay`: the relay; it stays self-contained so it builds alone.
- `deploy/`: example configurations, the installer, systemd units, proxy examples, the Dockerfile.
- `scripts/`: the repository checks CI runs.
