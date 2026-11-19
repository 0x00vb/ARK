# 0128. The Computer asks once a turn, and an Auto chat does not ask

Status: Accepted. Date: 2026-11-19. Amends 0018 (every action asked), 0042 (the click asked
with a ring on the spot), 0045 (the turn-on question's second line) and 0123 (the Computer
asks in Auto).

## Context

ADR 0018 made every Computer action `ask_each_time`: no answer covered a later call, and
Auto (ADR 0107) asks for exactly those. The owner on 2026-11-19: ARK "asks permissions for
every single action". While an agent works in another app, each question is in ARK's window,
so the person switches windows for every look, click and keystroke. The owner asked for one
question when the agent takes control, and none when they already said in their message what
the agent should do.

A test through the real harness and gate (`crates/ark-computer/tests/asks.rs`) showed it on
v0.52.0: look, type, click in Notes asked 3 times in an Auto chat, and 3 times in a chat that
asks, even after an answer "for this session".

## Decision

| Part | Decision |
|---|---|
| Class | Every Computer action but `apps` is still `Mutate`, no longer `ask_each_time`. All share one approval class, `computer` (`ark_computer::GRANT`; a `GrantKey` with no fields is its group) |
| Once a turn | New descriptor flag `ActionDescriptor::once_a_turn` (`ask_once_a_turn()`), generic in `ark-core` and read by the harness. The first call of the class in a turn asks; a yes covers the rest of that turn (the screen lease ends with the turn anyway). The request goes to the gate as `ask_each_time`, so no answer is remembered for the session or the project |
| Auto chat | Runs Computer actions without a question when someone could be asked (a gate exists; a one-shot client with nobody to ask runs only what the agent may do alone). Auto is the person's own turn (ADR 0107, 0123), so their message is the permission. Delegated, started, group, scheduled and automation work is not Auto. A continuation of the person's Auto chat after colleagues they started finish (ADR 0126) runs in the chat's mode, as for every other change there |
| A chat that asks | "ARK wants to use your computer:" and that first step. Answers: Allow, Deny |
| An agent allowed to act alone (`allow_mutate`) | Still asks once a turn when someone can answer, so a scheduled or delegated run does not drive the screen while the person is away; it waits for them, as before, but once |
| Nobody to ask | Unchanged: with no gate it runs only when the agent may act alone |
| Automations | An unattended grant of a `once_a_turn` action is refused, as for `ask_each_time` (`automations.rs`), and the run guard asks for it (`authority.rs`) |
| A profile's guard | Its verdict decides as before: a guard that asks, asks at each call |
| Why not a session grant | The orchestrator shares one gate across every agent's tasks, so "Allow for this chat" would have let later scheduled runs use the screen unasked (found by qa) |
| What stays | One owner at a time, the person first (`QUIET`), "‹who› is using your computer · Stop", Stop failing the owner's later actions, refusing a name that is not the number's, keys only to the app in front, message draft and send proofs (ADR 0095) |

## Consequences

- A turn on the screen asks at most once, and not at all in Auto.
- Sending a message (`computer.send`) is covered too: the owner's rule is that their own words
  are the permission. In Auto a send the person did not ask for runs unasked, as a shell
  command already does; the pill shows it and Stop ends it. A web page or file an agent reads
  could ask it to use the computer.
- The question with a picture and a ring on the spot (ADR 0042) shows only when a click is
  the first Computer action of a turn that asks, which a picture normally precedes. The code
  stays; no page scenario reaches it now.
- An old lasting grant `computer:<app>` no longer matches any call.
- Turning the Computer on (ADR 0045) no longer says "It still asks before each step.": the
  question is only "Allow ARK to use the apps on this computer".

## Check

- `crates/ark-computer/tests/asks.rs`: Auto asks 0 times and types and clicks; a chat that
  asks asks once a turn and remembers even an "always" answer for no later turn; an agent
  allowed to act alone still asks once; a Deny touches nothing. On v0.52.0 the first two
  asked 3 times; without the `allow_mutate` rule the third asked 0 times.
- `crates/ark-orchestrate/tests/chat_mode.rs`, `the_screen_runs_in_the_persons_auto_turn_and_asks_in_work_nobody_watches`:
  the person's Auto turn runs it; a scheduled run of an agent that acts alone and work an agent
  starts ask. Without the `allow_mutate` rule the scheduled run drove the screen unasked.
- `crates/ark-computer/tests/computer.rs`, `an_agent_reads_a_window_and_acts_on_it_by_number`:
  every action but `apps` is in class `computer` and none asks each time.
- `crates/ark-interact/src/approval.rs`, `a_narrow_grant_names_what_it_covers_on_its_button`:
  the title and the two answers.
- `crates/ark-core/src/action.rs`, `a_grant_key_splits_answers_by_input_and_never_widens`.
- `crates/ark-ui/tests/page.rs` (WebKit): `the_person_sees_who_uses_the_computer_and_takes_it_back`
  (one question, then Stop while another question waits; the click is not asked, and the
  screen log has no press), `an_auto_chat_uses_the_computer_unasked_and_shows_what_it_saw`.
