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

## Next
1. **`ark-cli`** — a reusable CLI framework (start/resume a session, ask, inspect context/tool-calls/usage, approvals, logs, session management) that consumers extend with their own `clap` subcommands. The generic verbs live in ARK; domain commands stay in the consumer. *(The SecRes migration confirmed the shape: `open_engine` + `security_profile_for` + `Harness::ask` is the repeated boilerplate an `ark-cli` would absorb.)*
2. **`ark-ui`** — an optional, themeable conversation shell built against the runtime's event stream: streaming responses, tool-call rendering, an approval prompt, an execution timeline, a context/usage inspector. Consumers use it unchanged, theme it, add panels, or ignore it and build their own against the SDK.
3. **Benchmark harness** — generalize SecRes's `bench/` into a repeatable token/step/tokens-per-finding comparison any consumer can run to show a runtime change didn't regress efficiency.

## Non-goals
- ARK will not grow domain functionality. It stays the machinery; domains are consumers.
- No giant framework. New surface is added only when multiple domains need it.
