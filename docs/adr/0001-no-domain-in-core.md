# 0001. The runtime names no domain, tool or file format

Status: Accepted
Date: recorded 2026-10-03 (decision made at the extraction from SecRes, v0.1)

## Context
ARK was extracted from SecRes, a security research workstation. SecRes had fixed security
vocabularies: nine capabilities, Mach-O and ELF artifact kinds, typed evidence families, a
security persona. Kept in the core, they would make every other app carry them.

## Decision
`ark-core` and the runtime name no tool, file format or domain concept. Domain behavior is
injected through a seam (`Adapter`, `AgentProfile`, `Distiller`, `Sniffer`,
`KnowledgeProvider`, `CapabilityCatalog`, `Provider`), never an `if domain` branch.
Orchestration names no vendor and no org shape (team, manager); apps build those from
`AgentSpec`s.

## Consequences
- Apps supply vocabularies, tools, prompts and distillers.
- A feature one consumer wants goes into the consumer, or into ARK as a generic seam with a
  second domain in view.
- The test question: "would this make sense for an agent that edits novels?"
  `examples/writing-agent` runs on the same runtime with no change to any ARK crate.

## Check
`crates/ark-ui/tests/neutral.rs`; review.
