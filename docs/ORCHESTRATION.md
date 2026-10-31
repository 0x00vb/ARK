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
| `agents.create {name, role, instructions?, skills?, capabilities?}` | adds an agent with the skills named (none by default) and the capabilities named (`["web", "browser"]`, from `Orchestrator::capabilities`; every app tool when none are named); its state-changing actions still ask the person. An existing name or an unknown capability is refused |
| `agents.delegate {to, objective, context?, item?, wait?}` | hands any live agent a task with `Origin::External`; waits for the result unless `wait: false`. `item` links it to a board item |
| `agents.wait {tasks}` | waits for tasks handed off with `wait: false` |
| `agents.schedule {to, objective, context?, at?, days?, every_minutes?, once?}` | has an agent run the task at set times ([Schedules](#schedules)). `Mutate`: the person agrees to standing work once |
| `agents.unschedule {schedule}` | stops a schedule; its description lists the ones running |
| `board.list`, `board.add {items}`, `board.update {item, …}` | the project's board ([The board](#the-board)), from `Orchestrator::board_tools()` |

The `agents.delegate` summary lists the current agents and their roles, so the chat picks one
without a lookup. An app with no orchestrator offers none of these.

## Persistent conversations

An agent can create a lasting colleague with `agents.create {name, role, instructions?}`
when its tool patterns permit it. The colleague inherits its capabilities, skills, model,
profile, contacts and budget. It keeps its own conversation. Creation starts no work.

`agents.start {to, objective, context?}` sends one brief and returns immediately. It starts
an independent turn in the receiving agent's existing conversation. It does not copy the
sender's chat, wait for an answer, or start another turn when the recipient finishes.
`agents.message` remains a note for a running turn or the next brief.

The main chat's `agents.create` also accepts `contacts: [name, ...]`. Each direction is
explicit, and names may refer to colleagues the chat will create next. The creator and an
agent it creates can contact each other immediately. Other contacts follow the agent's
stored permissions.

Identical recipient, objective and context within one sending task return the original turn,
including across restart. A new external request can send the same brief again. One external
request's lineage may create eight agents and start eight independent turns, or fewer when
its delegation budget is smaller. Objective and context limits are 8,000 and 16,000 characters.

Desktop lists each agent once within its project. Kept agents stay visible. Search includes
names and roles even before an agent's first turn. See [ADR 0086](adr/0087-agent-conversations-start-independent-turns.md).

## Schedules

An agent can run a task on its own at set times: "every weekday at 09:00", "every 30
minutes", "the 1st of each month", "on 15 Oct at 14:00", "once, in 2 hours". A schedule is a
record in the orchestrator's store (ADR 0017, 0053). Each run is an ordinary task, submitted the
way `submit` does it, so it is queued, approved, traced and shown like any other. It carries
`TaskRecord::schedule` and `slot`, the time it is for.

```rust
let s = orch.schedule("digest", TaskSpec::new("summarize what changed today"),
    When::daily_at("18:00").on(["weekdays"]).in_zone("Europe/Madrid"))?;
orch.schedule("billing", TaskSpec::new("send the invoices"), When::cron("0 9 1 * *"))?;
orch.schedule("notes", TaskSpec::new("send the report"), When::on_date("2026-10-15", "14:00"))?;
orch.pause_schedule(&s.id)?;  orch.resume_schedule(&s.id)?;
orch.change_schedule(&s.id, ScheduleChange { when: Some(When::daily_at("19:00")), ..Default::default() })?;
orch.run_schedule_now(&s.id)?;            // a run now; the next time stays
orch.schedule_runs(&s.id, 20);            // its runs, newest first
orch.schedules();                         // soonest first, paused last; also Graph::schedules
orch.unschedule(&s.id)?;
```

| `When` | Runs |
|---|---|
| `every_minutes: 30` | every 30 minutes from its last time (5 at least) |
| `at: "09:00"` | every day at 09:00 in `tz` |
| `at: "09:00", days: ["mon", "wed"]` (or `weekdays`, `weekends`) | on those days |
| `at: "14:00", date: "2026-10-15"` | once, on that day |
| `cron: "0 9 1 * *"` | minute hour day-of-month month day-of-week; lists, ranges, steps, names; both day fields given: either matches |
| `once: true` | one time (at the next `at`, after `every_minutes`, or the next cron time), then it ends |
| `tz: "Europe/Madrid"` | the IANA zone times are read in; filled when the schedule is made (the person's client's zone, else the machine's) |

| Rule | Why |
|---|---|
| The orchestrator's timer looks every 20 s; it starts when `build` runs inside a tokio runtime | no window or chat needs to be open |
| A time the clocks skip runs as far after it as the jump; a time that happens twice runs the first time; intervals are durations | one run a day across daylight saving |
| A run missed while the process was down runs once when it is back (`if_missed: once`), saying it is late, or is let go when more than 15 minutes late (`skip`, counted in `missed`) | the morning report still comes, once |
| A run is skipped while the schedule's previous one is open (`skipped`); with `overlap` one more waits behind it, never more | runs never pile up |
| A run is made once per time: a task for (schedule, slot) found after a crash is not made again; a stop, pause or change waits for the tick | nothing runs twice, nothing stopped runs |
| A run that fails for a reason that may pass tries again after 5, then 10 minutes (`retries`, default 2), continuing what it did; out of credit does not. Stop and Pause cancel a run that only waits to try again | a network gone at 02:00 does not lose the night's work |
| 3 runs in a row that did not finish pause the schedule when the third ends, and say why in its chat | money is not spent on a run that keeps failing |
| A schedule keeps its newest 50 finished task records (their events stay, as for any task) | a schedule every few minutes does not fill the store with tasks |
| Ending an agent (`terminate`) ends its schedules | an ended agent runs nothing |
| An app with `App::orchestrate` opens the orchestrator of every project with active schedules when it starts (`scheduled_projects`) | scheduled work runs in projects nobody has open |

The chat makes, changes, resumes and runs a schedule with `agents.schedule` (asked each time,
with when and all each run is handed; no "always" covers it) and stops or pauses one with
`agents.unschedule`. Without `to`, the schedule gets an agent of its own. The tool's description
says today's date in the person's zone and lists the schedules with their state. Stopping a
schedule ends the agent made for it when nothing else uses it. Agents
never get these tools. The terminal has `/schedules` and `/schedules run|pause|resume|stop <id>`;
`/agents` lists them. The window shows a line per schedule above the composer of the chat that
asked, and the schedule with its runs in the side pane.

A run that needs an approval waits for an answer like any agent's question (tray, phone), and
holds the next runs until it is answered. "Always in this project", or "Allow for this session"
until the runtime restarts, lets it run unattended.

Each run's result is posted in the chat that asked for the schedule (ADR 0025): the window
shows it under that chat's last turn, live if the chat is open, and the chat's next turn
carries it, so the person can answer it. Any task can do the same with `TaskSpec::report_to`:

```rust
let chat = session.id().as_str();
orch.schedule("orders", TaskSpec::new("check the new orders").report_to(chat), When::daily_at("09:00"))?;
let tools = orch.chat_tools_in_zone(chat, Some("Europe/Madrid"));   // what the chat gets
ark_runtime::session::posts(engine, chat, 0, 50)?;   // the chat's posts, oldest first
```

## The boards

Each project has a board per goal: the work toward it, who has each piece and how far it is
(ADR 0023, ADR 0083). The person and the agents change the same `WorkItem`s; the window draws a
board as To do, In progress and Done beside the chat, the terminal lists them with `/board`. An
item is an outcome; a task is one agent's run toward it.

```rust
let b = orch.add_board("user", "Chapter 3", None)?;   // the board with that goal, or a new one
let w = orch.add_work("user", "Draft chapter 3", WorkChange::default().board(&b.id).note("Under 2,000 words."))?;
orch.submit("writer", TaskSpec::new("Draft chapter 3").item(&w.id))?;   // In progress, owner writer
orch.change_work("user", &w.id, WorkChange::default().blocked("needs the outline").owner("user"))?;
orch.board();   // every board's items: To do, In progress, Done (latest first)
orch.boards();  // the boards, the latest activity first
```

| Rule | Why |
|---|---|
| A task linked to an item moves it to In progress with that agent as owner | the board shows who works on what without the agent saying so |
| The last open linked task finishing moves the item to Done, its answer in the log | done means done; the planner may move it back |
| A failed task blocks the item with the error; a retry clears it; a cancel puts it back in To do | failures show where the work is tracked |
| An item another agent works on cannot be given to a second one, nor its owner changed | two agents never do the same work |
| Blocked and owned by `user`, or its task waiting for the person's answer: "Needs you" | what the person must do is one glance |
| A worker's brief carries its item: title, note, its last 3 steps | an agent continues what another agent or session left |
| The chat gets `board.list` (its description names the boards by goal with their counts), `board.add` (with the goal: a new goal makes a board), `board.update`; workers get none | planning is the chat's; bookkeeping is code's |
| A board keeps the chat that made it; that chat's window opens it | the person sees the plan when it is made |
| The log keeps the last 30 steps | history is a summary, not a transcript |

Boards and items are stored with the orchestrator's records (`orch_board`, `orch_work`) and
traced as `EventKind::Board`, `BoardRemoved`, `Work` and `WorkRemoved`. Items made before boards
had goals join one board named "Board" on load. `/board add <title>`, `done <item>`, `move <item> <column>`,
`give <item> <agent>`, `remove <item>` and the window's `board_*` methods build the same
`Control`s.

## Questions and restarts

An agent's state-changing call asks the person through the orchestrator's approval gate. The
question is kept on the task (`TaskRecord::question`), so it survives a restart and any client
answers it (ADR 0037).

| | |
|---|---|
| While it asks | The task is `Waiting` on `WAITING_PERSON` (`"your answer"`), its agent is `Waiting`, its capacity is given back. `questions()` lists such tasks; the graph's task carries `question` |
| Answering | `answer(task, decision)` from any client: the window's tray (`approve`), `/task answer <task> once\|session\|always\|deny`. A grant it gives is kept |
| A restart | Open work continues: a task with an open question waits for the answer and then runs again, and the same call asked again takes the kept answer; started tasks continue from their working state; queued ones run; suspended ones stay suspended. A task whose state-changing call was cut short fails with the call named, since only the person can tell whether it happened; `retry` runs it again |
| No change twice | A continuing run gets the state-changing calls the earlier run made (`Harness::done_before`); an identical call returns the earlier result instead of running |
| What an agent does | `AgentNode::step`, worded by `ark_interact::orchestration::doing` for every front-end |
| A stop | `Engine::stop_soon()` (SIGTERM on a served app, quitting the window or the terminal): runs stop at their next safe point, a batch with a state-changing call finishes first, and each task is saved as "continues after a restart" instead of failing (ADR 0038) |
| Results to the chat | A task the chat hands off (`agents.delegate`) reports to it: the waiting turn takes the result as its answer, or the task posts it into the chat when it ends (`wait: false`, a cancelled turn, a restart). `TaskRecord::reported` makes it once; a restart posts a result that had not arrived (ADR 0038) |

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
| `agent:<name>[:<model>]` (`agent:claude-code`, `agent:codex:gpt-5.5`) | none: another agent program does the task with its own tools (below) | none |

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

### An agent on another program

With `ArkBuilder::workers(true)`, an agent whose model is `agent:claude-code` or `agent:codex`
does its tasks in that program, installed and signed in on this computer (ADR 0043 step 2).
The task stays the orchestrator's; only the loop is the program's.

```text
task -> brief, the project folder -> the program (its own tools, its own model calls)
          wants to change a file / run a command -> an ARK question on the task (kept, any client answers)
        <- its answer, its usage, its cost
     -> what changed in the folder since the task started -> outputs, versions before
```

| What | How |
|---|---|
| Authority | The program starts with a clean environment (no ARK keys or secrets), without the person's or the project's own settings, in the mode that asks. ARK answers each of its requests with the program's "once" options; a grant is ARK's and names the file (`program.change:report.md`) or the command (`program.run:npm test@.`). A request with nothing concrete to name asks every time. Read-only steps the program classifies as reads (`ls`, reading a file) do not ask, as reads do not for ARK's agents |
| Results | The folder is listed and copied (clones) when the task first starts; when it ends, new and changed files are the task's outputs and changed or removed files keep their versions before (`program.files`). Paths the program names are not trusted for this |
| Restarts | The program's session id and process group are kept with the task (`orch_worker`). After a restart, what the gone process left running is stopped, the program loads its own session, and an answer given in an earlier run answers the same request again (`TaskRecord::decisions`): the person is not asked twice |
| Measurement | Tokens arrive as a model call, the program's own cost in `Usage::cost_micro_usd`, questions in `Usage::asked` |

`ark-orchestrate` names no program or protocol: the catalog lives in `ark-acp` and reaches the
orchestrator as `ark_runtime::worker::Workers`.

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

A project folder's own instructions (`AGENTS.md` by default, `ArkBuilder::project_instructions`)
go before the bound rules in each conversation turn: its first section and those closest to the
task within 1,500 tokens, the rest named, and the subfolders that have their own file listed
(ADR 0051). They show as
rules of the file's name (`AGENTS.md@0:<heading>`). Agents of an orchestrator do not read it yet.

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
| A schedule cannot be edited; its runs do not report into a chat | stop it and ask again; the runs are in the agent's work |
| Codex adds about 5k input tokens to each resumed turn (mostly cached) | rotation keeps it bounded |
| A vendor session still open when the app exits keeps its file | it is resumed next time, from the ledger |

## Model choices for new colleagues

`agents.create` accepts optional `model` and `reasoning` strings. Model uses the existing
account-qualified provider:model form. Reasoning must be offered for that model.
An agent-created colleague inherits the creating turn's resolved model, account and effort
when both are omitted. An explicit model uses its own default effort; reasoning alone changes
the inherited effort. A separate reasoning value cannot accompany a model with @effort.
Unavailable choices are refused before the colleague is published. Creation starts no task.
