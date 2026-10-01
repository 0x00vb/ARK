# Extending ARK

Everything domain-specific is an **extension**, never a fork. This document lists the seams,
from the one you'll always use (tools + profile) to the deeper ones.

## 1. Tools — the `Adapter` trait

A tool is an `Adapter`: a namespace and a set of `ActionDescriptor`s, each with a JSON-Schema
input and a `RiskTier`. `run(action, input, ctx)` returns an `ActionOutput` (a JSON summary,
the exact command line for reproducibility, and any produced artifacts).

```rust
#[async_trait::async_trait]
impl Adapter for MyTool {
    fn name(&self) -> &str { "mytool" }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("mytool.do", "do the thing", RiskTier::Read)
            .with_capabilities([Capability::new("my-capability")])
            .with_input_schema(serde_json::json!({ "type": "object", "properties": {} }))]
    }
    async fn run(&self, action: &str, input: serde_json::Value, ctx: &RunCtx<'_>)
        -> ark_tools::Result<ActionOutput> { /* … */ }
}
```

- **Risk tiers** drive the approval gate: `Read` and `Observe` auto-run; `Mutate` needs
  approval (denied unless `allow_mutate`). Choose honestly — a tool that spends money or writes
  to a remote system is `Mutate`.
- **`ctx.workspace`** is the project's on-disk scratch dir; confine file writes to it.
- **`ctx.blobs`** resolves an artifact id to bytes/path, so a tool can operate on stored
  artifacts.
- **Produced artifacts** are stored content-addressed with provenance; large output belongs
  here (retrievable on demand), not in the JSON summary.

## 2. The `AgentProfile` — persona, vocabulary, behavior

The profile is the single struct that turns the generic runtime into *your* agent:

| Field | What it injects |
|---|---|
| `prompts.instructions` | The persona + method system block — the agent's identity. |
| `capabilities` | A `CapabilityCatalog` (slug + description + synonyms) the agent reasons in. |
| `inferrer` | Maps action names → capabilities when a descriptor doesn't declare them. |
| `distillers` | A `DistillerSet` of domain result-summarizers (see §3). |
| `knowledge` | An optional `KnowledgeProvider` → enables the `search_knowledge` meta-tool. |
| `core_actions` | Actions kept always-loaded so simple tasks skip discovery. |
| `investigation_markers` | Phrases that mark a task as a full investigation vs. trivial. |
| `prompts.plan_system` / `critic_intro` / `force_answer` | The planner/critic/fallback prompts. |

Build it with `AgentProfile::builder()`; every field has a domain-neutral default.

## 3. Distillers — turning tool output into evidence

A `Distiller` is `Fn(action, output) -> Option<(summary, identifiers)>`. Push domain
distillers onto a `DistillerSet`; the first to return `Some` wins, then ARK's common distiller
(files/exec/edit/diff shapes), then a generic fallback. This is how you keep raw output out of
the prompt without spending a model call.

```rust
let d: Distiller = std::sync::Arc::new(|action, out| match action {
    "mytool.do" => Some((format!("did {} things", out["count"]), vec![])),
    _ => None,
});
let distillers = DistillerSet::with_common().with(d);
```

## 4. Content sniffers — recognizing your formats

Implement `ark_core::Sniffer` and `engine.register_sniffer(...)` to classify ingested bytes
into a domain `ArtifactKind::Other("…")`. ARK ships only the text/json/binary classifier.

## 5. Out-of-process plugins — MCP

Any tool written in any language can join as an **MCP server**. Drop it in
`<store_root>/plugins.json`:

```json
{ "plugins": [ { "name": "entropy", "command": "python3", "args": ["entropy_server.py"] } ] }
```

ARK spawns it, lists its tools, and registers each as `entropy.<tool>` — behind the same
approval gate, invocation log, and evidence model as a native adapter. Risk is derived from
MCP annotations (`readOnlyHint`/`destructiveHint`/`openWorldHint`, or an explicit
`x-ark-risk`).

## 6. Models — the `Provider` trait

Implement `Provider` to add a backend, or use the built-ins. Two-tier routing
(`ModelRouter::new(fast, strong)`) sends planning/summarizing to a cheap model and
reasoning/synthesis to a strong one.

## 7. Knowledge — the `KnowledgeProvider` trait

Implement it to give the agent an external reference corpus (docs, a vector store). When set,
the `search_knowledge` meta-tool appears and the agent is told to cite results separately from
observations.

## 8. The shell: commands, labels, approvals

`ark-cli` is extended from your app, never edited. Add slash commands by implementing
`ark_cli::Command`, label your tools with `ActivityLabels` or a `ToolRenderer`, class approvals
with `AgentProfile::builder().approval_classifier(..)`. Details and examples: [`CLI.md`](CLI.md).

## 9. Reuse of results

An identical successful call may be served from an earlier result. Say how far that holds
for each action:

| Declare | When | Reused |
|---|---|---|
| nothing (`Reuse::Auto`) | most actions | within a turn until something changes state; across turns only a `Read` of a stored artifact |
| `.reusable()` (`Reuse::Stable`) | output depends only on input | always |
| `.volatile()` (`Reuse::Volatile`) | a live capture, a clock, a counter | never |

SecRes marks its live captures (`frida.trace`, `debug.trace`, `debug.memory`,
`debug.attach`) volatile; its static analyses of stored binaries are reused across turns
without declaring anything.

## 10. What the user is looking at

A front-end reports the focused surface and selection as a `Focus`. A `Selection` has
`text` and `line`, and any other marks in the view's own words, at the top level of the JSON:
`{"line": 12}`, `{"address": "0x1000"}`, `{"cell": "B4"}`. The agent sees each as
`name value` and nearby evidence is matched against them. ARK gives none of these names a
meaning.

## What you should *not* do

- Don't add a domain type to `ark-core`. Use `Other(String)`, a capability slug, or a
  distiller instead.
- Don't fork the runtime to change behavior. If you find yourself wanting to, the missing seam
  is a bug — open an issue so the extension API grows instead.
