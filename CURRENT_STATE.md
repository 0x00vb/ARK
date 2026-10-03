# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-03

## Release

| | |
|---|---|
| Latest tag | v0.7.0 (2026-10-03), on `origin` and `github` |
| In progress | the measured-runtime milestone continues after v0.7.0: the Haiku regression check of the P2 defaults (not run for v0.7.0), then overhead on multi-step, resume, `parallel` and `team`. `ROADMAP.md` has the phases |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | nothing: v0.7.0 has what its `agents-canvas` branch uses (`ark.setting`, `ark.compose`, top-bar tabs, projects as folders, the chat's agent tools); it can pin v0.7.0 and drop its local `[patch]` |
| SecRes | nothing; pins v0.5.0 |

## Next

After v0.7.0: the `Next` list in [`ROADMAP.md`](ROADMAP.md).

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
