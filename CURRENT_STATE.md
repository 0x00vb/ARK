# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-03

## Release

| | |
|---|---|
| Latest tag | v0.8.0 (2026-10-03), on `origin` and `github` |
| In progress | Next step 2: Claude Code and Codex first-class. Step 1 (v0.8.0) is done: SecRes and ark-desktop pin it. Capability work (ROADMAP › Capabilities, C2 on) runs in parallel in another session |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | nothing; pins v0.8.0 with the web on. Per-agent skill switches in its agent inspector can use `AgentSpec::skills` and method `skills` now |
| SecRes | nothing; pins v0.8.0 |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (done),
Claude Code and Codex first-class, ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK, ARK Cloud on the same runtime.

## Working notes

- Found while dogfooding v0.8.0: `/tools` and the window's `tools` method leave out
  `web.search` and `web.fetch` (the web is added per turn, not to the engine's registry).

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
