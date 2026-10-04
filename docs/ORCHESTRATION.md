# Orchestration

Several agents working together, built from a few ARK primitives. ARK runs them, keeps
their contexts apart, schedules them against provider capacity and records everything they
do. What the agents are (a coordinator and readers, a team, a hierarchy of teams) is the
application's choice. Crate: `ark-orchestrate`. Examples:
[`examples/agent-team`](../examples/agent-team) (a coordinator and two readers on Claude and
ChatGPT subscriptions) and `/review` in [`examples/writing-agent`](../examples/writing-agent)
(an editor and two readers on a manuscript, one team per project).

## Primitives

| Primitive | Type | What it holds |
|---|---|---|
| Agent | `AgentSpec`, `AgentRecord` | name, role, instructions, model, profile, allowed tools, who it may delegate to, parent, default budget, diagram tool on or off |
| Task | `TaskSpec`, `TaskRecord` | objective, context, artifacts, `after` (dependencies), budget, attempts; state, owner, origin, parent, children, usage, result, outputs, rules applied |
| Delegation | `agents.delegate`, `agents.wait` | an agent creates a task for an agent it may delegate to, and waits for the result or continues |
| The person's chat | `Orchestrator::chat_tools`: `agents.create`, `agents.delegate`, `agents.wait` | the conversation creates agents and hands any of them a task ([ADR 0007](adr/0007-the-chat-creates-agents-and-tasks.md)) |
| Message | `agents.message`, `Orchestrator::message` | a short note into another agent's running task, or its inbox for the next task |
| Artifact | ARK artifacts | passed explicitly with a task; a task's outputs go back to the delegator |
| Budget | `Budget` | steps, tokens, model calls, wall time, delegations |
| Graph | `Graph`, `OrchEvent` | the live state and the full trace |

An agent starts from the `Ark`'s profile. An app can offer other profiles by name
(`OrchestratorBuilder::profile(name, summary, profile)`); a spec picks one with
`profile_name`, which is stored, so a front-end can choose it (`profiles` in the workspace
protocol) and it survives a restart. `AgentSpec::profile` (a profile value) still works from
code but is not stored.

A team, a department or a manager is a composition: agents whose `delegates_to` and
`parent` say who works for whom.

```text
agent ──delegates──> agent ──delegates──> agent        (a chain)
agent ──delegates──> {agent, agent, agent}             (a team)
agent ──> {team lead ──> {agents}, team lead ──> {agents}}   (teams of teams)
```

## Running work

```rust
let orch = Orchestrator::builder(&ark, &project).build()?;
orch.spawn(AgentSpec::new("lead").model("claude:sonnet").delegates_to(["reader-a", "reader-b"])).await?;
orch.spawn(AgentSpec::new("reader-a").role("reads one document").model("codex").tools(["doc.read"])).await?;
orch.spawn(AgentSpec::new("reader-b").role("reads one document").model("claude:haiku").tools(["doc.read"])).await?;
let done = orch.run("lead", TaskSpec::new("compare the two reviews").artifacts([a, b])).await?;
```

`run(agent, task)` is the whole contract. The orchestrator then:

1. waits for the tasks in `after`, and fails the task if one of them did not finish;
2. waits for the agent (one task at a time per agent) and for a slot on the agent's account;
3. builds the agent's view: its allowed tools, its agent tools, its rules, its own project,
   its conversation carry-over, and a brief of the task;
4. runs one harness turn in the agent's project, on the agent's models;
5. records the result, the artifacts it produced and its usage, and hands outputs to the
   delegator.

| Lifecycle call | Effect |
|---|---|
| `spawn(spec)` | add an agent, or update one (a terminated agent comes back) |
| `submit(agent, task)` / `run(agent, task)` | schedule a task; `run` also waits |
| `suspend(agent)` | no new work; its running task stops at the next step and is suspended |
| `resume(agent)` | suspended tasks run again from the agent's working state; the brief lists what they had delegated |
| `cancel(task)` | stops the task and every open task below it |
| `terminate(agent)` | cancels its tasks, closes its model session, takes no more work |
| `retry(task)` | runs a failed or cancelled task again |

## From the chat

In an app built with `App::orchestrate` (or `App::orchestrator`), every turn of the person's
conversation also gets `Orchestrator::chat_tools()`, through `TurnOptions::tools`. The
person asks in words; the chat decides what becomes an agent or a task.

| Action | Does |
|---|---|
| `agents.create {name, role, instructions?, skills?}` | adds an agent with every app tool and the skills named (none by default); its state-changing actions still ask the person. An existing name is refused |
| `agents.delegate {to, objective, context?, wait?}` | hands any live agent a task with `Origin::External`; waits for the result unless `wait: false` |
| `agents.wait {tasks}` | waits for tasks handed off with `wait: false` |

The `agents.delegate` summary lists the current agents and their roles, so the chat picks one
without a lookup. An app with no orchestrator offers none of these.

## Context isolation

Each agent works in a child project of the orchestrator's project. Its working state,
evidence cards, invocations, notes and conversation are its own. It has no folder of its
own: file tools work in the top-level project's folder, read when it runs, so changing the
project's folder (`/folder`, `workspace`) moves its agents too.

A delegated agent gets a brief and nothing else from the delegator:

```text
Task t-e19f328b delegated by lead (from its task t-9d9af35e).

== OBJECTIVE ==
Read the spring support review and report its facts ...

== CONTEXT FROM lead ==
Document is spring-review.md, a support team review ...

== INPUTS ==
- art:74ae89d31c6e spring-review.md (text)

Your answer is returned to the agent that delegated this task as its result. Answer with the result itself.
```

The result comes back to the delegator as the result of its `agents.delegate` call: an
invocation in its store and an evidence card in its context. A longer result is clipped at
2,400 characters and the card says so and names the `get_evidence` path that reads the rest
(added after a live review where the delegator noticed a cut and did not fetch it).
Results of tasks in `after` are added
to the brief, clipped at 1,200 characters. Tests in `crates/ark-orchestrate/tests/orchestration.rs` check that a reader's requests contain neither the
coordinator's context nor the other reader's task.

## Models and subscriptions

The orchestrator asks a `ModelResolver` for an agent's models and never looks at the vendor.
`ark-orchestrate` names no vendor in its code (`crates/ark-ui/tests/neutral.rs` checks).

```text
run(agent, task)
  -> Models::resolve(lane = "<project>/<agent>", spec = "claude:sonnet")
  -> SubscriptionRuntime::session(lane, spec)    one managed vendor session per agent
  -> ManagedSession (a Provider)                 open, resume with a delta, rotate, release
  -> CliModel                                    a running claude (stream-json) or codex app-server per session
```

| Spec | Reasoning calls | Planner and critic calls |
|---|---|---|
| `claude:*`, `codex:*` | the agent's own `ManagedSession` | stateless calls on the same account |
| a name given to `ModelsBuilder::provider` | that provider | the same provider |
| anything else (`ollama:*`, `anthropic:*`, `openai:*`) | `ark_model::build` | the same provider |
| none | the `Ark` router (a subscription router still gets a managed session per agent) | the `Ark` router |

An agent with no spec follows the session's model: the resolver reads `Ark::current_model`
each time, so after `/model` (or the window's model choice) new agents run on the new model,
and an existing agent moves to it before its next task, releasing its vendor session on the
old one. A spec the person gave an agent always wins (ADR 0014).

Two agents never share a vendor session, even on the same model. The orchestrator uses the
`Ark`'s `SubscriptionRuntime`, so single-agent turns and agents draw on one capacity per
account.

A single agent gets the same treatment: when an `Ark`'s reasoning model is a subscription,
each conversation (`Session`) reasons through its own managed session (lane
`session/<id>`, `Ark::session_router`). Measured in the writing agent's terminal: two turns of
one conversation, each in a new process, used one vendor session for 3 calls; the second
process resumed it from the store.

### The managed session

ARK's state is canonical; a vendor session is a cache that can be thrown away.

| Behavior | How |
|---|---|
| Reuse | while healthy, a call resumes the agent's vendor session |
| Incremental turns | a resumed call sends only user and tool messages the vendor has not seen, tool definitions it has not seen, and changed instructions (`<host_update>`) |
| Context monitoring | the vendor's context size is read from each reply's measured usage |
| Rotation | before a call would pass `max_context_tokens` (64k) or `max_turns` (100), a new vendor session opens |
| Minimal rehydration | a new vendor session opens with the request the harness assembled, which is already under ARK's context budget, never the vendor's old history |
| Crash recovery | lane state is in the store (`StoreLedger`). A lane found mid-call after a restart, or whose last call failed, rehydrates instead of resuming |
| Idle cleanup | `Orchestrator::sweep_idle` / `spawn_sweeper` release sessions idle longer than `idle_secs` (30 min) |
| Session limit | an account keeps at most `max_sessions` (8) open; the least recently used idle one is released |
| File cleanup | when ARK lets a vendor session go (rotation, release, eviction), it deletes the vendor's file for that id: `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/<ARK's work dir>/<id>.jsonl`, or `rollout-*-<id>.jsonl` under `<CODEX_HOME or ~/.codex>/sessions`. Nothing else is touched |

Settings: `ARK_SESSION_REUSE=0` (off: every call is stateless, the earlier behavior),
`ARK_SESSION_MAX_CONTEXT`, `ARK_SESSION_MAX_TURNS`, `ARK_SESSION_IDLE_S`,
`ARK_SESSION_KEEP_FILES=1` (keep the vendor's files).

## Capacity

A subscription allows a few requests at a time. Every account has a `Capacity`
(`max_concurrent`, `min_interval_ms`, `max_sessions`); defaults are 2 requests at a time for
`claude` and `codex`, 4 for others, overridden with `ARK_<ACCOUNT>_CONCURRENCY` or
`ModelsBuilder::capacity`.

| Rule | Why |
|---|---|
| One running task per agent | an agent has one context and one vendor session |
| At most `max_concurrent` running tasks per account | the scheduler queues the rest, visibly (`TaskQueued`, `waiting_on: "capacity: ..."`) |
| At most `max_parallel` running tasks overall (8) | a bound on local work |
| Every model call waits for a slot on its account's `Limiter` | the hard limit, planner and critic calls included |
| A rate or usage limit cools the account down | the limiter holds new calls; the harness retries the failed one |
| A task waiting on tasks it delegated gives its slots back | a parent never blocks its own children, even with one slot |
| Delegating to an agent up the waiting chain is refused | it would wait on itself |

## Rules

A rule set is a project's standard, stored once and versioned (`RuleBook::put`: an edited set
becomes the next version; an unchanged one is not stored again). It is bound to a project,
an agent or a task (`RuleBinding`, optionally pinned to a version).

```text
- [E1] (always) Never commit a secret.
- [E2] Every public function has a test. #testing #code
```

For each task, ARK selects deterministically: every `always` rule, then the rules whose tags
and words match the task best, up to 400 tokens. The block rides in the task's first message
and is counted as `rules` in each `ContextReport`. The rest are reachable with `rules.lookup`
and `rules.get`. The selection is recorded on the task (`TaskRecord.rules`, as
`set@version:id`) and in the trace (`RulesApplied`). Project bindings apply to a
single-agent `Ark` too.

## Skills

An agent with `skills(["release-notes"])` (or `["*"]`) gets those skills of the project folder's
`.agents/skills` and the store's `skills`: their catalog (one line each) in every task's first
message, counted as `skills` in each `ContextReport`, and `skills.read` / `skills.file` to read
one. None by default. The person's conversation reads every skill unless the app narrows it
(`ArkBuilder::skills`). See [EXTENSION.md](EXTENSION.md#skills) and
[ADR 0009](adr/0009-skills-load-on-demand.md).

## Diagrams

An agent with `diagrams(true)` has `diagram.create` (preloaded) and `diagram.read`. A diagram
is Mermaid text with a kind: `flowchart`, `sequence`, `class`, `state`, `er`, `component`,
`dependency`, `architecture`, `orchestration`. `ark_tools::diagram::validate` checks the
header against the kind, closed blocks, balanced brackets and quotes, and the shape of each
line, and reports problems by line so the agent can fix them. It is a structural check, not
the full Mermaid grammar. A stored diagram is an artifact of kind `diagram` whose first line
records kind, title, version and the artifact it revises. Diagrams flow back to the delegator
like any output. `Graph::to_mermaid` exports the orchestration itself as a diagram.

## Observing a run

Everything a front-end shows comes from the orchestrator's records and event stream.

| API | Returns |
|---|---|
| `graph()` | `Graph`: agents (state, current task, model, managed session, context use, usage), tasks (state, why waiting, usage, tool calls, inputs, outputs, rules, brief size, result, error, times), edges (`owns`, `delegated`, `depends_on`, `parent`, `may_delegate`, `message`, `produced`, `input`), account capacity, `seq` |
| `subscribe()` | live `OrchEvent`s, streamed text included |
| `events(after_seq, limit)` | stored events (all but streamed text) |
| `task_trace(task)` | one task's events, including the runtime events of its agent |

To follow a run: load `graph()`, then apply events after its `seq`.

| Front-end | Where |
|---|---|
| Both | `App::orchestrator(orch)` (one project) or `App::orchestrate(f)` (one orchestrator per project, with the front-end's approval gate) adds `/agents` (the graph) and `/agents <task>` (one task's trace); reports come from `ark_interact::orchestration` and carry the graph as typed data (`view: "orchestration"`) |
| Both | `/agent new <name> [provider:model]`, `/agent pause|resume|end <name>`, `/agent message <name> <text>`, `/task <agent> <objective>`, `/task cancel|retry <task>`. Each is an `ark_interact::orchestration::Control` run by `control`; the window's methods (`spawn_agent`, `submit_task`, ...) build the same `Control`s, so a change means the same in both |
| Terminal, headless | `app -p "/agents"` prints the same report; any slash command runs this way (`app -p "/task lead summarize notes.md"`) |
| Window | read: `graph`, `agents`, `tools`, `task`, `task_trace`, `orch_events {after, limit}`. Control: `spawn_agent`, `submit_task`, `cancel_task`, `retry_task`, `suspend_agent`, `resume_agent`, `terminate_agent`, `message_agent`. Live: `{kind: "orch", event, line, tone, activity}`, which page scripts receive as `ark:orch`. With `App::orchestrate` each project has its own orchestrator. Details: [`UI.md`](UI.md#protocol) |

## Limits

| Limit | Note |
|---|---|
| Delegations from one model reply run at most 4 at a time | the harness's parallel tool limit; more queue |
| An agent loaded from the store after a restart has no custom `AgentProfile` | spawn it again with its full spec |
| A message pushed into a running task is delivered at the next step | the same as steering |
| Codex adds about 5k input tokens to each resumed turn (mostly cached) | rotation keeps it bounded |
| A vendor session still open when the app exits keeps its file | it is resumed next time, from the ledger |
