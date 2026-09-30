# ARK Roadmap

## Shipped (v0.1)
- Headless runtime: Engine, Harness, Agent, approval gate.
- Token/context optimization: round-based budget, distillation, progressive disclosure, dedup, working-state resume, model routing.
- Abstractions: `Provider` (models), `Adapter`/`Registry` (tools), `Store` (persistence), `CapabilityCatalog`, `DistillerSet`, `Sniffer`, `KnowledgeProvider`.
- Extension model: adapters + MCP out-of-process plugins + `AgentProfile`.
- Observability: `Metrics` + typed `HarnessEvent` stream.
- SDK facade (`Ark::builder()`), docs, and two unrelated example agents.

## Next
1. **`ark-cli`** — a reusable CLI framework (start/resume a session, ask, inspect context/tool-calls/usage, approvals, logs, session management) that consumers extend with their own `clap` subcommands. The generic verbs live in ARK; domain commands stay in the consumer.
2. **`ark-ui`** — an optional, themeable conversation shell built against the runtime's event stream: streaming responses, tool-call rendering, an approval prompt, an execution timeline, a context/usage inspector. Consumers use it unchanged, theme it, add panels, or ignore it and build their own against the SDK.
3. **Capability acquisition** — port the `find/propose/acquire_capability` flow (need → discover provider → approve → install → use) as an optional module reading a consumer `capabilities.json`.
4. **Benchmark harness** — a repeatable token/step/tokens-per-finding comparison so a runtime change can be shown not to regress efficiency.
5. **Origin migration** — migrate the security workstation to consume ARK: security tools, prompts, distillers, capabilities and UI stay local; generic behavior comes from ARK. Dependency direction: `security-product → ark`, never the reverse.

## Non-goals
- ARK will not grow domain functionality. It stays the machinery; domains are consumers.
- No giant framework. New surface is added only when multiple domains need it.
