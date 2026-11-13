# 0117. A chat makes a group of agents with shared rules

Status: Accepted. Date: 2026-11-12.

## Context

The owner (2026-11-12): from a chat, create agents to run a business (a CMO, a COO), each with
its own personality, following rules they share, able to talk to each other. Only when the person
asks. ADR 0113 gave ARK groups, but only the window could make one, and a group had no rules and
no link between its members' agents.

## Decision

| Part | Decision |
|---|---|
| Boundary | ARK adds no team concept. Teams stay what apps build from `AgentSpec`s. A chat can configure a group (ADR 0113), which is already ARK's generic primitive: a novel's editor and researcher are a group too. Consumers today: ARK Desktop and the phone |
| Data | One field: `AgentSpec.groups` (group ids, `#[serde(default)]`, left out when empty). Contact, rules and cleanup all derive from it, so nothing is left behind when a member or the group goes |
| Types | `Group`, `Member` and the `#groups` scope move to `ark-orchestrate::group`; `ark_interact::groups` re-exports them. The orchestrator writes the group record (`Orchestrator::put_group`), the host forwards the live `EventKind::Group` as `ark:group` |
| Tool | `groups.set {name, rules?, members: [{name, role?, personality?, model?}]}`, 1 to 8 members listed, a group of two or more after the call. One upsert call: the same name in this folder is the same group, so the person can later add a rule or an agent. A name that is free becomes a new agent; an agent of this folder joins and keeps its own role and personality. Members already on the group's record (a group made in the window) join too, so every member has the same contacts and rules. A name from another folder (`folder/name`) is refused. Members the call does not list stay: removing a member is a window action. The result lists the members, who was created, who joined, and whether the rules changed |
| New member | One helper (`AgentSpec::colleague`) builds it, for `groups.set` and `agents.create` alike. From its creator it keeps `model`, `tools`, `budget`, `profile_name`, `profile` and `skills`, so a member holds its creator's permissions (approvals still apply). It clears `title`, `description`, `look`, `standing`, `delegates_to`, `groups`, `instructions`, and sets `role`, `parent`, `allow_mutate=false`. `personality` becomes `standing` (ADR 0116, 4000 characters) |
| Contact | Two agents that share a group id may `agents.message` and `agents.start` each other, in both directions. `agents.delegate` still needs `delegates_to`: a group adds no delegation. Work one member starts on another only through the group (no delegation, no parent and child) asks before any state-changing action, even on an agent that may act on its own (`allow_mutate`): the person agreed to a group, not to one member acting through another. A message is text the other reads in its running or next task, so a member may not message, only through the group, an agent that acts on its own: it uses `agents.start`, which asks. Any hop of the chain counts: work a member started, then delegated on, still asks. A brief is never another agent's conversation |
| Rules | Rule set `group-<id>`, every rule `always`. `rules` is the group's full list: a call that passes it replaces the rules (a new version only when they differ), a call that omits it keeps them. At most 20 rules of 300 characters per group, and 20 always-rules per agent across all its groups; the check covers the members listed and those already in the group, so a call that would pass the limit for anyone is refused. `rules_for` appends the sets of the agent's groups, so every task of a member follows them, scheduled and automation runs included. A new version is read by each member's next task |
| Same name, same group | A group of this folder with that name (any case) is updated. A new group's id is a hash of the folder and the normalized name. A retry with the same input changes nothing: no second agent, no new rule version, no new member row |
| Order of writes | Everything is checked first (names, the 8-agent limit of the request, rule limits, models) and nothing is written for an invalid call. Then, under the conversation lock, what can still fail runs first: the new agents one by one. If one cannot be made, the agents this call made end (`Terminated`) and nothing else is written; the same call later makes them again (they are marked `group-undone:<id>`; an agent the person ended keeps `group:<id>` and stays ended). Then the joins, the rule set, the group record. A storage error after the agents leaves a part, and the same call run again finishes it. A plain name is this folder's: an agent of that name in another folder is a different agent |
| Edits | A `spawn` over an existing agent keeps its stored `groups` when the incoming spec has none (as `standing`, ADR 0116), so a Name, Model or Folder edit that sends an older spec cannot take an agent out of its group |
| Leaving | The window's `group_update` removes the group id from members taken out. `group_delete` removes it from every member and deletes the rule set |
| Consent | Offered only on the person's own turn (the `role.set` condition, phone turns included), and only before any other tool in the turn (`read_before`). `role.set` and `groups.set` may both come first. A failed attempt does not use up the first call. Nothing untrusted has been read when it runs, so rules and personalities come from the person's message. Not offered on delegated, scheduled, automation or group-chat tasks. Observe tier, no card |

## Consequences

- `agents.create` changes: the new agent no longer copies its creator's `look`, `standing`,
  `title`, `description` or `delegates_to`. The creator and the new agent still contact each other
  through `parent`.
- `AgentSpec` gains `groups`: a struct literal outside the crate must add it.
- One more tool schema in person turns, and up to 20 always-rules in the prompt of each member.
- "Read my CRM, then set up a group" needs two messages.
- Membership added in the window (`group_create`, `group_update`) does not set the id: such a
  group routes chat as before and gives no contact or rules, until a chat's `groups.set` names it.
- `AgentSpec.groups` is left out of the JSON when empty: a reader must treat a missing field as
  no groups.
- Open, from ADR 0113 and not changed here: in a window group chat, a member's `@Name` reply starts the next member's turn as a person turn, so an agent with `allow_mutate` acts on another member's text without asking.
- Not verified: whether a live model calls the tool at the right moment and not for a one-off
  request. That needs a live run with the owner's OK.
