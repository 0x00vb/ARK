# 0053. Scheduled work is asked for each time, runs once per time, and continues after a failure

Status: Accepted
Date: 2026-10-07

## Context
ADR 0017 added schedules and ADR 0025 posts each run's result in the chat that asked. Asked for
"every day at 2 AM, open the stock spreadsheet, recount it, update it and give me a summary",
ARK fell short:

| Gap | Where |
|---|---|
| Only "at HH:MM on days" and "every N minutes": no date ("on 15 Oct at 14:00", "tomorrow at 9"), no month days | `When` |
| Times read in the machine's offset at each run, and no zone kept: a runtime on a server in UTC runs "2 AM" at the server's 2 AM | `When::next_after` |
| The model was never told the date or time, so "tomorrow" was a guess | the chat's tools |
| A schedule needed an agent named first: the chat had to create one, then schedule | `agents.schedule` |
| No pause, resume, change or run now; stop deleted it | API and tools |
| A run failed by an unreachable model was lost until the next day. A retry inside a task repeated the changes its failed try had made (`done_before` only for restarts) | `drive` |
| A crash between creating a run and saving its schedule ran the same time twice | `tick_schedules` |
| A timer tick wrote back a copy of the schedule taken before a stop or change | `tick_schedules` |
| "Always in this project" on `agents.schedule` let any later turn, including words injected by a page or mail the chat read, set up unattended work without asking | the approval class |
| The window showed no schedules at all | `ark-ui` |

## Decision
Still no job system. A schedule is a record in the orchestrator's store and each run is a task
(ADR 0017). The record, the timer and the chat tools grow.

| Question | Decision |
|---|---|
| When | `When` is one of: `at` (HH:MM) with `days`, or with one `date` (YYYY-MM-DD, a single run); `every_minutes` (5 at least); `cron` (5 fields: minute hour day-of-month month day-of-week, lists, ranges, steps, names; both day fields restricted means either). `once` ends after one run. Runs closer than 5 minutes, a cron that never matches in 5 years, and a time that has passed are refused with words that say how to fix them |
| Zone and daylight saving | `When::tz` is the IANA zone, kept when the schedule is made: the zone the person's client sent in `hello` (`Intl`), else the machine's (`jiff`). A runtime elsewhere reads "02:00" in the person's zone. A time the clocks skip runs as far after it as the jump (02:30 on a spring-forward day runs at 03:30); a time that happens twice runs the first time. Intervals are durations. A record without a zone (made before) is read in the machine's zone |
| Telling the model "today" | `agents.schedule`'s description carries `Today: Wed 2026-10-07 (Europe/Madrid).`, in the person's zone, and the schedules there are (id, when, next, state). The date only: a description that changed every minute would spoil the prompt cache. "In 2 hours" is `every_minutes` with `once` |
| Which agent | `to` is optional. Without it, the schedule gets an agent of its own named from its objective (`recount-the-stock`), with every capability; what it carries from run to run is this schedule's alone. Stopping the schedule (`stop_schedule`) ends that agent when no other schedule uses it and it has no open task. Only the chat gets the schedule tools; agents never do, so a run cannot make more standing work |
| What a run is handed | The objective and context the chat wrote (the tool asks for them complete: files, places, what to leave), and one line: "This is the run scheduled for Thu 02:00 (Europe/Madrid)", plus "It starts late, at 09:12" when it does. Never the chat's transcript |
| Asking | `agents.schedule` (make, change, resume, run now) is `Mutate` and `ask_each_time`: no "for this session" or "always" answer covers it. Its approval shows when and, in full (never clipped, the card scrolls), what each run is handed; a change names the schedule by what it does and when it runs now. An app run with `allow_mutate` (the terminal's `--yes`) asks nothing, this included: that mode is the person's choice to run unattended. `agents.unschedule` (stop, or `pause`) is `Observe`: it only takes work away. Each run's own state-changing calls ask through the orchestrator's gate as any agent's do, kept on the task across restarts (ADR 0037) and sent to a phone (ADR 0047). What lets a run go unattended is the person's own grant for that action: "Always in this project", or "Allow for this session", which lasts until the runtime restarts and covers every agent of the project, scheduled runs included |
| States | `ScheduleState`: scheduled, running, asking (waits for an answer), retrying, succeeded, failed, paused, from the record and its last run. `ark_interact::orchestration::{schedules, schedule_view, schedule_runs}` word them once for both front-ends |
| Pause, resume, change, run now, stop | `Orchestrator::{pause_schedule, resume_schedule, change_schedule, run_schedule_now, unschedule}`; `Control::{PauseSchedule, ResumeSchedule, RunSchedule, Unschedule}`; `/schedules run\|pause\|resume\|stop <id>`. A paused schedule is kept (`paused`, `next_ms: None`); resuming starts from its next time, the times missed while paused do not run. A change keeps the zone unless it names one. Run now keeps the next time and is refused while a run is open (unless `overlap`). Stop removes the schedule; its runs and posts stay. A run already working goes on after any of these (cancel its task to stop it); a run only waiting to try again is cancelled by Stop and Pause. A one-time schedule stays, with no next time, until its run ends, so the window shows how that run goes. A change to a time that has passed is refused, also while paused. An unknown id is refused at once with the ids there are |
| One run per time | A run's task carries `slot`, the time it is for, and is saved with `schedule` and `slot` in one write. A tick that finds a task for (schedule, slot) advances without making another. Every change to a schedule takes the timer's lock, so a tick never writes back a copy from before a change. An interval counts from its time, not from the tick (no drift) |
| A failure that may pass | Runs get `max_attempts = 1 + retries` (default 2) and `retry_delay_ms` 5 minutes, doubling. Only failures trying again might fix are tried again (the model unreachable or overloaded after its own 4 retries in 60 s, a reply it could not get right, a store or tool error); out of credit, a missing project or action, a plugin that cannot start are not. While it waits the task is queued, `waiting_on: "trying again at 02:10"`, and `retry_at_ms` is saved, so a restart waits too. The next try continues the failed one: it gets the state-changing calls the task made (`Harness::done_before`), its own only, not another task's that ran in between. This also fixes any task with `max_attempts > 1` |
| Missed runs | `if_missed: once` (default): a run whose time passed while ARK or the computer was off runs once when ARK is back, however many times were missed, and says it is late. `skip`: a run more than 15 minutes late is let go (`missed` counts it); a one-time schedule let go posts a note in its chat. ARK does not wake the computer or start itself: that is the app's (ARK Desktop's login item, a server's supervisor) |
| Overlap | Default: a due run is skipped while the last is open (`skipped`), so a run waiting for an answer holds the next ones. `overlap: true`: one more run waits behind it, never more |
| Failures in a row | Counted when each run ends. After 3 runs in a row that did not finish, the schedule pauses itself at once and posts why in its chat. Resuming clears the count |
| Growth | A schedule keeps its newest 50 finished task records (not ones linked to a board item or with children); older ones leave the store. Their events and invocations stay, as for any task. The tool descriptions list 10 schedules and count the rest |
| The window | A quiet line per schedule of this chat above the composer: when, what, and its state ("next Thu 02:00", "running now", "trying again at 02:10", "waits for you", "paused", "did not finish Thu 02:00"). The line opens the schedule in the side pane: what it does, when (the zone only when not the person's), next and last run, Run now (off while a run is open), Pause or Resume, Stop (asks once more), and its runs with their results and files. No calendar and no form: making and changing are asked in words. Host methods `schedules`, `schedule`, `schedule_run`, `schedule_pause`, `schedule_resume`, `unschedule`; `hello` takes `tz` |

## Consequences
- Easier: "every day at 2 AM recount the stock" is one call and one approval, and the person
  sees it in that chat until it is stopped. A runtime on a server keeps the person's 2 AM.
- Harder: every new schedule or change asks, even for a person who said "always" to other work.
  That is the point: standing work spends money with nobody watching.
- Cost: the larger `agents.schedule` schema adds about 200 input tokens to each turn of a chat
  that orchestrates (simulated `board` workload: 14,899 to 16,075 tokens, +7.9%, success
  unchanged). The descriptions were cut from +17% before that.
- A run waiting for an answer at 02:00 blocks the next runs until someone answers it (tray,
  phone). Pre-approving an action ("Always in this project") is what makes it unattended.
- `done_before` repeats no identical call; a model that writes a different change on a retry is
  asked again for it (ADR 0037's limit). Nothing promises a change is not made in other words.
- Breaking: `When` has new fields (`date`, `cron`, `tz`): struct literals add
  `..Default::default()`. `EventKind::ScheduleChanged` is new. `TaskRecord::{slot,
  retry_delay_ms, retry_at_ms}`, `TaskSpec::retry_delay_ms` and `ScheduleRecord::{missed, paused,
  if_missed, overlap, retries, failures, counted}` are new fields (records saved before read
  with defaults). `ScheduleRecord::advance` is crate-private and takes the slot.
- The person's zone is one per app: the last client to say `hello` sets it for every chat.
- Not done: waking the computer for a run; end dates; editing in a form; ARK Desktop's Agents
  canvas still hides paused schedules and labels times with the browser's clock (its card); a
  "trying again at" time is in the machine's zone.

## Check
- `crates/ark-orchestrate/src/schedule.rs`, `src/cron.rs`: dates, cron, zones and daylight
  saving in New York (gap and fold), old records.
- `crates/ark-orchestrate/tests/schedules.rs`: pause, resume, change, run now, a pause after a
  restart; a run made before a crash not made again; a retry that continues without repeating a
  change, out of credit not retried; the wait to try again kept across a restart; missed once or
  let go; overlap; 3 failures pause at once and tell the chat; runs kept; the chat's words for
  all of it, `ask_each_time`, a change from another zone keeps the schedule's, Stop ends its own
  agent, Pause cancels a run waiting to try again, no schedule tools for agents; passed dates and
  too-frequent cron refused; an unknown id refused at once (no deadlock); a one-time schedule
  kept until its run ends, also one a stopped process left open.
- `crates/ark-interact/src/labels.rs`: `a_schedule_approval_shows_when_and_all_each_run_is_handed`.
- `crates/ark-ui/tests/page.rs` (WebKit):
  `a_schedule_asked_in_words_is_seen_run_paused_and_stopped_beside_its_chat`.
- `examples/writing-agent/tests/restart.rs`:
  `a_schedule_made_in_the_chat_survives_a_kill_and_runs_at_its_time` (real processes, a fake
  model: approved, SIGKILL, the next process runs it at its minute, the save asks, the result is
  posted in the chat).
- Live (2026-10-07, qwen3.5:27b on Ollama, a real `writer --serve`, every question answered
  "Allow once" by a script): "Every day at 2 AM, open stock.csv, recount the inventory with the
  changes in movements.csv, update stock.csv with the new counts, and give me a summary." The
  chat made `every day at 02:00` in Europe/Madrid (the zone the page sent), next "Thu 02:00
  CEST" on a machine in another zone, with a full brief. Run now: the run asked for two Run code
  steps and the change to `stock.csv`, updated it, and its summary was posted in the chat.
  Found: before scheduling, the chat wrote sample `stock.csv` and `movements.csv` of its own over
  the person's (asked, and allowed by the script); a person would have seen and could deny it.
