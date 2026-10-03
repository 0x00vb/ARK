# Current state

What is happening in ARK right now. Read this at the start of a session. Update it at the end
of a session that changed what is in progress, blocked or next. Keep it under a page: history
goes in git and `CHANGELOG.md`, direction in `ROADMAP.md`, decisions in `docs/adr/`.

Last updated: 2026-10-03

## Release

| | |
|---|---|
| Latest tag | v0.7.0 (2026-10-03), on `origin` and `github` |
| In progress | the measured-runtime milestone is closed at v0.7.0; what it left open (the Haiku check, `parallel` and `team` overhead, output tokens) is under "Deferred" in `ROADMAP.md`. Since v0.7.0 on `main` (`[Unreleased]`): MCP plugins added and removed while ARK runs (`/plugin`, Settings › Tool servers), skills read on demand (`/skills`, Settings › Skills, ADR 0009), and the web (`web.search`, `web.fetch`, ADR 0010; step C1 of ROADMAP › Capabilities) |

## Waiting on this repo

| Who | Waits for |
|---|---|
| ark-desktop | nothing for its current work (v0.7.0). The next tag brings it Settings › Tool servers and Settings › Skills with no change of its own; per-agent skill switches in its agent inspector need that tag too (`AgentSpec::skills`, method `skills`). The web needs one line once on that tag: `.web(true)` on its `Ark::builder()` |
| SecRes | nothing; pins v0.5.0 |

## Next

Prove ARK as a product, in this order ([`ROADMAP.md`](ROADMAP.md) › Next): v0.8.0 (API trim,
cleanup, SecRes upgraded), Claude Code and Codex first-class, ARK Desktop public, hosted-model
evidence (time-boxed), persistent headless ARK, ARK Cloud on the same runtime.

## Working notes

- Several Claude sessions may edit this tree at once. Agree a file split before editing (see
  `../WORKSPACE.md`, Sessions).
- `.playwright-mcp/` is scratch output from browser dogfooding. It is untracked; delete it when
  done.
