# 0050. An agent never loses its task: a pinned task frame, a working context sized for the model, a bad reply the model repairs

Status: Accepted
Date: 2026-10-07

## Context
In ARK Desktop (v0.13.0, Codex), the owner asked for syntax highlighting in the file editor.
The agent read the editor's code and stated a plan. Then:

1. Its `files.edit` call carried arguments that were not JSON (JSON inside a string; fixed in
   ADR 0049). The retry was blocked because text had been shown (also ADR 0049), and the error
   ended the turn.
2. The owner wrote "cointinue". The new turn read everything again.
3. Some steps later the turn forgot the task. Its own words: "the original requested change was
   lost from the compressed context". It worked on something else and ended there.
4. It tried `cargo test` and concluded "this environment has no cargo executable".

The store of the owner's ARK Desktop (`~/.ark-desktop/meta.db`, session `dcd093f2`, turn 1)
shows why.

| # | Cause | Evidence |
|---|---|---|
| C1 | The task is the first round of the working set (`WorkingSet`, `budget.rs`), and rounds are dropped oldest first. The task, the conversation and ADR 0048's carry-over all sit in that round | Context reports: `task 32, conversation 80` at steps 1 and 2; `task 0, conversation 0` from step 3 on; 12 rounds dropped by step 13 |
| C2 | Codex ran through a vendor session (`ManagedSession`, on by default). The vendor thread kept everything it had been sent, so it still had the task. At 64k tokens (`SessionPolicy::max_context_tokens`) the session rotated, and the new thread started from ARK's own request, which no longer had the task | `prompt_tokens` 20,432 → 66,028 over steps 1 to 7, then 19,883 at step 8. At step 8 the agent ran `search_evidence "GOAL cointinue ..."` |
| C3 | ARK's request budget is 6,000 tokens for every model (`ARK_CTX_BUDGET`). It was tuned on qwen3:4b. One file read fills it. Codex's window is 272k | `budget 6000` at every step; step 6 was 16,766 tokens because the newest round alone was over budget |
| C4 | The note left after dropped rounds says "their conclusions are in the research state". That state holds what the model chose to note, and here it noted nothing | `research_state`: 0 findings, 0 next actions |
| C5 | A turn sets its goal, rules, skills, preloaded tools and its "trivial" class from the person's new message. "cointinue" became the goal and was classed trivial, which turned the critic off | `goal = "cointinue"`; `classified trivial: true` |
| C6 | A malformed reply was sent again unchanged and failed the turn once retries ran out. The model was never told what was wrong | `harness.rs` `call_model` |
| C7 | `code.run` is contained by design (ADR 0041: "This is the difference from Run code"): `PATH=/usr/bin:/bin`, `HOME` in a scratch folder, and it can read only the system, the project and its runtime. The agent called `cargo` from Python in it. `process.start` runs commands with the person's login shell, but ARK Desktop does not turn processes on | The `code.run` call of 13:09:15 (`subprocess.run(['cargo', ...])` → `FileNotFoundError`); `ark-desktop/src/main.rs` has no `.processes(true)` |

What ADR 0048 and 0049 already give: a turn's calls survive a crash and name their turn; the
next turn is told what they did and does not repeat a change; a change cut short is asked about
again. Codex's tool arguments are typed objects for every schema strict mode can express, which
covers all of ARK's own tools, and shown text no longer blocks a retry. What neither gives: the
task survives the turn's own context limit and a vendor session's rotation; the goal survives
"continue"; the budget fits the model; a bad reply becomes feedback; commands run where the
person's programs are.

## Invariant
ARK may drop raw context. It never drops the active goal, what the person said about it (the
newest instructions in full, a count of older ones), what was done, what was under way, or why the work stopped. A follow-up after an unfinished turn
continues the same work. All of this is held in records that survive the process: turns, calls
and evidence. Any request the model gets, including the first one of a rotated vendor session,
carries it.

## Decision

| Question | Decision |
|---|---|
| The task frame | The working set has a pinned head (`WorkingSet::pin`) that is never dropped. The harness pins the first message: the goal and what the person said about it, ADR 0048's carry-over, the conversation's carry-over, rules, skills and what is ready to call. Only rounds after it are dropped. A vendor session that rotates starts from ARK's request, so it gets the head too. When the system prompt plus the head is over the budget, both are still sent with the newest round (as today for an oversized round); the budget then bounds nothing else |
| What dropped rounds leave | A ledger instead of "conclusions are in the research state": one line per call the dropped rounds made, in order, the newest kept and a count of older ones. A line has the action, its key input (`path`, `query`, `command`, `url`, `name`; 60 characters), the first line of its result card (100 characters) and `inv:` to read it again. At most `budget / 400` lines (10 to 40). Below them is the model's last note among those rounds, its plan, clipped to 600 characters. Deterministic, no model call. It changes only when more rounds are dropped, which happens in chunks (`REFILL`), so the prefix cache keeps it. A live vendor session gets each new ledger as one more message (up to about 1k tokens per drop chunk), a small copy of what it already has; it stays, because a rotated session needs it |
| What the person said while it ran | Steering messages also go in the working state at the end of every request ("The person also said: …", the newest 5, 300 characters each). They can no longer leave with a dropped round. Each one reaching the model is also added to the stored turn at once (`Session::steered_now`), so a crash keeps it in the goal |
| The goal | `Turn::goal` (`Goal { request, follow_ups }`): the request this turn works on and every later message about it (follow-ups and steering, newest last; the newest 8 at 500 characters each with a count of older ones, the request up to 4,000 characters). The request is the turn's input as the app passed it, even when it is empty (files only). The harness receives it with `Harness::goal(Goal)`; `ask`'s `question` stays the person's newest message. It is written when the turn begins, so a crash keeps it, and every record path keeps it (`record_turn`, `fail_turn`, the recovery on open). A turn after an unfinished one (failed, interrupted, or stopped by the person, as ADR 0048) takes that turn's goal and adds its own message. A chain of any length keeps the first request and every instruction. A finished turn ends the chain. The frame says to follow the newest instruction where they differ. No word is special: "continue", "retry" or a new instruction all continue (ADR 0048). Old turns without a goal read as their input (`#[serde(default)]`) |
| A turn that ran out of steps | When the step limit or the stall guard ends the loop before the model answers, the answer is asked for as before and shown, and the turn is marked (`HarnessOutcome::ran_out`, `Turn::ran_out`). It counts as unfinished for the next turn: its goal continues, its changes are not made again, and the model is told it reached its step limit. The person sees its answer as before. This is where a person most often types "continue" |
| What the goal decides | The research state's goal, rule selection, skills, preloaded tools and the trivial class read the goal with the person's newest message, not the newest message alone. The working state shows `GOAL:` clipped to 300 characters, since the full goal is in the head |
| What a stopped turn hands on | ADR 0048's carry-over, extended: each call as the ledger line above (not raw JSON), the newest 20; the model's last note when it made its last call (`Invocation::narration`, written in the call's write-ahead record, so it survives a crash; the harness hands it over with `Engine::narrating`, as `Engine::for_work` hands the work name); and why the turn stopped (`Turn::error`, clipped to 300). Calls and `done_before` come from every turn of the chain (up to 20 turns, read from the store), not only the last 3, so a fourth "continue" cannot repeat a change |
| The request budget | `Provider::context_window()` (a default method, `None`): the input tokens a request may have. The harness resolves the budget from the provider in its `Role::Reason` slot, so each agent of an orchestration gets its own: a quarter of the window, at least 6,000, at most 48,000, never more than half the window. Unknown windows keep 6,000. Known windows: Claude models 200k (API and Claude Code), Codex 272k, OpenAI by model family, a named endpoint when it declares one (`Endpoint::context_window`, set with `.context_window(n)`). A vendor session's window is its rotation limit (`SessionPolicy::max_context_tokens`, 64k): each call is billed for the whole vendor thread, so a subscription lane gets a 16,000 budget and its threads stay as large as today. A budget the app or `ARK_CTX_BUDGET` sets wins (`Settings::context_budget: Option<u32>`). Every `Provider` wrapper passes the window of the model it wraps; a test checks each one |
| A small window | The parts of the head that grow scale with the budget: carried calls `budget / 600` (5 to 20), the goal's request `budget / 2` characters (1,000 to 4,000). With a known window the request stays under it except for one oversized newest round, as today |
| A reply that cannot be used | `ProviderError::Malformed` means the model's own output cannot be used: arguments that do not parse, a reply that is not structured, a reply cut off. It is not resent unchanged. Shown text is withdrawn (ADR 0049), and the model gets a new round: "Your last reply could not be used: <detail>. Nothing ran. Call the tool again with valid arguments; split long text into several smaller edits." The loop goes on. The front-ends see the existing `Retry` event with `repair: true` (`#[serde(default)]`, so stored traces stay readable by older apps), worded as the model fixing its reply, not as a failed call. At most 2 repairs in a row and 4 in a turn; then the turn fails, keeping its goal and calls for Continue. Both front-ends show it in `ark-interact`'s words ("The model's reply could not be used."), not the provider's detail, which stays in the record. A refused schema is no longer `Malformed`: it is retried as before. This round is the backstop for every provider (Claude Code, APIs, tools whose schema strict mode cannot express) |
| Commands | `code.run` stays contained. Its description says so: a project's own commands go through `process.start`. `process.start` says it also runs a command to its end (tests, a build): `wait` up to 300 s, and the answer has the exit code and the end of the output. Without `process.start`, the model can say the command cannot run here, not "there is no cargo". ARK Desktop turns processes on (`.processes(true)`, ADR 0041; it pins v0.13.0, which has them): a card on the board, in ark-desktop |

Ruled out:
- Sending the whole transcript. The head and the ledger are bounded, and the budget is a
  fraction of the window.
- A model-written summary of dropped rounds: it costs a call, and a weak model can lose the
  goal in its own summary.
- Recognizing "continue" or "retry". ADR 0048 already continues on any message.
- Giving `code.run` the person's PATH: it still could not read `~/.rustup`, and containment is
  its point.

## Risks
- `process.start` runs outside any sandbox, as the person, with their environment and the
  network. Every start is a Mutate call: it asks unless granted, the grant is per command and
  folder (`grant_per`), and with nobody to ask it does not run (ADR 0041). Turning it on in ARK
  Desktop puts that question in front of the person.
- A bigger budget means more input tokens per step for API models (up to 48k). It also means
  fewer steps spent reading again what was dropped. Subscription lanes keep 64k vendor threads.
  Measured below against Claude Code.

## Consequences
- Small models (unknown windows): the budget stays 6,000, but the request changes. The head
  stays for the whole turn instead of leaving at the first drop, the ledger replaces the note
  (up to about 1k tokens), and steering is in the working state. A long turn has less room for
  its newest rounds. The simulated bench runs before and after; `bench/baseline.json` is
  regenerated in the same commit if numbers move, and a live qwen3:4b run is recorded.
- New fields: `Turn::goal`, `Turn::ran_out`, `HarnessOutcome::ran_out`, `Invocation::narration`, `ContextReport::ledger`, `Retry::repair`
  (all `#[serde(default)]`); `Endpoint::context_window` (Breaking for struct literals; use
  `.context_window(n)`); `Provider::context_window` (a default method); `Harness::goal`,
  `Engine::narrating`, `Session::steered_now`, `WorkingSet::pin`. `Settings::context_budget`
  becomes `Option<u32>` (Breaking); Settings › Advanced shows "by model" when it is `None`.

## Check
- `budget.rs`: the head stays with any number of rounds; the ledger keeps the newest lines with
  their input and the last note; it is bounded.
- `crates/ark-runtime/tests/long_task.rs`: 80 calls with large reads under a 6,000 budget, with
  steering midway. Every request holds the goal, the steering and the ledger. With a
  `ManagedSession` over a scripted backend that rotates, the first request of each new vendor
  session holds the goal.
- `crates/ark-runtime/tests/retry.rs`: a malformed reply after text becomes a repair round, the
  tool runs once, the turn finishes; a third malformed reply in a row fails the turn; a refused
  schema is retried without a repair round.
- Budget: a 200k window gives 48,000, an unknown one 6,000, an 8k window 4,000, and
  `ARK_CTX_BUDGET` wins. Each `Provider` wrapper passes the window on.
- `session.rs`: the goal survives `record_turn`, `fail_turn` and the recovery on open; a chain
  X, "use tree-sitter" (fails), "continue" keeps X and "use tree-sitter"; a fourth continue
  does not repeat turn 1's change.
- `crates/ark-sdk/tests/lost_task.rs`, the owner's sequence. A child process runs "add syntax
  highlighting to the editor" on a scripted model: several large reads, a malformed large edit,
  then a SIGKILL. The parent reopens the store and sends "cointinue". The first request holds
  the original goal, the reads, the last note and the failure. The model repairs the edit, runs
  the project's test with `process.start` (exit code and output reach it), and finishes. The
  file has the change once, and the research state's goal is the original request. A second
  variant: the turn fails on malformed replies, then the process restarts and continues.
- Live, with the owner's subscriptions: the same task on a copy of ARK's editor, on
  `codex:default` and on `claude:sonnet`, killed mid-turn and continued with "continue". It
  keeps its goal and ends with highlighting working and its test run. The same task in Claude
  Code (`claude -p`), with input tokens compared. The owner asked for these runs (2026-10-07).
- A small window (8k): the assembled request stays under it with a long chain and carry-over.
- `crates/ark-ui/tests/host.rs`: both front-ends send the same head and ledger.
- `ark-proc`: `process.start` finds `cargo` through the login shell and returns its exit code.
- `scripts/ci.sh` with the bench gate; a live qwen3:4b run on the bench's long workloads.
