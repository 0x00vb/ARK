# ARK Extraction Map

ARK (Agent Runtime Kit) is extracted from an internal security-research workstation
("Workbench", a Rust workspace of `wb-*` crates). This document is the Phase-1 audit:
it classifies every source module as **GENERIC** (belongs in ARK), **DOMAIN** (stays
in the security product), or **MIXED** (a generic mechanism wrapped around a
security-specific vocabulary that must be split), and records how each mixed module is
separated.

The source is unusually clean: its ADR-0002 already forbids tool/platform names in the
core crate. So the extraction is mostly *opening fixed security vocabularies into
extensible registries* and *injecting the system prompt* — not rewriting machinery.

## The test we hold ourselves to

> Could this exact harness power an agent whose only job is helping an author write a
> novel?

If a type, prompt, or default answers "no", it is domain-specific and must move out or
become a consumer-provided extension.

## Crate-by-crate classification

| Source crate | Lines | Verdict | Extracted to | Notes |
|---|---|---|---|---|
| `wb-core` | ~2000 | MIXED | `ark-core` | Shapes are generic; enumerated vocabularies leak security. |
| `wb-providers` | ~680 | GENERIC | `ark-model` | Model abstraction. Rename `WB_*` env → `ARK_*`. |
| `wb-store` | ~635 | GENERIC | `ark-store` | Content-addressed blobs + SQLite index + FTS. Already agnostic. |
| `wb-adapters` (trait layer) | ~400 | GENERIC | `ark-tools` | `Adapter`, `Registry`, `RunCtx`, `BlobResolver`, `subprocess`, `mcp`. |
| `wb-adapters` (concrete) | ~2300 | DOMAIN | (security product) | macho/ghidra/frida/r2/entitlements/symbols/strings/web. |
| `wb-device` | ~980 | DOMAIN | (security product) | iOS device conn + lldb debug. |
| `wb-agent` (runtime) | ~4000 | MIXED | `ark-runtime` | Loop/budget/router/distill/metrics generic; prompts + distillers + knowledge domain. |
| `wb-agent::knowledge` | ~260 | DOMAIN | (security product) | Security knowledge base. Runtime keeps a `KnowledgeProvider` trait seam. |
| `apps/wb-cli` | ~770 | MIXED | `ark-cli` (later) | CLI framework generic; security subcommands stay in the product. |
| `apps/wb-desktop` | ~570 | MIXED | `ark-ui` (later) | Conversation shell + PTY generic; RE surfaces domain. |

## The `wb-core` split (MIXED → `ark-core` + domain)

| Item | Verdict | Disposition in ARK |
|---|---|---|
| `ids` (ArtifactId, ProjectId, …) | GENERIC | Kept verbatim. |
| `error` (CoreError) | GENERIC | Kept. |
| `action` (ActionDescriptor, RiskTier) | GENERIC | Kept. `RiskTier` (Read/Observe/Mutate) is already platform-neutral. |
| `invocation` (Invocation, status) | GENERIC | Kept — the reproducibility spine. |
| `project` (Project, Note, NoteKind, Author) | GENERIC | Kept. NoteKind is research-methodology, not security. |
| `artifact` (Artifact, Provenance) | GENERIC | Kept. |
| `artifact::ArtifactKind` | **MIXED** | **Split.** Keep universal variants (Text, Json, Binary, Image, Source, Archive, Blob) + `Other(String)`. Security formats (Macho/Dsc/Elf/Plist/Crash/Trace/Har/Pcap) become `Other("macho")` etc., registered by the consumer. |
| `capability::Capability` | **MIXED** | **Split.** The *concept* (reason in capabilities, not tool names) is the crown jewel and is generic; the *enum* (Decompilation, DynamicTracing, Debugging…) is security. Becomes an open `Capability(slug)` newtype + a consumer-provided `CapabilityCatalog` of `{ slug, describes }`. |
| `context` (WorkspaceContext) | GENERIC | Kept. |
| `surface` (SurfaceKind, Focus, PresentDirective) | MIXED (mild) | Kept; `SurfaceKind` already has `Other()`. Decompiler/Hex/Trace variants retained as generic view roles (a writing agent simply never uses them) but documented as optional. |
| `research` (EvidenceCard, ResearchState, Claim, graph) | GENERIC | Kept — this is domain-neutral research methodology. |
| `evidence` (Function, Xref, Decompilation, Trace, Dns, Tls, Http, NetHost, SourceLocation, Diff) | **DOMAIN** | **Moved out.** These are RE/network evidence families. ARK core keeps only the generic `EvidenceCard`. Typed families live in the security product (or optional `ark-evidence-*` community crates). |

## The `wb-agent` split (MIXED → `ark-runtime` + domain)

| Module | Verdict | Disposition |
|---|---|---|
| `engine` (Engine: ingest/run_action/approval gate) | GENERIC | Kept. `Registry::with_builtins()` removed — the consumer registers adapters. `detect_kind` reduced to a generic text/json sniffer + a pluggable `Sniffer` hook. |
| `budget` (WorkingSet: round-based context compaction) | GENERIC | Kept verbatim — a crown-jewel token optimization. |
| `router` (ModelRouter, Role) | GENERIC | Kept. `WB_MODEL_*` → `ARK_MODEL_*`. |
| `metrics` (Metrics) | GENERIC | Kept verbatim. |
| `blobs` (StoreBlobs bridge) | GENERIC | Kept. |
| `catalog` (meta-tools, capability index, progressive disclosure) | GENERIC | Kept. `search_knowledge` meta-tool gated behind a `KnowledgeProvider` seam. |
| `distill` (mechanism) | GENERIC | Kept: `Distiller` fn type, ordered registry, `generic_summary` fallback, `call_digest`. |
| `distill` (binary/device/dynamic/network/source distillers) | DOMAIN | Moved out — registered by the consumer via `DistillerSet`. |
| `capmap` (adapter→capability coverage) | GENERIC | Kept (operates over the consumer's capability catalog). |
| `acquire` (capability acquisition from specs) | GENERIC | Kept (reads a consumer `capabilities.json`). |
| `knowledge` (security KB search + citation) | DOMAIN | Moved out; runtime exposes a `KnowledgeProvider` trait. |
| `agent` (simple ReAct loop) | MIXED | Kept as `Agent`; hardcoded "Workbench security partner" system prompt → `AgentProfile.instructions`. |
| `harness` (advanced loop) | MIXED | Kept as `Harness`; all prose prompt text (`static_system`, `draft_plan`, critic) → composable `Prompts` with generic defaults, overridable by the consumer. `classify_trivial` keywords (vulnerab/exploit) generalized. |
| `notebook` (Markdown export) | MIXED | Kept; generic invocation/note export. Any security phrasing dropped. |
| `eval` (harness self-eval) | GENERIC | Kept as an optional testing aid. |
| `recipes` (canned research loops) | DOMAIN | Moved out — recipes are domain workflows the consumer registers. |

## Hidden coupling checklist (audited)

- **Env vars**: `WB_PROVIDER`, `WB_MODEL`, `WB_MODEL_{FAST,STRONG}`, `WB_CTX_BUDGET` → `ARK_*`.
- **Prompts**: the only security identity is in `agent::system_prompt` and
  `harness::static_system`/`draft_plan`/`run_critic`. All injected via `AgentProfile`.
- **Tool-name strings** in `catalog`/`harness` meta-tools are generic ("adapter", "action").
- **`classify_trivial`** keys on `vulnerab`/`exploit` — generalized to a configurable set.
- **`detect_kind`** hardcodes Mach-O/ELF magic — moved behind a `Sniffer` hook; ARK ships a text/json sniffer only.
- **Storage schema** is generic JSON-blob-per-row; no security columns. Kept as-is.
- **Events** (`HarnessEvent`, `AgentEvent`) name no tool; kept.

## Dependency direction (must hold)

```
ark-core  ← ark-model, ark-store, ark-tools
ark-core, ark-model, ark-store, ark-tools  ← ark-runtime
ark-runtime  ← ark-sdk
ark-sdk  ← consumers (security-product, writing-agent, minimal-agent)
```

No ARK crate ever depends on a consumer. `ark-core` depends on nothing but serde/uuid/blake3/time.
