# 0089. A client chooses only models its runtime offers

Status: Accepted
Date: 2026-10-23
Amends: 0014 (agents follow the session model), 0057 (external-client protocol)

## Context

A phone may create or continue a chat on several computers. Each computer has different model
programs, account state, local models and endpoints. A client-side model list would advertise
choices that the selected computer cannot run. A display name is not enough to bind a retry to an
exact choice.

The existing runtime can switch a conversation's router. Its session record did not preserve the
selected model, so another client could not show the current choice after a restart.

## Decision

Protocol v1.7 adds `models.select`. `GET /client/v1/models` returns only ready models for the
addressed route. Each model has an opaque spec issued by that runtime and verified effort choices.
The response also gives the runtime's current default display. It gives a default selection only
when the default is one lossless catalog choice. A client must not turn a two-tier router into a
made-up single model.

A new chat may include a model selection. The runtime validates and builds that choice before it
writes a session. The deterministic session's first record stores the canonical model with the
project. A retry with the same device, request id, project and model returns the same chat. A
retry that changes the project or model is refused.

An idle existing chat changes model through the ordered `chat.model` mutation. The runtime builds
the new router before admission, stores the canonical model, then installs the router. The receipt
is terminal only after that work completes or is refused. A restart rehydrates the router from the
stored model. Conversation snapshots expose the current selection when it still maps exactly to
the ready catalog. Older session records and older clients continue without a model field.

Provider commands, credential paths and endpoint secrets never enter the catalog. Disconnected
providers remain in the separate connection snapshot. A missing capability keeps the previous
client behavior.

## Consequences

- Model choice follows the selected computer.
- A client never claims a change from an accepted receipt alone.
- A failed or unavailable model creates no empty chat and consumes no mutation counter.
- Existing two-tier routing remains honest until a person selects one ready model.

## Check

Protocol generation keeps Rust, JSON Schema and Swift exact. Runtime tests cover old session
records and atomic model creation. Client listener tests cover an invalid preflight, response-loss
retry, project and model conflicts, one created session, a terminal model change and snapshot
reconciliation.
