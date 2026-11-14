# Instructions for coding assistants

For AI coding assistants (Claude Code reads this file through [CLAUDE.md](CLAUDE.md); Codex, Cursor, Copilot and
others read it directly) and for the people who drive them: what the code does not tell you. The full rules are in
[CONTRIBUTING.md](CONTRIBUTING.md), which wins where the two differ.

## Map

The open protocol of Kalamar's community servers (version 1.1, revision 1.1.13) and its reference implementation.
The launcher and the game live in the other repository, `Kalamar`; they implement their side of what is specified here.

| Path | What |
|---|---|
| `docs/protocol/` | **the normative specification** and `openapi.yaml`: what other launchers and servers implement |
| `crates/kalamar-protocol` | the specification's pure functions, no I/O: match contract helpers, result validation, the result decision |
| `crates/kalamar-service` | the reference service: HTTP API (`src/api/`, one module per area), events, matchmaking, ratings, admin command line; SQLite migrations in `migrations/` |
| `crates/kalamar-relay` | the UDP relay matches run through; self-contained, it builds alone |
| `crates/kalamar-testkit` | simulated launchers and a simulated game, and the conformance suite any server can be tested with |
| `deploy/` | the installer (`install.sh`) and its tests, example configurations, systemd units, proxies, Dockerfile |
| `scripts/` | the repository checks CI runs |

Tour: [docs/architecture.md](docs/architecture.md). Index: [docs/README.md](docs/README.md).

## Checks before a pull request

CI (`.github/workflows/ci.yml`, `checks.yml`) runs these. Rust 1.97.1 with clippy and rustfmt, Python 3.8 or newer:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace        # unit and HTTP tests, and the conformance suite against three server offerings
bash deploy/test-install.sh            # after an installer change; dry runs only (Git Bash works on Windows)
python scripts/check-english.py
python scripts/check-links.py
python scripts/check-content.py
python scripts/third-party-inventory.py --check   # without --check it regenerates docs/third-party-dependencies.md
```

CI also builds the three programs in release mode and runs the conformance suite against a running reference
server on 127.0.0.1, with the test clock and raised rate limits: the recipe is the `conformance` job of
`.github/workflows/ci.yml` (the README's "Run it locally" explains why a run with `deploy/kalamar.local.toml` as it is
reports `NOT CONFORMANT`).

**Not testable here:** matches with the real game and launcher (only two-player matches were ever played), and
logins against the real OAuth providers and SMTP services (only local stand-ins). Say in the pull request what you
could not run.

## Rules that are not negotiable

1. **No game content, ever:** no game files, extracted data, lists of the game's modes, stages, weapons or gear,
   artwork, fonts, keys, or addresses and protocols of the publisher's online services. Mode and stage identifiers
   are opaque strings that come from each player's own game. Why: the project exists only because it ships nothing of
   a game, and it does not describe, imitate or connect to any publisher service. `scripts/check-content.py` catches
   file types, names, keys and secrets, not everything.
2. **The game contacts no one by itself.** The service never talks to a game and a game never talks to the service:
   the launcher is in between, and the game reaches only the relay of its match, by an address the launcher gives it
   (it never resolves names). A server must never ask a launcher to open any other connection for the game, or to
   disable TLS or certificate checks
   ([extending.md](docs/protocol/extending.md#5-what-a-server-must-never-ask-of-a-launcher)). Plain HTTP is for
   `127.0.0.1` test servers only. Why: players must be able to trust that a server cannot point their game or their
   launcher anywhere else.
3. **A server sends data, never code:** plain text and typed data components, no HTML, markdown, script, style sheet or
   image ([activities.md](docs/protocol/activities.md)). In the other direction, everything a launcher, a game's result
   file or a relay datagram brings is untrusted: malformed input gets the protocol's 4xx error shape, never a 5xx
   (`malformed_input_never_answers_5xx` in `crates/kalamar-service/tests/http.rs`), sizes are bounded, and results are
   only cross-checked, never believed. Why: anyone can run a server and anyone can write a launcher. Keep what
   [docs/security-model.md](docs/security-model.md) and [SECURITY.md](SECURITY.md) promise true.
4. **English only** in code, comments, docs, commit messages and logs (American spelling), outside the exceptions of
   `scripts/english-only.json`. The service's own texts for players exist in English and French in the code
   (`auth/password.rs`, `auth/mail.rs`, `matches.rs` and a few others); texts an operator configures are maps of
   language codes. Why: one working language for contributors, and players read their own.
5. **Compatibility.** A minor protocol revision only adds: new members are optional, enums are open, nothing is
   removed ([extending.md](docs/protocol/extending.md#1-versions)). A change updates `api.md` (and `events.md`,
   `match-contract.md` as needed), `openapi.yaml` and the changes table of `overview.md` together, and adds a
   conformance scenario in `crates/kalamar-testkit`. A database change is a new migration file plus its entry in
   `db::MIGRATIONS`, never an edit of an applied one. The configuration refuses unknown keys
   (`deny_unknown_fields`), so a key is never renamed or removed, and a new one is optional with a default. Why:
   launchers, servers and operators' files in the field must keep working.
6. **No code from projects with an incompatible license.** Copy only MIT, BSD, ISC, zlib or Apache-2.0 code, keep its
   notice, say where it comes from. Copyleft code may be read for behavior, never copied or reproduced from memory.
   Why: the protocol and the server are MIT and must stay reusable by anyone.
7. **Privacy and money.** Log no token, code, player id, IP address or payload; add no data collection; no payment,
   shop or currency. A change to what is stored updates `PRIVACY-TEMPLATE.md` and `docs/LEGAL-NOTES.md`. Why:
   operators answer for their players' data, and the protocol keeps it minimal by design.
8. **Local runs stay local.** `deploy/kalamar.local.toml` enables the `dev` login (anyone logs in as anyone): bind to
   127.0.0.1 and never expose it. Tests are deterministic (the manual clock, `kalamar_service::clock::ManualClock`, no
   sleeping), bind to 127.0.0.1 and never contact a real service. Secrets (`*.key`, `data/`) are never committed.
9. **Open an issue before changing these:** the normative pages of `docs/protocol/` and `openapi.yaml` (other people
   implement them); applied migrations in `crates/kalamar-service/migrations/`; the relay's wire format, which is the
   engine's (`engine/docs/lan-relay-protocol.md` in the Kalamar repository; a change on one side breaks games);
   `rustfmt.toml` (it would reformat everything); the exceptions of `scripts/content-rules.json` and
   `scripts/english-only.json` (each needs a reason a maintainer reviewed). `docs/third-party-dependencies.md` is
   generated, never edited by hand.

## Conventions

- **Commits:** a plain English sentence in the imperative that says what the change does, without `feat:` or `fix:`
  (`Free a player of a match once its report is in`); the body says why; a commit about an issue
  ends its subject with `(#123)`. Each commit is signed off (`git commit -s`, Developer Certificate of Origin).
- **Pull requests:** one topic, as small as it can be, from and against `main`, rebased before review, the template
  filled in, CI green. A protocol change, a new dependency, a new kind of stored data, a new login method or a change
  to what is logged starts as an issue.
- **Tests and docs come with the change:** a test that fails without it, a conformance scenario for protocol
  behavior; the specification, the README or `docs/operator-first-steps.md` for operators, `SECURITY.md` when what is
  protected changes. A new dependency is justified in the README's "Dependencies" table.
- `cargo fmt` is enforced; do not mix formatting of untouched code into a change.

## AI-assisted contributions

Welcome, under every rule above. The person who opens the pull request is its author and answers for every line:

- Read the whole diff and understand it. Run the checks yourself, and write in the pull request, in your own words,
  what was run (commands and results) and what was not. A template ticked by a tool is not a test report.
- An assistant never invents or extrapolates a test result, a conformance summary, a measurement or a log line. A
  check that did not run is reported as not run.
- No game content "for reference" (rule 1), and no real players' data, tokens, secrets or addresses in code, tests,
  logs, issues or pull requests.
- No code reproduced from a copyleft project, from memory or otherwise (rule 6).
- `Co-Authored-By:` lines naming an assistant are welcome and never required. `Signed-off-by:` is the human author's
  own statement under the DCO: the human adds it.

## Read next

[CONTRIBUTING.md](CONTRIBUTING.md), [docs/architecture.md](docs/architecture.md),
[docs/protocol/overview.md](docs/protocol/overview.md), [docs/security-model.md](docs/security-model.md),
[SECURITY.md](SECURITY.md).
