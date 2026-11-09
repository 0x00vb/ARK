# 0109. An agent has many chats, and they run at once

Status: Accepted. Date: 2026-11-09. Amends 0087 (one conversation session per agent).

## Context

The owner wants an agent to feel continuous: one identity, its instructions, tools and memory,
with many separate chats. ADR 0087 gave a named agent exactly one conversation session, and the
orchestrator ran one task per agent at a time. The owner chose that two chats of one agent run in
parallel, with an explicit state model and no shared execution state between chats.

`TaskRecord.conversation` already means a Work or automation submission: it drops the agent's
program, skips its inbox and uses the objective as input. It could not carry a person's chat.

## Decision

| Part | Decision |
|---|---|
| Chat of an agent | A session whose project is the agent's `scope`, with no work item, not execution managed, no execution scope. `agent.session` stays its first chat, so old stores need no migration |
| Task field | `TaskSpec.thread` and `TaskRecord.thread`: the chat a person's task runs in. None is the first chat. `conversation` keeps its Work meaning |
| Admission | A thread must be a chat of that agent. Refused at submit and checked again before the run. Refused for agents on an agent program: their program holds one session |
| Lanes | The process-wide execution lock is keyed by (store, project, agent, lane). Lane is the thread, or `default` for the first chat, Work and automations. One task per lane; lanes run at once |
| Running | `AgentRecord.running` lists the tasks running now and is the only record of it. `current_task` is still written (the first entry) for older readers. On load, `running` is rebuilt from tasks whose process is alive |
| Agent state | Derived in one function: Suspended and Terminated stick; Running if a running task is not waiting on a delegation; Waiting if all are; else Idle |
| Steering | Belongs to the running task. `message_agent` takes an optional thread; without one it steers the first chat's task, else the inbox. `Mail.thread` keeps a late message with its chat |
| Models and tools | A thread resolves its own model lane `{project}/{agent}/thread/{id}`, closed when its task ends. Browser and web tools are owned per thread |
| Shared | Identity, tools, the agent's project memory (one SQLite transaction per write, ADR 0079), its folder |
| Not shared | Session history, working state (keyed by the chat's session), steering, vendor session, browser |

## Consequences

- Breaking for struct literals: `TaskSpec.thread`, `TaskRecord.thread`, `AgentRecord.running`, `Mail.thread`.
- Two chats of one agent can edit the same folder at the same time, as two agents in one project could before. Code runs keep their own admission.
- `step` and `last_context` on the agent show the latest lane.
- iPhone and Cloud clients keep `agent:<project>:<name>`, which maps to the first chat.

## Verification

Orchestrator tests: two threads run at once, the same thread queues, Work stays on the default
lane, state derivation, cancel and suspend per lane, steering and late mail per thread, foreign and
program refusals, `agent.session` unchanged by a thread, reload of legacy and multi-lane records,
concurrent memory suggestions. Host tests for the new calls.
