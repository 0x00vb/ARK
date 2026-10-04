# 0014. Agents follow the session's model unless the person sets one

Status: Accepted
Date: 2026-10-04

## Context
An orchestrator's default resolver copied the app's model router once, when the orchestrator
was built. The person's conversation could then switch model (`/model`, Settings › Models),
but every agent without a model of its own still resolved to the copy. A product audit found
the result: a person who chose a local Ollama model got a new agent on Claude Sonnet, with no
sign of it. ark-desktop had worked around it in its own code (`choice::Follow`).

The rule people expect: what they choose for the session is what runs, including in agents
the session creates. A local-only choice must never call Claude, Codex, a paid API or any
other external provider unless they ask for it.

## Decision
The model a session runs on is a shared handle, `ark_runtime::router::CurrentModel`.
`Ark::set_router` updates it; `Ark::current_model` hands it out. The orchestrator's default
resolver follows it (`ModelsBuilder::follow`) and reads it each time it resolves an agent, so:

| Agent | Runs on |
|---|---|
| created without a model (by code, `/agent new`, the window, or the chat's `agents.create`) | the session's model at the time it is resolved |
| created earlier without a model, after the session's model changed | the new model, from its next task; its vendor session on the old model is released |
| given a model by the person (`AgentSpec::model`) | that model, whatever the session's model |

`ModelResolver::default_version` tells the orchestrator that the default moved. A resolver
whose default never changes keeps 0. `ModelsBuilder::default_router` still exists for a fixed
default.

The tool a model uses to create agents (`agents.create`) has no model field, so a model cannot
move work to another provider on its own.

## Consequences
- Switching model is one action for the whole session. Agents with their own model are the
  only exception, and that is the person's own setting.
- An agent can change provider between two tasks. Its context is ARK's (ADR 0004), so nothing
  is lost; a vendor session on the old model is released, not resumed later.
- Consumers that wrapped the resolver to get this (ark-desktop's `Follow`) can drop the wrapper.
- `Models` resolves the default from the current router at each resolve; the cost is a read
  lock.

## Check
`crates/ark-orchestrate/tests/orchestration.rs`:
`agents_without_a_model_follow_the_sessions_model_when_it_changes` (the audit's case through
the chat's `agents.create`; fails on the old copy),
`an_agents_own_model_is_kept_when_the_sessions_model_changes`,
`moving_off_a_subscription_releases_the_agents_vendor_session`.
