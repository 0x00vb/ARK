# Changelog

What changed for people building on ARK, per release. Not a commit log.

- Sections: `Added`, `Changed`, `Fixed`, `Breaking`. A `Breaking` entry says what an app must
  change.
- New entries go under `[Unreleased]`; a release renames it (see `docs/RELEASING.md`).
- One line per change where possible. Link to a doc instead of explaining it here.

## [Unreleased]

## [0.5.0] - 2026-10-01

### Added
- Subscription models through the vendor's official CLI: providers `claude` (Claude Code,
  Claude subscription) and `codex` (Codex, ChatGPT plan), e.g. `--model claude:sonnet`. ARK
  runs the tools; the CLI's own tools are off. See `docs/OPERATIONS.md#subscriptions`.
- `ark_model::catalog()`: the backends usable on this machine (signed-in subscriptions,
  Ollama models, APIs with a key) and the command that fixes one that is not ready.
- Workspace: the model menu lists those backends and switches with one click. New protocol
  method `models`. `/model` with no argument lists them too.

### Changed
- The model chip and `Summary.model` name the model only (`ModelRouter::label`), without
  "(single tier)". `/model` still shows the routing.
- Documentation reorganized for maintainers: `AGENTS.md` (rules and change lifecycle; `CLAUDE.md`
  imports it), `ROADMAP.md` at the root (state, limits, next), `docs/RELEASING.md` (release,
  app upgrade, rollback; replaces `docs/VERSIONING.md`), diagrams in `docs/ARCHITECTURE.md`,
  provider and development environment variables in `docs/OPERATIONS.md`.
  `docs/EXTRACTION_MAP.md` is folded into `docs/ARCHITECTURE.md` (Origin).
- Comments that described the extraction history now describe the code. No library change.

## [0.4.1] - 2026-10-01

### Fixed
- `scripts/ci.sh` ran the benchmark from `./target/release` even when `CARGO_TARGET_DIR`
  pointed elsewhere, so `scripts/verify-release.sh` (which honors `CARGO_TARGET_DIR`) failed
  at its last step. Found by verifying v0.4.0 from a fresh clone. No library change.

## [0.4.0] - 2026-10-01

Production hardening, measurement, and a cheaper harness. No new front-end surface.
Before/after numbers are in `bench/RESULTS.md`; the benchmark harness is in `docs/BENCH.md`
and running ARK in production in `docs/OPERATIONS.md`.

### Added
- **`ark-bench`**: a benchmark and eval harness for any ARK application. Workloads, a metered
  provider that records every request (size by part, measured usage, latency, first token,
  prefix shared with recent requests), a simulated model whose policy reacts to what each
  request contains, JSON reports and comparisons with regression gating, and a generic suite
  of 13 workloads. `bench/at-commit.sh` builds the current bench against an earlier commit.
- Provider resilience: `ProviderError::{Status, Overloaded, Malformed}` and
  `is_transient()`; `HttpConfig` (connect, idle and request timeouts, output cap);
  `RetryPolicy` (`ARK_MODEL_RETRIES`, default 2) used by the harness; `HarnessEvent::Retry`;
  `ModelCall.retries`, `Metrics.provider_retries`.
- `Reuse { Auto, Stable, Volatile }` on `ActionDescriptor` (`.reusable()`, `.volatile()`).
- `get_evidence` takes `match` and `offset`; `ark_runtime::evidence`.
- Context reports for every model call: `ContextReport.role`, `model_output`,
  `action_schemas`; the planner, critic and forced answer emit one.
- `Session::begin_turn` / `fail_turn`, `Turn.{incomplete, error, pid}`,
  `Engine::recover_interrupted`, `Invocation.pid`, `session::INTERRUPTED`.
- `ark_interact::logging` (`ARK_LOG`, file or stderr), `TraceMode` (`ARK_TRACE`),
  `TRACES_KEPT`; `Store::{project_stats, session_text_chars, prune_turn_traces,
  list_running_invocations}`.
- `TurnOptions::present`, `Harness::presentation`, `Interaction::set_presentation`,
  `Ark::context_preview_for`.
- `Usage::cache_write_tokens`, `Usage::uncached()`, `Metrics.cache_write_tokens`.
- Events: `HarnessEvent::Stall`, `ModelCall.first_token_ms`, `ToolResult.raw_tokens`.
- `scripts/ci.sh`, `scripts/check-version.sh`, `scripts/verify-release.sh`,
  `.github/workflows/ci.yml`.

### Breaking
- `ActionDescriptor::reusable: bool` is replaced by `reuse: Reuse`. `.reusable()` still
  marks an action `Stable`.
- `Selection` keeps `text` and `line`; `address`, `symbol` and `function` become named
  `marks` (flattened in JSON, so `{"address": "0x1000"}` still parses). Use
  `Selection::mark(name)`.
- The runtime tools offered by default are `load_action`, `get_evidence`, `search_evidence`
  and `record_state`. `list_actions` and `record_note` are no longer offered (still handled);
  `present` is offered only when the front-end can present.
- Request layout: the working state is the last message of each request instead of part of
  the system prompt; the system prompt is fixed for a turn and ordered instructions, index,
  project; loaded tools keep their load order; artifacts are listed as `art:<12 hex>`, at
  most 12. Default prompt text changed accordingly (a MINOR behavior change by our policy;
  in 0.x it ships with the breaking release).
- Anthropic requests carry `cache_control` breakpoints and merge consecutive messages of
  one role; the default output cap is 8192 (was 2048). Ollama sends `num_predict` (4096).
- New fields on `HarnessEvent::ModelCall` and `ToolResult`, new variants `Retry` and `Stall`:
  exhaustive matches need the new arms.
- The default activity labeler no longer treats `process`, `function` or `symbol` arguments
  as the subject.
- A tool call whose arguments are not valid JSON is `ProviderError::Malformed`, not `{}`.

### Fixed
- The stall guard counted every retrieval as progress, so a model repeating one search ran
  to the step limit.
- A read after a write in the same turn could be served the stale first result.
- A short artifact id and the full id were different calls to the dedup check.
- `get_evidence` could not reach rows past the first 40 of a large output.
- With no output cap and Ollama's context shifting, one degenerate reply ran for over 20
  minutes; there was no request or idle timeout on any provider.
- The common distiller summarized any `*.read` / `*.write` / `*.list` action as a file
  operation and dropped its fields (`notes.read` became "read  (0 lines)").
- The working-state digest showed the oldest six items of each section, not the newest.
- After every turn the workspace loaded the whole transcript and every stored tool output to
  count them; memory and turn time grew with each turn.
- A turn killed mid-run, or ending in an error, vanished from the transcript; a tool call
  killed mid-run left no record.
- The browser event stream reconnected every few seconds forever, and a reconnect during a
  turn could lose or repeat events.
- The HTTP server spun when `accept` kept failing.
- Warnings (for example a plugin that failed to load) went nowhere: no logger was installed.
- A reply that writes a tool call as text (`record_state({...})`) is no longer taken as the
  final answer: the model is told once that nothing ran and asked again. With the working
  state as the last message, qwen3:4b did this in 4 of 39 live runs.

## [0.3.0] - 2026-09-30

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

### Breaking
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
- `-p` printed the answer twice after the model narrated before a tool call (the final
  answer was compared with all streamed text, narration included).
- Default activity labels no longer carry domain verbs; "Showing show …" and "1 model calls"
  wording.

## [0.2.0] - 2026-09-30

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

### Breaking
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

## [0.1.3] - 2026-09-30

### Added
- `HarnessOutcome::grounded()`: true when the answer cites at least one piece of evidence.
  A convenience for consumers that gate on grounding.

## [0.1.2] - 2026-09-30

### Added
- `ingest_bytes` now also indexes an `ArtifactKind::Other(_)` artifact for full-text search
  when its bytes are valid UTF-8 (previously only `is_textual()` kinds were indexed). Lets a
  consumer's text-bearing domain format (a plist, a JSON trace, an outline) be searchable
  without ARK knowing the format. Binary `Other` bytes fail the UTF-8 guard and are skipped.

### Breaking
- `AcquisitionProvider` is now an **async** trait (`find`/`propose`/`acquire` are `async fn`),
  since acquiring a provider is inherently I/O (probe, install, connect). Refines the seam
  introduced in 0.1.1 before it ships in a released consumer.

## [0.1.1] - 2026-09-30

### Added
- **Capability-acquisition seam** (async `AcquisitionProvider` trait, `AcquisitionResult`), injected
  via `AgentProfile::acquisition`. When a profile supplies one, the runtime enables the
  `find_capability` / `propose_capability` / `acquire_capability` meta-tools and dispatches to
  it (acquisition gated by the same approval as a `Mutate` action). Previously these meta-tools
  were stubbed as "not enabled". Mirrors the existing `KnowledgeProvider` seam and stays
  domain-agnostic (a security agent installs a disassembler; a writing agent could wire a
  thesaurus). Discovered while migrating the security workbench onto ARK.
- **`HarnessEvent::Capability { action, detail }`**: emitted when an acquisition meta-tool
  runs, so a UI can surface it. This is additive; match arms should already carry a `_ =>`.

## [0.1.0] - 2026-09-30

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
