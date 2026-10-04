# 0017. Work runs without the window: live conversations and schedules

Status: Accepted
Date: 2026-10-04

## Context
ROADMAP step 5: ARK keeps working when nobody watches. ADR 0016 makes a runtime an app's
binary that clients connect to. Three things still tied work to one window:

| Gap | Where |
|---|---|
| One live conversation per runtime. Opening another chat was refused while a turn ran | `ark_ui::Host` held one `Interaction` |
| A project's agents started only when a chat opened that project. Each `Interaction` built its own orchestrators, and one approval gate followed whichever project was open | `Interaction::follow_project` |
| No way to have work start later or repeat | nothing |

The person should see none of this. They start something in a chat and go to another one,
and the first keeps going. They ask in words for "every weekday at 9", and it happens.

## Decision
No new job system, server or permission model. Each gap closes with the primitives that
exist.

| Question | Decision |
|---|---|
| Several conversations | `Interaction::siblings()` opens another live conversation of the same app. Store, tools, the model chosen last, capabilities, orchestrators and lasting grants are shared. Each conversation has its own session, its own "for this session" grants and its own turns. The `Host` keeps the current conversation plus those still running a turn. Moving to another chat while a turn runs leaves that turn running. A conversation ends when its turn ends, unless it is current again. Its session is stored as before |
| Never the same chat twice | Opening a chat that is live brings that conversation forward, and any change that lands on a live chat does the same. No session is ever open in two interactions |
| What a window sees | Turn events and questions carry `session` (questions also carry `chat`, its title). `working` lists the chats running a turn. The page shows the open chat, puts other chats' questions in the tray with their chat's name, and marks working chats in the sidebar with a dot. Lists and transcripts read the store, so they answer while turns run. All of it is additive to the protocol |
| Agents outlive chats | One orchestrator per project, shared by all of an app's conversations (`App::orchestrate`). Each has its own approval gate kept in its own project. When the app opens, projects with active schedules get their orchestrator, so scheduled work runs with no chat open there |
| Schedules | A `ScheduleRecord` in the orchestrator's store: an agent, a `TaskSpec` and a `When`. `When` is `every_minutes`, or `at` (`HH:MM` local) with optional `days`, optionally `once`. The orchestrator's timer (every 20 s) submits each due run through the same path as `submit`. A run is a plain `TaskRecord` with `schedule` set: queued, approved, traced and shown like any task |
| Schedule rules | A run missed while the process was down runs once when it is back, not once per missed time. A run is skipped while the previous one is still open. A `once` schedule ends after its run |
| Asking for it | The chat gets `agents.schedule` (`Mutate`: standing work spends model time with nobody watching, so the person agrees once) and `agents.unschedule`. Each run's own state-changing actions still ask, unless the person chose "Always in this project". The terminal gets `/schedules` and `/schedules stop <id>`; `/agents` lists them; `Graph.schedules` carries them |

## Consequences
- The product reads as Projects, Chats, Agents, Work. Nothing new to set up, and no daemon or
  queue words reach the person.
- Cloud and self-hosting run the same thing: a runtime process, its store and its timer
  (ADR 0016). A supervisor restarts the process; schedules and stored chats survive.
- A scheduled run that needs an answer waits in the tray until someone connects. A
  notification outside the window is the client's job (ark-desktop), not the runtime's.
- A conversation's in-memory turn history (`/trace` of earlier turns in this process) belongs
  to that conversation. A conversation brought forward has its own.
- The model picker is one choice for the app: a conversation brought forward, or opened, takes
  the model chosen last. A turn already running keeps the model it started with.
- Not done: a schedule's runs do not report into a chat; they show in the agents' work.
  Schedules are not editable (stop one and ask again). There are no calendar expressions
  beyond days of the week.

## Check
- `crates/ark-orchestrate/tests/orchestration.rs`:
  `a_schedule_runs_its_task_at_its_times_and_survives_a_restart`,
  `a_scheduled_run_is_skipped_while_the_last_one_is_still_open`,
  `the_conversation_schedules_work_and_stops_it`.
- `crates/ark-interact/tests/interaction.rs`:
  `sibling_conversations_share_the_agents_and_scheduled_work_wakes_at_open`.
- `crates/ark-ui/tests/host.rs`: `a_chat_keeps_working_while_the_window_moves_to_another`.
- `crates/ark-ui/tests/page.rs` (WebKit):
  `a_chat_keeps_working_in_the_background_while_another_runs`.
