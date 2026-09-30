# ARK Roadmap

## Shipped (v0.1)
- Headless runtime: Engine, Harness, Agent, approval gate.
- Token/context optimization: round-based budget, distillation, progressive disclosure, dedup, working-state resume, model routing.
- Abstractions: `Provider` (models), `Adapter`/`Registry` (tools), `Store` (persistence), `CapabilityCatalog`, `DistillerSet`, `Sniffer`, `KnowledgeProvider`.
- Extension model: adapters + MCP out-of-process plugins + `AgentProfile`.
- Observability: `Metrics` + typed `HarnessEvent` stream.
- SDK facade (`Ark::builder()`), docs, and two unrelated example agents.

## Shipped (v0.1.1 – v0.1.3, migration-driven)
- **Capability acquisition** — the `find/propose/acquire_capability` flow is now an injectable
  seam (`AcquisitionProvider`, async), enabled per-profile; `HarnessEvent::Capability` surfaces it.
- `ingest_bytes` indexes UTF-8 `ArtifactKind::Other(_)` for search; `HarnessOutcome::grounded()`.
- **Origin migration DONE** — the security workstation (SecRes) now consumes ARK as a
  git-tag-pinned dependency; ~5,300 lines of generic machinery deleted from it; behavior and
  token efficiency verified unchanged (its `bench/COMPARISON.md`). ARK now has **three** unrelated
  consumers: minimal-agent, writing-agent, and the security workbench.

## Shipped (v0.2.0)
- **`ark-cli`**: the interactive shell. Launching an app enters a persistent session with
  streaming, approvals, cancel, history, resume, slash commands, context and usage inspection,
  and a headless `-p` mode. Consumers add commands, labels, themes and status sections.
- Runtime seams it needed: streaming providers, owned serializable events, `ApprovalGate`,
  `CancelToken`, `ContextReport`, `Session` with bounded carry-over and compaction.
- Two apps on it with no shell changes: `writer` (in-tree) and `secres`.

## Next
1. **`ark-ui`**: an optional, themeable GUI shell on the same seams `ark-cli` uses (events, `ApprovalGate`, `CancelToken`, `Session`). Streaming, tool activity, approval dialogs, a timeline, a context inspector.
2. **Benchmark harness**: generalize SecRes's `bench/` into a token and step comparison any consumer can run to show a runtime change did not regress efficiency.

## Non-goals
- ARK will not grow domain functionality. It stays the machinery; domains are consumers.
- No giant framework. New surface is added only when multiple domains need it.
