# Changelog

All notable changes to ARK are documented here. Format: [Keep a Changelog](https://keepachangelog.com);
this project adheres to [Semantic Versioning](https://semver.org).

## [0.3.0]

An interaction layer both front-ends share, and a graphical workspace on it.

### Added
- **`ark-interact`**: the layer between the runtime and any front-end. `App` (an app described
  once), `Interaction` (project, session, approvals, turns, views), `Command` / `Reply` /
  `Effect`, structured `Report`s (`Block`, `Table`, `Tone`, ...), `Labels` / `ToolLabeler` /
  `ActivityLabels`, `Activity`, `trace` (`TurnRecord`, `TurnRecorder`, `interpret`), `inspect`
  (context, usage with optional `Pricing` / `ARK_PRICE`, working state, sessions) and
  `ChannelApprover`. Generic commands gain `/rename` and `/project`.
- **`ark-ui`**: the graphical workspace. `ArkUi` builder (accent, logo, welcome, suggestions,
  assets, scripts, stylesheets, handlers), `Host` with a `call` protocol and an event stream,
  a Tauri window (feature `desktop`, default) and a loopback HTTP server with a per-run token.
  The page's extension API (`window.ark`): panels, artifact viewers, tool renderers, actions,
  status lines, events. See `docs/UI.md`.
- Steering: `Steering`, `TurnOptions::steer`, `Harness::steering`. A message sent while the
  agent works reaches the model at the next step; one that arrives too late is returned in
  `HarnessOutcome::steered` for the next turn. `HarnessEvent::Steered`, `Turn::steered`.
- `Usage::cached_tokens` (Anthropic cache reads, OpenAI `cached_tokens`), summed in `Metrics`
  and `SessionUsage`.
- `HarnessEvent::ToolResult::context_tokens` and `ContextReport::retrieved`: what a result
  added to the context, and evidence fetched on demand shown apart from tool results.
- Sessions: `Session::set_title`, `rename`, `transcript_page`; a `turn_traces` store table
  (`put_turn_trace`, `get_turn_trace`), so a stored turn replays as it ran.
- `Ark::shared_engine()`: read-only store access while a turn runs.
- `HarnessEvent` is `Deserialize`.
- Page tests (`crates/ark-ui/tests/page.rs`): the real page in WebKit, opt-in with
  `ARK_UI_PAGE_TESTS=1`. A neutrality test covering the runtime's code as well as the
  front-end layers.

### Changed (breaking)
- `ark-cli` renders `ark-interact` and holds no command logic. `ArkCli::new(app)` replaces
  `ArkCli::builder()`; the agent, commands, labels and status move to `App`. `Command::run`
  takes `&mut Interaction` and returns `Reply`; `CommandContext` and `CommandOutcome` are gone.
  `ToolRenderer` is now `ark_interact::ToolLabeler`.
- `ark-cli` keys: while the agent works, enter steers and tab queues (enter used to queue).
- New `HarnessEvent` variant `Steered`; `ToolResult` gains `context_tokens`;
  `HarnessOutcome` gains `steered`. Exhaustive matches need the new arm.
- `Ark` holds its `Engine` in an `Arc`.

### Fixed
- A failed result is no longer reused. Dedup served a failed call's card back as "reused
  prior evidence", so a retry after a timeout or a missing tool never ran again. Only a
  successful result is reused; a failure runs again, and repeating the same failure counts as
  no progress for the stall guard.
- Cancelling a tool call kills its whole process group. Before, a shell's children (for
  example the `sleep` in `sh -c "sleep 30"`) outlived the cancelled call.
- An `artifact` argument given as a short id (`art:dc3b0c93c72c`), a prefix or a name is
  resolved to the full id before the adapter sees it. The model is shown short ids, so its
  calls used to fail with "artifact not found".
- Default activity labels no longer carry domain verbs; "Showing show …" and "1 model calls"
  wording.

## [0.2.0]

The interactive shell, and what the runtime needed to support it.

### Added
- **`ark-cli`**: the interactive terminal shell every ARK app opens into. Streaming output,
  semantic activity lines, an approval menu, esc/ctrl+c to stop, input history and multiline
  editing (reedline), sessions and `--resume`, generic slash commands (`/context`, `/usage`,
  `/trace`, `/state`, `/compact`, ...), consumer commands, labels, themes and status sections,
  and a headless `-p` / `--json` / `--events` mode. See `docs/CLI.md`.
- `Provider::complete_streaming` with native streaming for Anthropic, OpenAI and Ollama. The
  default implementation emits the full text once, so every provider works.
- `ApprovalGate`, `Approver`, `ApprovalRequest`, `ApprovalDecision`, `FixedApprover`, and
  `AgentProfile::approval_classifier`: ask a human before `Mutate` actions and remember
  "allow for this session" per approval class.
- `CancelToken` and `Harness::cancel`: cooperative cancellation; `HarnessOutcome::cancelled`.
- `ContextReport`, per-request `HarnessEvent::Context`, and `Harness::preview_context`.
- `Session` (with `CarryPolicy`, `Turn`, `SessionRecord`, `SessionUsage`, `CompactReport`):
  stored conversations with a bounded carry-over, `Session::compact`, and store tables
  `sessions` / `session_turns`. `ResearchState::compact`.
- SDK: `Ark::start_session`, `open_session`, `latest_session`, `ask_session`,
  `context_preview`, `harness`, `router`, `set_router`, `settings`, `find_or_create_project`,
  `ArkBuilder::engine`, `TurnOptions`.
- `ActionDescriptor::reusable`: results that stay valid across turns.
- `ARK_PROVIDER=mock` with `ARK_MOCK_SCRIPT` (and `ARK_MOCK_DELAY_MS`) replays a scripted model
  for end-to-end tests of front-ends. `MockProvider::with_fallback`, `with_chunk_delay`.

### Changed (breaking)
- `HarnessEvent` is owned (`String`/`Value` fields instead of borrows), `Clone` and
  `Serialize`, and has new variants: `Step`, `Context`, `TextDelta`, `ModelCall`,
  `ApprovalRequested`, `ApprovalResolved`, `Cancelled`. `ToolCall` gains `id`; `ToolResult`
  gains `id`, `invocation`, `duration_ms`, `error`; `Answer` gains `streamed`. Match arms that
  bind fields need `..`.
- `HarnessOutcome` gains `cancelled`.
- `ArkBuilder` reads `ARK_CTX_BUDGET` for its default budget, as `Harness::new` already did.

### Fixed
- Consumer distillers now run before ARK's common distiller, as `docs/EXTENSION.md` always
  said. Before, a domain `list` or `read` summary was shadowed by the generic one.
- Evidence is only reused across turns for inputs that cannot change (a content-addressed
  `artifact`, or a `reusable` action). A workspace file read in an earlier session is no
  longer served stale. Within a turn, identical reads are still reused. `Mutate` calls are
  never deduplicated.

## [0.1.3] — unreleased

### Added
- `HarnessOutcome::grounded()` — true when the answer cites at least one piece of evidence.
  A convenience for consumers that gate on grounding.

## [0.1.2] — unreleased

### Added
- `ingest_bytes` now also indexes an `ArtifactKind::Other(_)` artifact for full-text search
  when its bytes are valid UTF-8 (previously only `is_textual()` kinds were indexed). Lets a
  consumer's text-bearing domain format (a plist, a JSON trace, an outline) be searchable
  without ARK knowing the format. Binary `Other` bytes fail the UTF-8 guard and are skipped.

### Changed
- `AcquisitionProvider` is now an **async** trait (`find`/`propose`/`acquire` are `async fn`),
  since acquiring a provider is inherently I/O (probe, install, connect). Refines the seam
  introduced in 0.1.1 before it ships in a released consumer.

## [0.1.1] — unreleased

### Added
- **Capability-acquisition seam** (async `AcquisitionProvider` trait, `AcquisitionResult`), injected
  via `AgentProfile::acquisition`. When a profile supplies one, the runtime enables the
  `find_capability` / `propose_capability` / `acquire_capability` meta-tools and dispatches to
  it (acquisition gated by the same approval as a `Mutate` action). Previously these meta-tools
  were stubbed as "not enabled". Mirrors the existing `KnowledgeProvider` seam and stays
  domain-agnostic (a security agent installs a disassembler; a writing agent could wire a
  thesaurus). Discovered while migrating the security workbench onto ARK.
- **`HarnessEvent::Capability { action, detail }`** — emitted when an acquisition meta-tool
  runs, so a UI can surface it. This is additive; match arms should already carry a `_ =>`.

## [0.1.0] — unreleased

### Added
- Initial extraction of a domain-agnostic agent runtime from a security-research workstation.
- Crates: `ark-core`, `ark-model`, `ark-store`, `ark-tools`, `ark-runtime`, `ark-sdk`.
- Two agent loops (`Agent`, `Harness`) with a managed context budget, model routing,
  deterministic distillation, progressive tool disclosure, a persistent working state +
  investigation graph, a self-check + critic, an approval gate, and full metrics.
- Model providers: Anthropic, OpenAI(-compatible), Ollama, and a scriptable Mock.
- Out-of-process tools via an MCP plugin bridge.
- `AgentProfile` as the single injection point for domain persona, capabilities, distillers,
  and knowledge.
- Example agents: `minimal-agent`, `writing-agent`.
- Docs: README, architecture, extension guide, versioning, roadmap, extraction map.

The security-specific vocabularies of the source (a fixed capability enum, binary/RE artifact
kinds, typed RE/network evidence families, the security persona and distillers) were removed
from the core and made consumer-supplied.
