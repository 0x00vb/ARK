# 0027. Named OpenAI-compatible endpoints, and out of credit as its own error

Status: Accepted
Date: 2026-10-05

## Context
Many model services speak the OpenAI chat API: routers (OpenRouter, LiteLLM), servers (vLLM),
and a hosted gateway an app may sell. ARK reached them only through the `openai` provider and
one `OPENAI_BASE_URL`, so a person could use one such service at a time, in place of OpenAI.

Dogfooding a gateway showed two real failures:
- A model spec starts with a provider name when it has one (`claude:sonnet`). A gateway model
  named `claude` therefore ran on the Claude Code subscription. `ARK_PROVIDER=openai` with
  `ARK_MODEL=openai:claude` sent `openai:claude` as the model id.
- A prepaid service answers HTTP 402 when the balance is used up. ARK treated it as one more
  client error: "openai returned 402: ...", with the generic "try again" hint. Trying again
  cannot help.

## Decision
An app (or the environment) names OpenAI-compatible endpoints. A name is a provider.

| Question | Decision |
|---|---|
| API | `ark_model::endpoints::register(Endpoint::new(name, base_url).label(..).key(..))`, `unregister`, `endpoints`, `endpoint`, `is_provider`. Process-wide, like `keys::set_source` |
| Environment | `ARK_ENDPOINTS=name=url,name=url`. A registered endpoint wins over one of the same name |
| Key | Given by the app, else `<NAME>_API_KEY` read like every API key: the environment, then the app's key store |
| Names | Lowercase letters, digits, `-`, `_`. A built-in provider's name is refused, so an endpoint never shadows `claude` or `ollama` |
| Provider | The existing `OpenAi` provider with the endpoint's base URL, key and name (`describe()` is `name model`). Same streaming, retries, usage |
| Models | The endpoint's `GET /models`, reused 5 minutes (20 s after a failure), 3 s timeout. `catalog()` lists the endpoint as `BackendKind::Endpoint`; one that cannot be reached is not ready, and the rest of the list is unchanged |
| 402 | `ProviderError::OutOfCredit { provider, detail }` from any HTTP provider, not transient. The harness stops the turn with `AgentError::OutOfCredit`; its message is the provider's own words |
| Front-ends | `TurnEnd::out_of_credit()` gives `OutOfCredit { provider, message, hint }`, computed once in `ark-interact`. The terminal prints the hint under the error; the window adds it as a notice; `turn_finished` carries `out_of_credit` for other clients |

ARK names no endpoint. Which endpoints exist, and how a key reaches the key store, is the app's.

## Consequences
- Easier: several OpenAI-compatible services at once, each in the model picker with its own
  models; an app that sells models registers one endpoint and needs no ARK change.
- An endpoint whose name equals the first part of an Ollama model id (`qwen3` in `qwen3:4b`)
  would take that spec. Name endpoints after the service, not after a model family.
- The hint says what to do in words. An app that knows where credit is added (a web page)
  shows its own button from `out_of_credit.provider`.
- `ProviderError` and `AgentError` gained a variant: code that matches them exhaustively adds an
  arm (`Breaking` in the changelog).

## Check
`crates/ark-model/tests/endpoints.rs` (a fake endpoint: key, model id, model list, 402, an
unreachable endpoint) and `crates/ark-interact/tests/out_of_credit.rs` (one call, no retry,
the hint; `name:claude` is not Claude Code).
