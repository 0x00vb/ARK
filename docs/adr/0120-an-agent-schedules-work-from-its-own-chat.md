# 0120. An agent schedules work from its own chat

Status: Accepted, with one point for the owner to confirm (an agent with `allow_mutate` does not ask in ask mode). Date: 2026-11-13. Builds on 0025 and 0053 (schedules report into a chat), 0107 (a chat starts in Auto; schedules asked in Auto), 0113 and 0117 (group-only work asks), 0118 (an answer arrives as the agent's next message).

## Context

The owner (2026-11-12): a named agent schedules work from its own chat, and each run answers in that chat as the agent's message. `agents.schedule` and `agents.unschedule` were only in `ChatTools`, the old assistant chat. Desktop has named agents only (0115).

## Decision

| Part | Decision |
|---|---|
| Tools | `agents.schedule` and `agents.unschedule` are offered to a chat turn of a named agent (`TaskRecord::is_chat_turn`, not an agent program, `AgentSpec.tools` allows each) unless the chat is in Plan. One code path with `ChatTools` (`Scheduling`); the assistant chat is unchanged |
| Not offered | A scheduled run is not a chat turn, so it has no schedule tool, `work.later`, `groups.set` or `role.set`: a schedule never makes schedules. Work another agent started, even through a group, is not a chat turn either |
| Answer | The schedule's task has `reply` and `report_to` = the chat that asked. Each run posts its answer once as `Post.kind = message`, scrubbed, as in 0118. The pause after 3 failures (0025) stays a result card |
| Approval | The chat's mode, read once when the turn's tools are built (`task.thread` or the agent's chat; a chat with no record yet is a new one, Auto). Auto: making a schedule asks nothing. Ask mode (Manual): it asks every time, no grant covers it. An agent with `allow_mutate` acts alone by its own setting and does not ask in any mode. Stopping or pausing never asks (Observe, as before) |
| Runs | The run's own state-changing actions ask as scheduled runs do. A schedule's own agent is a colleague of the asker (its tools and model, `allow_mutate` off, parent = the asker), never more than the asker |
| Targets | `to` is the asker or empty (an agent of its own). Never another agent, whether reached by delegation, kinship or a group: in Auto nothing asks, a run has origin External, and the run would act with that agent's own authority (`allow_mutate`) and without a group's ask (0113). For other agents the asker uses `agents.delegate` or `agents.start` |
| What it sees | Only schedules the asker made for itself (`reply`) or that run on an agent made for the asker, not by the chat they report to: the person's own tools may report into the same chat. A schedule the person made is the person's to change. A chat with no record yet counts as a new chat in Auto; one whose record cannot be read gets no schedule tools. Change, resume, run now and stop refuse the rest. At most 10 at once. A run at most once an hour (the gap between its next two runs) |
| Refused schedule | If the schedule cannot be made after its own agent was, that agent ends |
| Admission | A schedule's result chat is checked outside the project transaction: an agent's chat lives in that agent's own scope, which the project transaction cannot read. The chat must exist in the project or one of its agents' scopes |
| Calendar | Reads every schedule record, so a schedule made from an agent chat is listed (tested) |
| Page | No change: a `message` post already draws as the agent's turn |

## Consequences

- In Auto a page the agent reads can set up work that runs unattended (0107 kept this asking). Bounds: the 10 and once-an-hour limits, the run's own actions ask, it is in the Calendar and the agent can stop it, the person can say "ask me before changes". No per-run budget or daily ceiling is added.
- Ask mode does not reach other actions of a named agent today (the orchestrated harness is Manual and reads no chat mode); this decision reads the mode only for the schedule tools.
- A run posts as its owner: an agent's answer lands in its own chat as its message. A deleted result chat is not handled: the post fails with a warning, the result stays in the task record and the schedule goes on.
- The 10 is per agent, with no total over all agents.
- A schedule made on a group turn is the member's own: its answers copy into the group as that member's message (0118).
- A schedule keeps its own agent's context between runs; its growth is not measured. Change, resume, run now and stop of the schedules it sees are Observe in Auto, as the creation is: they only touch work the asker or its own agent made. A chat whose mode cannot be read is offered no schedule tools.
- Not verified: the real app (no dogfood, the page is unchanged); a live model choosing to schedule at the right moment and not for a one-off request.
