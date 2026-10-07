# 0051. A turn ends on its answer and follows the project's instructions

Status: Accepted
Date: 2026-10-07

## Context
In ARK Desktop (`claude:sonnet`), the owner asked again for syntax highlighting in the file
editor ("analyze it, make a small plan and implement"). The store of the owner's ARK Desktop
(`~/.ark-desktop/meta.db`, session `9a7331b8`, turn 1) shows the run.

| Step | The reply |
|---|---|
| 1 to 4 | one short note (39 to 82 characters) and 3 or 4 reads |
| 5 | the plan (205 characters) and three writes: the change |
| 6 to 10 | a full answer (1,431 to 1,798 characters, 13 to 25 lines) and 1 to 3 reads of the files it had just written |

Ten model calls, 24 tool calls, 551k input tokens, 118 s. The person saw the answer five times.

| # | Cause | Evidence |
|---|---|---|
| C1 | Any reply with a tool call continues the turn. The note beside the calls is not sent back while its round is shown (`budget::sent`, ADR 0050, measured), so the model does not see that it already answered, and writes it again with one more check | Consecutive answers share 61 to 67% of the shorter one's words |
| C2 | Reads of files the turn wrote count as progress (each creates a card), so the stall guard never fires | `cards_created` grows at steps 6 to 10 |
| C3 | The project folder's own instructions never reached the model. ARK reads `.agents/skills` from a project folder, not `AGENTS.md`. The workspace's `AGENTS.md` says where work goes, that UI work loads a design skill, that work goes on the board, and that the repo's checks pass before "done" | Context reports: `rules: 0` at every step |
| C4 | It never ran a check. It read back what it had written and called that checking | No process or test call in the turn |

The vendor thread also grew to 155k tokens at step 6. That is ADR 0050's choice (subscription
lanes keep vendor threads up to 64k) plus internal Claude Code calls on long replies. It is not
changed here: a live run is needed to see what Claude Code does on those steps.

## Decision

### A restated answer ends the turn
A reply is a check of the turn's own work when all of these hold:

- its text reads as a full answer: at least 600 characters on at least 3 lines (`ANSWER_CHARS`,
  `ANSWER_LINES`);
- the turn has written files;
- every call is a registry action that changes nothing and has a `path` naming a file this turn
  wrote, or a folder holding one. A meta tool, or a call without a path, is not a check.

The first check runs. A second check whose text repeats the first (at least half of the shorter
one's words, `RESTATED`) ends the turn: its calls do not run and its text is the answer. It
goes through the same steps as any answer (steering, claimed files, announced work), so the
turn has `answered` set and is not "ran out" for ADR 0050. A change in between starts over. No
new event: `HarnessEvent` has no `#[non_exhaustive]`, and the `Stall` event would show a
warning and mark the turn unfinished.

Deterministic, no model call. On the owner's run it ends at step 7 instead of 10.

### The project's own instructions are rules
`ArkBuilder::project_instructions(names)` names the files a project folder keeps its
instructions for agents in. The default is `["AGENTS.md"]`: the open convention several agent
tools read, and what a folder of chapters can use as well as a repo. An app can name others, or
none to turn it off. A name is a file name, never a path.

Each conversation turn reads the first file found at the folder's root, in Markdown sections:

- the first section always (cut to fit), then the sections that share the most words with the
  task, while the whole block fits `INSTRUCTIONS_BUDGET` (1,500 tokens); they are shown in the
  file's order;
- the sections left out are named (at most 12, each name cut to 48 characters), to be read from
  the file;
- subfolders up to 2 levels deep holding one of the names are listed: "read the one for a folder
  before you change files in it". Hidden folders, build output and links are not walked; at
  most 4,000 entries are read and 12 files named.

The block goes before the bound rules (`RuleSelection::with_project`), so it shows in
`ContextReport::rules` and in the `Rules` event as rules of the file's name; the activity line
reads "Following N rules (M more not shown)" with "AGENTS.md: <headings>". It says whose it is:
"From the project's own file AGENTS.md (written for this project, not by ARK). Follow it where
it applies to this task; the person's request comes first."

Safety: the file is the person's folder, but may come from a cloned repository. A link is not
followed. At most 64 KiB is read. It is not stored in the `RuleBook` (no version per edit). The
block is labelled as the project's text.

### Not done: two more lines in the default instructions
Two lines were tried in `DEFAULT_INSTRUCTIONS`: check the result after a change the way the
project's instructions say (reading back what was just written is not a check), and one short
line beside a tool call with the full answer once. In the simulated suite they cost about 95
tokens a request (+5.8% input tokens, +2.2% cost per success, every workload) and scripted
models cannot show a gain. They wait for a live run on a hosted model that shows they pay. The
project's own instructions (above) already carry a project's checks to the model.

## Consequences
- Fewer calls and tokens on turns that change something. The simulated suite has no workload
  that restates an answer, so the bench gate does not exercise it; `restated_answer.rs` does.
- Projects with a large `AGENTS.md` send up to about 1,500 more tokens in the pinned task frame.
  It is a stable prefix, so a prompt cache keeps it.
- Agents of an orchestrator do not read the project file yet (`rules_for`): their scope is a
  project of their own. When a run shows they need it, the same function serves.
- On the workspace this came from (`AGENTS.md`, 9,130 bytes), the block is about 1,500 tokens:
  the intro and the three sections closest to the task; the rest are named. A task about UI
  does not always share words with the UI section, so the model may need to read it.
- When a restated answer ends the turn, a vendor session (Claude Code, Codex) keeps that reply
  with calls that never ran. Those calls are fields of a structured reply, not API tool calls,
  so the next request is valid; the next turn's frame starts a new task. Not changed until a
  run shows the model confused by it.
- `CLAUDE.md` is not read by default: another tool's file name. An app adds it if it wants.
- The simulated bench does not move: its projects have no `AGENTS.md`, and no workload
  restates an answer.
- A live run of the owner's task on `claude:sonnet` shows the effect on a real model (the
  owner's OK, `docs/BENCH.md`).

## Check
- `crates/ark-runtime/tests/restated_answer.rs`: the owner's shape ends at the second
  restatement; long notes beside reads of new material, a change in between and a different
  answer do not.
- `rules::tests`: sections and the budget, sections chosen by the task, the list of sections
  left out bounded, a fenced `#`, nested files, hidden and build folders, links, a path as a
  name, a huge file.
- `ark-interact`: `rules_name_the_file_they_come_from`.
- `ark-sdk`: `a_turn_follows_the_project_folders_agents_file` (on and off, the context report).
