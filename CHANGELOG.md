# Changelog

All notable changes to ARK are documented here. Format: [Keep a Changelog](https://keepachangelog.com);
this project adheres to [Semantic Versioning](https://semver.org).

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
