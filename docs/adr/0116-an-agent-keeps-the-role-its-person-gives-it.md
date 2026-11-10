# 0116. An agent keeps the role its person gives it in chat

Status: Accepted. Date: 2026-11-09.

## Context

The owner (2026-11-09): when we create an agent and open its chat, we must be able to instruct
its role in words, with no new UI. "You are the CEO ..." stays its main instruction; a request
that is not a role leaves it a generic agent.

## Decision

| Part | Decision |
|---|---|
| Data | `AgentSpec.standing` (text, at most 4000 characters, `#[serde(default)]`, left out of the record when empty). An older record reads as empty |
| Prompt | `agent_profile` appends it after the base or app instructions and the "You are the agent" line, under `The person set your role:` |
| Why not `instructions` | `agent_profile` replaces the base prompt with `instructions`: "You are the CEO" would drop ARK's working rules |
| Why not `role` | `role` is the short label other agents read in their contact lists; long text there costs every colleague's prompt |
| Tool | `role.set {instructions}` sets the agent's standing instructions (`AgentSpec.standing`) to the full text it passes; empty text clears it. It never touches `AgentSpec.role`, the colleague label. The result reads back the stored text (clipped), so the reply shows it. Risk tier Observe: it never shows an approval card, in any mode (the owner asked for no UI). It changes only the calling agent's own instructions, and only as the first call of a person's turn, so nothing read earlier can steer it |
| `spec.tools` | The tool obeys an agent's `tools` patterns like any tool: an agent whose patterns exclude `role.set` does not get it |
| Who gets it | Only a task the person typed: `TaskSpec.person` and `TaskRecord.person`, set by the front-end send paths (window chat send, phone chat send, work admission, the terminal's `Control::Submit`), never inferred from `origin` or `thread`. Also required: origin `external`, not an automation run, not a schedule's task. A delegated, scheduled, automation or group-chat task never has it. It acts only on the calling agent |
| Fixed list position | When offered it is registered after the app's tools and before the agent tools, in every person turn, so the tool list prefix is the same each time |
| Takes effect | From the person's next message; the running turn keeps its prompt (as `mode.set`, ADR 0104) |
| Persistence | `Inner::set_standing` updates the agent record, saves it and emits `AgentSpawned`, as an edit through `spawn` does, without resolving the model again. A `spawn` over an existing agent keeps the stored `standing` when the incoming spec leaves it empty, so a Name, Model or Folder edit that sends an older spec cannot erase it; only the tool clears it, and a spec with text replaces it |
| Vendor sessions | A changed system prompt is not lost on a reused vendor session: the session layer compares the system digest and sends the new text as a `<host_update>` (`ark-model/src/session.rs` plan, `cli.rs`). With a new session the prompt is built from the updated spec. Claude Code's whole-prompt cache (ADR 0102) pays one write for that agent after a role change |
| Context | The block is part of `profile.prompts.instructions`, so the context report counts it under instructions |

## Injection bound (owner's decision)

Text the agent reads in a turn (a page, a file, a tool result) must not be able to set a role.
The rule is deterministic: `role.set` is refused, with a plain error the model sees ("Set your
role only from the person's own message, before reading anything else."), when any other tool
call already happened in the task, or when `role.set` was already called in it. The first call of
a person's turn works. Also: person turns only, own agent only, 4000 characters counted as
characters. No approval card is added for this. Not covered: the person's own message that
pastes hostile text and asks for a role, and text the model sees before its first call that is not
a tool result: a task's `context`, shared artifacts, and memory or recall in the prompt. Tasks run
by another agent program (ADR on workers) do not get the tool.

## Consequences

- One tool schema more in each person turn. The simulated benchmark ran before and after with no
  change (cost per success 0.083269 both): its agents get no person-typed tasks, so the schema's
  cost is not measured there. A standing block adds up to about 1,000 tokens to every
  request of that agent.
- `AgentSpec` and `TaskSpec` gain a field; struct literals outside the crate must add it.
- A message queued before the role changed may run with the older text (its spec is read at
  admission).
- Not verified: whether a live model calls the tool at the right moment, and does not call it for
  a one-off request ("write this as a CEO would"). That needs a live run with the owner's OK.
