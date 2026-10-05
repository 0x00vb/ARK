# 0037. Work survives a restart: questions kept on tasks, and a restart continues work without doing a change twice

Status: Accepted
Date: 2026-10-05

## Context
ADR 0036 decided that what a client shows about work is runtime state, and planned W1 to W3.
Before this change:

| Gap | Where |
|---|---|
| A question an agent asked lived only as a `oneshot` in `ark_ui::Host`. A restart lost it, the orchestrator did not know its task waited for the person, and only a window's page could tell (the board's "Needs you", ARK Desktop's badges) | `ark-ui/src/host.rs`, `ark-interact/src/approval.rs` |
| A restart failed every open task, queued and suspended ones too, with no report to its chat. A project's open work ran again only when someone opened that project and retried it | `Orchestrator::load` |
| What an agent was doing was worked out by ARK Desktop's page from the event feed | ark-desktop `ui/lib.js` |

Cloud runs the same runtime with nobody watching (ARK Cloud Phase 2), so a restart (a deploy, a
machine move) must not lose a question or fail work, and must not make a change twice.

## Decision
No new work type. The task record carries what the work needs to continue.

| Question | Decision |
|---|---|
| Where a question lives | On its task: `TaskRecord::question` (`Question`: id, the `ApprovalRequest`, when asked, and an answer given after the run that asked was gone). While it is open the task is `Waiting` with `waiting_on: "your answer"`, its agent is `Waiting`, and its capacity is given back, as when it waits on tasks it delegated. Events `Asked` and `Answered` trace it |
| Who answers | Any client. `Orchestrator::answer(task, decision)`: to the waiting run when there is one, else kept on the task. The window's `approve` takes a kept question's id (a string); `hello` lists kept questions beside live ones; the terminal's `/task answer <task> once\|session\|always\|deny`. Grants an answer gives are kept either way (`ApprovalGate::remember`) |
| A question after a restart | The task waits for the answer, then runs again as a continuation. When its agent asks the same call again (same action, same input), the kept answer answers it: the person is not asked twice. Its brief says what was asked and answered |
| What a restart does with open work | A process that is gone left each open task in one of these: a question open: it waits for the answer. A state-changing call the engine recorded as running and never finished (its invocation record; `INTERRUPTED_CALL`): failed with that call named, since the change may or may not have happened and only the person can tell; it reports to its chat and blocks its board item, and `retry` runs it again. Suspended: stays suspended. Anything else (pending, queued, running, waiting on tasks): runs again; one that had started continues from its working state, as a resume does. An app opens the orchestrators of every project with open work or schedules when it starts (`projects_with_work`), so this happens with no window |
| A change made twice | A run that continues an interrupted one gets the state-changing calls the earlier run made since the task started (`Harness::done_before`, from the agent's invocations). An identical call (same digest) is not run or asked about again; the model gets the earlier result. The brief lists them: "you already did, do not do again". The same applies to a resume after a suspend |
| What an agent is doing | `AgentNode::step`: the call it is making or made last in its current task, from its run's events. `ark_interact::orchestration::doing` words it once, from the graph alone: the question, what its task waits for, "Paused", or the call as the conversation words it. `/agents` shows it; the window's `graph` method adds it to each agent as `doing` |

## Consequences
- The board's "Needs you" and ARK Desktop read questions from the tasks; nothing counts approval
  events in page memory.
- A restart in the middle of a call that only reads runs it again; one that changes something
  is never repeated on its own.
- A changed call with a different input (a mail with other words) is not caught by the digest;
  the brief's list of what was done is the only guard. A chat turn (not a task) still ends with
  the process: the person sees its stored transcript and asks again.
- `TaskRecord`, `TaskNode`, `AgentNode` and `Brief` have new fields; `EventKind` has two new
  variants; `graph_report` takes the app's `Labels`. Breaking for code that builds these
  structs or matches every event (`CHANGELOG.md`).

## Check
- `crates/ark-orchestrate/tests/orchestration.rs`: `work_a_stopped_process_left_open_continues_after_a_restart`,
  `a_change_cut_short_by_a_restart_is_not_run_again_on_its_own`,
  `a_change_made_before_a_restart_is_not_made_twice`,
  `a_task_waiting_for_the_person_shows_it_and_any_client_answers`,
  `a_question_survives_a_restart_and_its_answer_runs_the_call_once`,
  `cancelling_a_task_that_waits_for_an_answer_after_a_restart_ends_it`.
- `crates/ark-ui/tests/orchestration.rs`, `an_agents_question_survives_a_restart_and_any_client_answers_it`:
  the window's `graph` and the terminal's `/agents` show the same line; after the process
  stops, `hello` lists the question and `approve` by its id runs the call once.
