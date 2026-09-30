# Changelog

All notable changes to ARK are documented here. Format: [Keep a Changelog](https://keepachangelog.com);
this project adheres to [Semantic Versioning](https://semver.org).

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
