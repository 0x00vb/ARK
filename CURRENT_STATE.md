# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-03

## Release

| | |
|---|---|
| Latest tag | v0.5.0 (2026-10-01) |
| In progress | v0.6.0 is being prepared by another session (stabilization: named agent profiles, project folder in ARK, native folder picker, shared approval title, changelog fixes). Not tagged. Check `git log` and `CHANGELOG.md` `[Unreleased]` for the current state. |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | the v0.6.0 tag, to bump its pin and drop its local `[patch]` |
| SecRes | nothing; pins v0.5.0 |

## Next

After v0.6.0: the `Next` list in [`ROADMAP.md`](ROADMAP.md).

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
