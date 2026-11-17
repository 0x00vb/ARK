# 0108. An agent checks what it changed, sees what it shows, and knows what it has

Status: Accepted. Amended by 0127 (nudges after each step, a first-reply note).
Date: 2026-11-08

## Context
The owner's ARK Desktop store (`~/.ark-desktop/meta.db`) held 71 traced turns from 2026-10-03
to 2026-11-07. The answers that failed the owner had the same few causes.

| # | What happened | Evidence | Cause |
|---|---|---|---|
| C1 | The agent changed files or started a server, did not look again, and answered as if it worked. The owner's next message said it did not | `self_check` passed in 61 of 62 answered turns; about 10 next messages were "no veo los graficos", "nothing worked", "why is it empty" | `self_check` (`harness.rs`) passes when any evidence was cited. It never asks whether a change was checked after it was made |
| C2 | The agent could not read a screenshot the owner added: "my file reader can't open it, and it can't read images in any case" | Noko chat, 2026-11-07 | `files.read` returned text only for pictures, although both CLI providers read pictures and `ActionOutput::with_image` exists |
| C3 | The agent opened a page or the canvas and said "I can't see what your view draws" | Noko and website chats | `browser.read` returns text and elements only. No action returns a picture of the view |
| C4 | The agent said "this session has no Node terminal" and started the dev server one turn later; it said it could not open desktop apps and opened WhatsApp in another chat | RedAgro chat, 2026-10-07 | The first message lists what can be added, never what the session has. The model guessed |
| C5 | A new chat answered "hello" with a Rust program from another chat of the same project | default project, 2026-10-03 to 2026-10-06 | `research_state` is keyed by project, and every chat's request carries it as `== WORKING STATE ==` |

## Decision

| Rule | Detail |
|---|---|
| One check nudge per turn | A change needs checking when it is a Mutate call that succeeded and whose namespace (text before the first `.`) has an Observe action; a look counts when a Read or Observe call succeeded. When the model answers and such a change came after the last look, the harness adds one round: check the change (read back, run, look) or say plainly it is unchecked. Never twice in a turn. While that nudge is still possible, the model's text is held, so an answer that gets the nudge is never shown and taken back. `self_check` reports `changed N; checked after: yes/no` and fails when the change stayed unchecked after the nudge or when the turn ran out of steps. That failure does not start the critic: the nudge already asked |
| Stored text holds no pasted secret | Before a turn, its trace, its working state or a note is stored, URL passwords, `NAME=value` where the name ends in KEY, TOKEN, SECRET or PASSWORD, and `sk-`, `ghp_`, `github_pat_` and `AKIA` keys become `[redacted]`. The running turn keeps the real text. Files and tool call records are not changed |
| Pictures are read | `files.read` attaches png, jpeg, gif and webp up to 3.5 MB as a picture. `browser.screenshot` (Observe) returns the browser view as a JPEG |
| The session states what it has | The first message of a turn carries one line: terminal, local pages, pictures, Computer. It is in the first message, not the system prompt (ADR 0102). It uses the same availability rules as the offered tools (Plan mode, the agent's profile) but counts preloaded actions that ADR 0102 withholds from the first Claude Code request |
| Working state belongs to a chat | `research_state` is keyed by project and session. Rows written before this change move to the session `#legacy`, which nothing reads. A run without a chat uses its own key: the task id for orchestrated work, the run id for an automation run, `""` for a CLI one-shot. Project memory stays project-wide (ADR on project memory) |

## Consequences
- A turn that changes something and never looks costs at most one more model round. A turn
  that already looked costs nothing more.
- A volatile Mutate call (`computer.look`, `computer.see`) asks for consent but looks: it counts as a look, not a change. A file write is not nudged: its own result confirms it. `self_check` counts only checkable changes, so a turn that only wrote files reads as before.
- The capability line costs under 60 tokens per turn and does not touch the cached system prompt.
- A picture costs `PICTURE_TOKENS` per read. The working budget already drops old pictures.
- What is on screen in the browser view, a signed-in page included, goes to the model provider
  when the agent calls `browser.screenshot`, as `browser.read` text already does.
- Old project-wide working state is ignored, not deleted. A chat that relied on it starts fresh.

## Rejected
- A model call to judge every answer: it adds a call to every turn. The nudge adds one only
  when a change went unchecked.
- Auto-attaching a screenshot after every browser action: pictures are expensive; the agent asks
  for one when it needs to see.
- Putting the capability line in the system prompt: it breaks whole-prompt caching (ADR 0102).
