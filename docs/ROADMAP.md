# ARK Roadmap

## Shipped (v0.1)
- Headless runtime: Engine, Harness, Agent, approval gate.
- Token/context optimization: round-based budget, distillation, progressive disclosure, dedup, working-state resume, model routing.
- Abstractions: `Provider` (models), `Adapter`/`Registry` (tools), `Store` (persistence), `CapabilityCatalog`, `DistillerSet`, `Sniffer`, `KnowledgeProvider`.
- Extension model: adapters + MCP out-of-process plugins + `AgentProfile`.
- Observability: `Metrics` + typed `HarnessEvent` stream.
- SDK facade (`Ark::builder()`), docs, and two unrelated example agents.

## Shipped (v0.1.1 – v0.1.3, migration-driven)
- **Capability acquisition**: the `find/propose/acquire_capability` flow is now an injectable
  seam (`AcquisitionProvider`, async), enabled per-profile; `HarnessEvent::Capability` surfaces it.
- `ingest_bytes` indexes UTF-8 `ArtifactKind::Other(_)` for search; `HarnessOutcome::grounded()`.
- **Origin migration done**: the security workstation (SecRes) now consumes ARK as a
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

## Shipped (v0.3.0)
- **`ark-interact`**: the interaction layer. `App`/`Interaction`, commands returning structured
  `Report`s, activity labels, traces, context/usage/state views. `ark-cli` was rewired onto it.
- **`ark-ui`**: the graphical workspace (Tauri window or local browser) on the same
  `Interaction`. SecRes's desktop app runs on it with 14 app-owned surfaces.
- Runtime seams: steering, persisted turn traces, cached-token accounting, session rename and
  paging, short artifact ids in tool input, process-group cancellation, and no reuse of a
  failed result.

## Shipped (v0.4.0)
- **`ark-bench`**: workloads, a metered provider, a simulated model that reacts to each
  request, JSON reports and comparisons with regression gating, a generic suite of 13
  workloads. SecRes has its own suite on it.
- Measured cost work: a request layout a prefix cache can serve, Anthropic cache
  breakpoints, a smaller runtime tool set, filtered evidence reads, search results with their
  matching lines, parallel reads, a stall guard that counts only new information.
- Hardening: timeouts, output caps, classified errors and safe retries; explicit reuse
  semantics; recovery after a kill mid-turn; flat memory and turn time over thousands of
  turns; bounded reconnect and numbered events in the workspace; structured logs without
  content.
- Release engineering: `scripts/ci.sh`, a fresh-clone release check, immutable tags.

Measured and not adopted: action summaries in the capability index (more tokens, no success
gain), routing planning to a smaller model, turning the planner off (both lowered success).
Numbers in `bench/RESULTS.md`.

## Next
1. **Native window tests**: the page is tested in WebKit and the shipped bundle is checked
   through its browser transport; the Tauri IPC path has no automated test.
2. **Live prompt-cache measurement on Anthropic and OpenAI**: the breakpoints are covered by
   unit tests and the simulated cache model, not yet by provider-reported cache reads.
3. **Tool choice for small models**: with qwen3:4b, two generic tasks still fail on picking
   the application's tool over a plausible neighbor.

## Non-goals
- ARK will not grow domain functionality. It stays the machinery; domains are consumers.
- No giant framework. New surface is added only when multiple domains need it.
