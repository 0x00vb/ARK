# 0080. OpenCode Go uses explicit conversations

Status: Accepted. Prior independent plan/design/source review is retained; current integration is qualified with disclosed cold author review on 2026-10-30.
Date: 2026-10-23

## Context

OpenCode Go documents direct access for other coding agents. It recommends the client's
own user agent and a stable conversation header. ARK's HTTP providers share instances
between chats. A static header would merge unrelated conversations.

The public Go model list returns identifiers without API-family metadata. A public list
also cannot verify a key or an active subscription.

## Decision

| Boundary | Behavior |
|---|---|
| Provider | `opencode-go` in `ark-model`, using the existing OpenAI Chat Completions transport |
| Models | `glm-5.3-flash`, `kimi-k2.7-code`, `deepseek-v4.1-flash`, intersected with Go discovery. Other identifiers fail locally |
| Inference | `https://opencode.ai/inference/go/openai/v1/chat/completions` |
| Discovery | `https://opencode.ai/inference/go/v1/models`, public, bounded, no credential |
| Key | `OPENCODE_GO_API_KEY`, then the existing Secrets-backed saved key. Read again for each completion |
| Binding | `Provider::for_conversation(&self, conversation: &str) -> Result<Option<Arc<dyn Provider>>>`. Default `Ok(None)` preserves the provider. A bound view replaces its own scope without changing shared instances |
| Router | `ModelRouter::for_conversation` binds distinct tiers once. Errors propagate. Shared single-tier identity stays shared |
| Conversations | Persisted chat ID or project-scoped agent lane, namespaced. Go sends only a deterministic BLAKE3 digest, never the raw agent name |
| Auxiliary calls | The same bound router covers planner, critic, summaries, tool selection and retries |
| One-shot API | `ask` and `ask_with` allocate one scope per run. `harness_for_conversation` takes an explicit scope. Unbound Go completion fails before HTTP |
| Credentials | Saving Go keys performs only input checks and returns `verified: false`. The first real task checks inference access. No public request is presented as authentication |
| Failures | Keep the selected model and provider detail. Existing bounded temporary retries apply. No automatic provider or billing-endpoint fallback |
| UI | Existing provider list and password field. Say Key saved or Environment key set. Key removal works while discovery is unavailable |

## Limits

Go is presented for coding work. This decision does not establish acceptance of general-purpose
traffic. ARK identifies itself truthfully and never reads another program's credentials.

OpenCode's Extra usage setting can spend Console credit after included limits. ARK cannot
promise that the Go URL prevents these charges. The connection flow discloses this behavior.
No remaining quota, reset time or active entitlement is fabricated.

## Alternatives

A global `OPENAI_BASE_URL` would replace another provider and lose its identity. Extending
named endpoints into a full multi-protocol catalog is larger than this initial integration.
Running OpenCode through ACP changes the execution owner and is a separate initiative.

## Verification

Local HTTP tests cover headers, immutable scopes, streaming, tools, usage, status errors,
redirect refusal and filtered public discovery. SDK and orchestration tests cover the binding
callers. Saved-key and real WebKit tests cover connection state and recovery.
Authenticated Go inference and installed-app acceptance remain separate gates until recorded.

Sources: [Go](https://opencode.ai/v2/docs/console/go),
[Harnesses](https://opencode.ai/v2/docs/console/harnesses/),
[API keys](https://opencode.ai/v2/docs/console/api).
