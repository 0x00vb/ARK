# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-04 (v0.9.0; `main` has the browser, step 5 and C5, untagged)

## Release

| | |
|---|---|
| Latest tag | v0.9.0 (2026-10-04), on `origin` and `github` |
| `main` | Everything since v0.9.0 in one place, no other branches: the browser (C4, ADR 0015), the window hooks for ARK Desktop's UX pass (`ArkUi::theme`, `ark.modelPicker`, `ark.onSubmit`), a runtime other machines connect to with saved access tokens (ADR 0016, [`docs/REMOTE.md`](docs/REMOTE.md)), work that runs without the window: several live chats, schedules (ADR 0017), and the Computer, `ark-computer` (C5, ADR 0018). `scripts/ci.sh` and the WebKit page tests passed on it. Not pushed |
| Next tag | v0.10.0 (`Breaking` entries under `[Unreleased]`). Ready to cut with [`docs/RELEASING.md`](docs/RELEASING.md) steps 2 to 9 when the owner says so. ARK Desktop 0.2 waits for it |
| Step 2 | Done in v0.9.0 (ADR 0012, ADR 0014). Deferred: start the CLI before the first call, deeper provider telemetry |

## Open follow-ups (not started)

| What | Owner |
|---|---|
| Rename "Allow for this session" in approvals | the owner decides the words |
| Turn the Computer off when an example serves other machines with tokens | remote work (`examples/writing-agent`) |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | v0.10.0. Its `main` has the whole 0.2 (Mail, Calendar, Browser, the UX pass, remote) and builds against `../ark` through a gitignored `[patch]` until the tag exists |
| SecRes | nothing; pins v0.8.0. v0.9.0 and v0.10.0 are optional upgrades (`Breaking` in `CHANGELOG.md`) |
| Mail and Calendar for anyone | ARK's Google OAuth client and Google's review: owner steps in [`docs/GOOGLE.md`](docs/GOOGLE.md). Until then they show as unavailable, or work with `ARK_GOOGLE_CLIENT_ID` for test users |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (done),
Claude Code and Codex first-class (done, v0.9.0), ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK (on `main`, untagged), ARK Cloud on the same
runtime.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
