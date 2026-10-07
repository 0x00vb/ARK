# 0048. A chat turn keeps its progress and continues where it stopped

Status: Accepted
Date: 2026-10-07

## Context
The owner left ARK working overnight and the laptop crashed. Reopened, the chat showed the
prompt and "stopped"; everything the turn had done was gone from view, and a follow-up started
over. ADR 0037 and 0038 made orchestrated tasks survive a restart. The chat's own turn was left
out: "a chat turn (not a task) still ends with the process", "the chat's own turn is not
continued after a restart".

An audit of `main` (3dfbff9) found:

| # | Gap | Where |
|---|---|---|
| G1 | The turn's trace (narration, calls, results: what the window replays) was written only after the turn returned | `Interaction::run_turn` |
| G2 | While a turn ran, its stored record was a placeholder with the input only | `Session::begin_turn` |
| G3 | Every tool call was already durable (write-ahead `Running` record, outputs, artifacts) but nothing tied a call to the chat turn that made it | `Engine::run_action` |
| G4 | The next turn's carry-over said only "did not finish"; the model started over | `Session::conversation_digest` |
| G5 | Chat turns never got `Harness::done_before`: a change the stopped turn made was made again | `Ark::ask_session` |
| G6 | A change cut short by the crash (it may have happened) ran again without asking | same |
| G7 | A stop by the app (SIGTERM, quit) stored the turn as cancelled, shown "Stopped." like the person's Stop | ADR 0038 |
| G8 | Model calls were retried twice (about 1.5 s); a short network drop failed a long turn | `RetryPolicy` |
| G9 | Blobs were renamed into place without an fsync, and an object of the wrong size was never rewritten | `Store::put_blob` |
| G10 | After a crash the chat's hand-off is still running; a "continue" handed the same work off again | chat `agents.delegate` |
| G11 | Liveness was the pid alone. After a reboot the pid of a dead run is soon another program's: the turn stayed "running" and its next turn overwrote it | `process::alive` |
| G12 | A finished turn row whose header was not yet saved was shown "did not finish" | `Session::load` |
| G13 | A chat whose first turn was cut short had no title and 0 counted turns: the window's list hid it | `Session::load` runs only when a chat is opened |

## Decision
No new work type and no checkpoint concept for the person. The records that already exist say
what was done; the next turn reads them.

| Question | Decision |
|---|---|
| What a stopped turn keeps | Its calls, as today, now naming their work: `Invocation::work` is `session:<id>/<turn>`, stamped by `Engine::for_work` before the call runs. Its trace, saved as the turn runs: at once after a result, a question, an answer, steering or the end, other events at most once a second (`Journal`, `ARK_TRACE` applies). The window shows each call as its record says (`activity::settle`): done, failed, cut short, or not run, even one the trace never got to |
| How the person goes on | Whatever they send next: "continue", "retry", or a new instruction. The window offers one **Continue** on the chat's last turn when it did not finish, on reopening and as soon as a turn fails (it sends "Continue" as their message). The terminal shows the same line; the person types |
| What the next turn knows | The unfinished turns at the end of the chat (up to 3 in a row) and their calls (`Session::unfinished`). The carry-over lists the calls, the newest 10, one clipped line each (done, cut short, failed), and what the tools keep from them (`Adapter::continued`: a hand-off still under way), and says to continue from there. Bounded, never the transcript |
| A change made twice | Their successful changes go to `Harness::done_before`: an identical call gets the earlier result, as for tasks (ADR 0037) |
| A change cut short | One that was running when the process died: an identical call is asked of the person whatever was granted, until they allow it once (`ApprovalRequest::cut_short`). Both front-ends ask "This was cut short and may already have happened. Do it again?" with Skip first (the default for Enter and Esc) and Do it again. With no one to ask it is never run again; the model is told only the person can tell |
| A hand-off made twice | A task the chat hands off keeps the turn's work name (`TaskSpec::work`). The turn that continues takes up, once per agent, a task its stopped turn handed that agent and that is still open (`Orchestrator::take_up`), and the model is told it is the earlier hand-off. Asked in other words, it may be other work: the call returns at once with the earlier task under way instead of waiting for it, and the model hands off again if it wants something else |
| A stop by the app | `Engine::stop_soon` ends the turn with `HarnessEvent::Interrupted` and stores it as interrupted, as a crash does. This replaces ADR 0038's "a chat's turn ends as cancelled". The person's Stop stays "Stopped." with no Continue (they can type) |
| Who wrote a record | `pid` plus the writer's exact start (`pid_start`), on turns, calls and tasks. macOS reads it with `sysctl(KERN_PROC_PID)`, which works for any user's process (`proc_pidinfo` refuses them, and after a reboot the pid of a dead run is often a system daemon's); Linux from `/proc/<pid>/stat`. A pid alive with another start is another program. Records without a start fall back to the pid |
| Opening the store | Recovers begun chat turns of dead processes, as it recovers their calls, so the chat is listed with its title before anyone opens it. A finished row the header did not count is counted as it is |
| Waiting for a model | 4 retries (was 2) for a temporary error. A model that cannot be reached at all (no connection, a timeout) is waited for up to `ARK_MODEL_OFFLINE_S` (60 s) more. Not a server on this machine or a refused connection: nothing listens there, and waiting does not start it |
| Blob writes | A unique temp file, `sync_all`, rename, then the folder synced. An object whose size is wrong is written again. A failed write removes its temp file; opening the store removes temp files over an hour old |

A chat turn is not continued on its own after a restart: the person is there and says so in
one word or click. Tasks still continue on their own (ADR 0037).

## Consequences
- A crash loses at most the streamed text of the last second and the call under way; the window
  shows the rest and where it stopped.
- The live trace rewrites the turn's record at each save. Measured on a 60-call turn with a mock
  model (release build): 36 ms for the turn against 19 ms with `ARK_TRACE=off`, about 0.3 ms per
  call; the final trace is 95 KB, so the turn writes about 3 MB in all. A turn is bounded by its
  step limit; an append-only trace can come if longer turns need it.
- `done_before` hides a deliberate repeat ("save it again") in the continuing turn only; the
  model gets the earlier result and can say so. A change repeated with a different input is
  guarded only by the carry-over's list, as for tasks.
- Observe calls that write records (`board.add`, `agents.message`) are not deduplicated; only
  hand-offs are taken up.
- On macOS `sync_all` is `F_FULLFSYNC`; a blob write waits for the disk, about 9 ms per blob on
  the owner's Mac (the reviewer's measure, 50 writes). SQLite stays WAL with
  `synchronous=FULL` and plain `fsync`, so a power loss on macOS can lose the last commits.
- Windows has no process start time yet: a reused pid there still looks alive (`ark-os` work).
- New fields: `Invocation::{work, pid_start}`, `Turn::pid_start`, `TaskSpec::work`,
  `TaskRecord::{work, pid_start}`, `ApprovalRequest::cut_short`, `RetryPolicy::offline`,
  `RunCtx::{work, continues}`, `TranscriptTurn::{interrupted, resume}`; a new
  `HarnessEvent::Interrupted`. Breaking for code that builds these with struct literals or
  matches every event (`CHANGELOG.md`).

## Check
- `crates/ark-sdk/tests/crash_recovery.rs` (a child process killed with SIGKILL):
  `killed_after_a_change_continues_without_making_it_again`,
  `a_change_cut_short_by_a_crash_is_asked_again_whatever_was_granted`,
  `a_change_cut_short_is_not_run_again_without_anyone_to_ask`.
- `crates/ark-sdk/tests/continue.rs`: the app's stop is interrupted and continues; the
  person's stop is "Stopped." and its change is not made again; another chat is not continued;
  a stored turn is counted as it is.
- `crates/ark-interact/tests/crash.rs`: a killed turn shows its calls (done, cut short), the
  interrupted line and the offer to continue, in the window's and the terminal's words.
- `crates/ark-orchestrate/tests/orchestration.rs`,
  `a_turn_that_continues_a_stopped_one_takes_up_its_handoff_instead_of_making_another`.
- `examples/writing-agent/tests/restart.rs`,
  `a_chat_turn_killed_after_a_save_continues_without_saving_again`: a real `writer --serve`,
  killed after a save, restarted; "Continue" finishes and the chapter is saved once.
- `crates/ark-ui/tests/page.rs` (WebKit, `ARK_UI_PAGE_TESTS=1`),
  `an_interrupted_turn_shows_what_it_did_and_continues_on_one_click`.
- Unit tests: `process` (a reused pid), `put_blob` (a cut-short object), `resilience` (an
  unreachable model).
