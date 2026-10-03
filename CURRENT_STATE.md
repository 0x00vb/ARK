# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-03

## Release

| | |
|---|---|
| Latest tag | v0.6.0 (2026-10-03), on `origin` and `github` |
| In progress | the measured-runtime milestone (0.7): orchestration control is in `ark-interact`; P0 and P1 are done (live results in `bench/RESULTS.md`: ARK beats a naive loop on Claude Haiku, loses to it on qwen3:4b); P2 under way: preloading, the ready-to-call list, small arrays whole and the planner off are defaults (qwen3:4b 42/42 at $0.0181 per success, against the naive loop's 36/42 at $0.0268). The user chose to run the Haiku check once, at the 0.7 release gate, after the remaining runtime work; no Claude runs before that without asking. `ROADMAP.md` has the phases |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | the next tag, for the window's project sidebar, top bar, Settings page, Terminal, Browser and Files, `ark.compose` and the chat's agent tools (`[Unreleased]`); its uncommitted changes use them through the local `[patch]` |
| SecRes | nothing; pins v0.5.0 |

## Next

After v0.6.0: the `Next` list in [`ROADMAP.md`](ROADMAP.md).

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
