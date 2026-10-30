# 0087. Agent conversations start independent turns

Status: Accepted
Date: 2026-10-30

## Decision

A named agent keeps its existing AgentRecord, child project and conversation session. A new
conversation with a colleague does not fork the sender's transcript or create a second owner
of the agent's context. Each message that starts work is a task in that conversation.

An agent whose tool patterns allow `agents.create` may create a persistent colleague. The
colleague inherits its tool ceiling, skills, explicit model, profile, contacts and default
budget. It names its creator as parent. Mutation approval remains required. Creation starts
no model call. The main chat can name contacts when creating agents, including names it will
create next. Each direction is explicit. A creator and its new colleague may contact each
other immediately, including during the creating turn.

`agents.start` starts an independent turn and returns its ID immediately. It carries only an
objective and context note, capped at 8,000 and 16,000 characters. Its Origin records the
sender. Its task parent is absent: cancelling the sender does not cancel it, and completion
does not start the sender again. `agents.message` remains a note to a running turn or inbox.
Blocking delegation remains available for work that needs that contract.

Identical sender task, recipient, objective and context identify one start. Their canonical
JSON digest is stored in TaskRecord.handoff_key before publication. Retries with different
engine invocation IDs, including after restart, return that same turn. Sending an identical
brief again from a new external request starts a new turn. Creation by the same task and name
returns the same identity. Another creator cannot replace it. Create-only operations and
ordinary updates share a mutex. Final validation, durable writes and publication share the
state invariant. Cancelled creation releases its resolved model lane.

Each external request's lineage may create at most eight identities and start eight independent
turns. A smaller originating or current max_delegations budget applies. AgentRecord records
creation_task and creation_root. Starts follow Origin rather than task parent to retain the
root across independent conversations. All new persisted fields have defaults for old stores.
There are no automatic response loops or inherited permanent creation roots for future days.

## Navigation and search

Desktop uses one project list of agent conversations. Kept agents sort first and remain
visible. The ordinary list shows eight rows; selected, working and attention rows also stay
visible. At most eight newly discovered identities remain exposed until visited or the list
is expanded. Disclosure and selection survive section remounts. This replaces the separate
kept list, which could show the same agent twice.

The shared shell offers `searchSource({id, label?, load(project, api), open(entry, scopedApi)})`.
Rows have id, title, optional description and updated_at. Loading uses read-only queries with
explicit project IDs. It must not invent an opening session in another project. Opening waits
for project navigation, then validates runtime and project before passing a scoped API.
Partial query failures preserve successful rows and expose retry.

`agent_contacts` reads stored names, roles, state and timestamps for one existing project.
It opens no chat, resolves no model and starts no work. It exposes no instructions, private
conversation or model session identifiers. Agents with no turns are discoverable.

## Client protocol

Protocol 1.6 adds optional ChatSummary.agent metadata and the `agents.conversations`
capability. Desktop, browser, iPhone and headless Cloud use the same AgentRecord and TaskRecord.
The external client chat ID is `agent:<project>:<name>`. Underlying agent session IDs resolve
to that same agent path. Reads preserve a validated requested alias ID for clients holding
an older selection. Lists expose only canonical IDs. Agent changes also invalidate openChat.
They never enter the app-default chat execution path.

External sends use the device, mutation epoch and counter plus project and agent as the durable
TaskRecord key. A restart between task persistence and receipt persistence returns the same
task. Invalid or deleted targets finalize as no longer applicable so later counters can proceed.
Task messages have stable IDs, full results and the originating author. Results must be produced
outputs of that agent's tasks in its context. Approval projection matches local runtime, project,
agent and task. The all-project feed invalidates agent conversations without a desktop subscriber;
feed lag invalidates chats and open conversations. Older chats omit agent metadata.

Native iPhone groups Agents and Chats. It shows three agent rows on compact screens and eight in a sidebar, plus current work and selection,
with a disclosure for the rest. Search includes roles. Equal agent names show project and computer
context. Native code stores no conversation history or agent context locally.

## Alternatives

A separate SessionRecord binding was rejected. AgentRecord already owns a durable session;
a second record would create competing history and context. A permanently expanded list of
all agents was rejected because daily creation would crowd out current work. A nested team
sidebar was rejected because the existing Agents canvas already holds those relationships.

## Verification

Conversation unit tests cover concurrent creation, inherited permissions, lineage limits,
reload, idempotent independent starts and cancellation during model resolution. A scripted
provider test creates and starts a colleague through real engine calls, repeats the start,
checks that the sender's private input is absent, and continues the same receiving session.
Host tests query equal agent names in different projects without navigating. WebKit tests
exercise production sidebar and search modules, including delayed navigation and stale routes.
