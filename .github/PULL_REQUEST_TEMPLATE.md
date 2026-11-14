<!--
Thanks! Rules: CONTRIBUTING.md. One topic per pull request; describe it in your own words. Delete what does not apply.
-->

## What and why

<!-- The problem, the change, and the issue it closes (Closes #123). -->

## How it was tested

<!-- The commands you ran and their results; for protocol behavior, the conformance scenario. Say what you could not
run. Written with an AI assistant? The same applies: CONTRIBUTING.md, "AI-assisted contributions". -->

## Checklist

- [ ] `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` pass
- [ ] Every check reported above was actually run on this change, by me; the ones I could not run are named as not run
- [ ] A test or a conformance scenario covers the change
- [ ] Protocol change: `api.md` (and related pages), `openapi.yaml` and the "Changes" table of `overview.md` updated together
- [ ] Nothing derived from a game or a console added, not even for reference (files, extracts, lists of modes or stages, images, keys)
- [ ] No new personal data stored or logged (or `PRIVACY-TEMPLATE.md` and `docs/LEGAL-NOTES.md` say what and why)
- [ ] New dependency: justified in the README's "Dependencies" table, `docs/third-party-dependencies.md` regenerated
- [ ] Commits are signed off (`git commit -s`)
