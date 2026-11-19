# 0128. The Computer asks once, and an Auto chat does not ask

Status: Accepted. Date: 2026-11-19. Amends 0018 (every action asked) and 0042 (the click
asked with a ring on the spot).

## Context

ADR 0018 made every Computer action `ask_each_time`: no answer covered a later call, and
Auto (ADR 0107) asks for exactly those. The owner on 2026-11-19: ARK "asks permissions for
every single action". While an agent works in another app, each question is in ARK's window,
so the person switches windows for every look, click and keystroke. The owner asked for one
question when the agent takes control, and none when they already said in their message what
the agent should do.

A test through the real harness and gate (`crates/ark-computer/tests/asks.rs`) showed it:
look, type, click in Notes asked 3 times in an Auto chat, and 3 times in a chat that asks,
even after an answer "for this session".

## Decision

| Part | Decision |
|---|---|
| Class | Every Computer action but `apps` is still `Mutate`, no longer `ask_each_time`. All share one approval class, `computer` (`ark_computer::GRANT`, `ActionDescriptor::grant_per("computer", &[])`: a grant key with no fields is its group) |
| Auto chat | Runs Computer actions without a question. Auto is the person's own turn (ADR 0107, 0123), so their message is the permission. Delegated, started, group, scheduled and automation work is not Auto |
| A chat that asks | The first Computer action asks once: "ARK wants to use your computer:" and that step. Answers: Allow in this chat (the session grant), Always allow (the project's lasting grant, removed in `/grants`), Deny. No "Allow once": it would ask again at the next click |
| Nobody to ask | Unchanged: with no gate nothing runs unless the agent may act alone |
| An agent allowed to act alone (`allow_mutate`) | Uses the computer without a question, like its other changes. ADR 0123 kept it asking only for `ask_each_time` actions, which the Computer no longer is |
| What stays | One owner at a time, the person first (`QUIET`), "‹who› is using your computer · Stop", Stop failing the owner's later actions, refusing a name that is not the number's, keys only to the app in front, message draft and send proofs (ADR 0095) |

## Consequences

- A long task on the screen asks at most once a chat, and not at all in Auto. Stop is the
  remedy for an agent doing the wrong thing, as for every other change in Auto.
- A web page or file an agent reads could ask it to use the computer. In Auto that now runs
  unasked, as a shell command already does. The pill shows it and Stop ends it.
- The question with a picture and a ring on the spot (ADR 0042) shows only when a click is
  the first Computer action of a chat that asks, which a picture normally precedes. The code
  stays; the page scenario no longer reaches it.
- An old lasting grant `computer:<app>` no longer matches any call.
- Turning the Computer on (ADR 0045) no longer says "It still asks before each step.": the
  question is only "Allow ARK to use the apps on this computer".

## Check

- `crates/ark-computer/tests/asks.rs`: Auto asks 0 times and the click happens; a chat that
  asks asks once for look, type and click; a Deny touches nothing. The first two fail before
  this change (3 questions each).
- `crates/ark-computer/tests/computer.rs`, `an_agent_reads_a_window_and_acts_on_it_by_number`:
  every action but `apps` is in class `computer` and none asks each time.
- `crates/ark-interact/src/approval.rs`, `a_narrow_grant_names_what_it_covers_on_its_button`:
  the title and the three answers.
- `crates/ark-core/src/action.rs`, `a_grant_key_splits_answers_by_input_and_never_widens`.
- `crates/ark-ui/tests/page.rs` (WebKit): `the_person_sees_who_uses_the_computer_and_takes_it_back`
  (one question, then Stop while another question waits; the click is not asked and fails),
  `an_auto_chat_uses_the_computer_unasked_and_shows_what_it_saw`.
