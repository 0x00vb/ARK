# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-04 (v0.9.0, browser on `main`)

## Release

| | |
|---|---|
| Latest tag | v0.9.0 (2026-10-04), on `origin` and `github` |
| Step 2 | Done in v0.9.0: Claude Code and Codex kept running per session, streamed answers, cancel, usage limits with their reset time (ADR 0012); subscription usage for apps (`ark_model::subscription_usage`, window method `subscription_usage`); agents follow the session's model (ADR 0014). A longer live task passed on each CLI (`bench/RESULTS.md`). Deferred: start the CLI before the first call, deeper provider telemetry |
| In progress | Next step 3, ARK Desktop public. Capabilities beside it (ROADMAP › Capabilities): C2 and C3 are in v0.9.0, grants per project included; C4 (Browser, ADR 0015) is on `main` since v0.9.0; ARK Desktop gets it with `.browser(true)` on the next tag |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | 0.1.0 ships on v0.8.1 (no capabilities). Its 0.2.0 turns on Mail, Calendar and Browser (`.connections(true)`, `.browser(true)`) and drops `choice::Follow` (ADR 0014); it needs the next ARK tag, the first with the browser (ark-desktop `ROADMAP.md` › Next). Its UX pass (branch `ux/simplify`) also needs `ArkUi::theme`, `ark.modelPicker` and `ark.onSubmit`, on `main` since d7c9910..4e18d35 |
| SecRes | nothing; pins v0.8.0. v0.9.0 is an optional upgrade (`Breaking` in `CHANGELOG.md`) |
| Mail and Calendar for anyone | ARK's Google OAuth client and Google's review: owner steps in [`docs/GOOGLE.md`](docs/GOOGLE.md). Until then they show as unavailable, or work with `ARK_GOOGLE_CLIENT_ID` for test users |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (done),
Claude Code and Codex first-class (done, v0.9.0), ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK, ARK Cloud on the same runtime.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
