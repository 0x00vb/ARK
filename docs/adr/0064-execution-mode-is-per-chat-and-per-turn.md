# 0064. Execution mode is per chat and frozen for a turn

Status: Accepted. Amended by 0104 (the composer control is gone; the mode changes by words).
Date: 2026-10-11

## Context

The composer has one approval behavior today. State-changing actions ask unless a session or
project grant already covers their approval class. People also need to ask for a plan that cannot
change anything, or deliberately let routine project work continue without repeated questions.

A front-end-only mode would be false. Background chats, scheduled work, another client and a
restored turn all use the runtime's approval gate. The runtime must own the authority used by a
turn.

`Mutate` is also too broad for Auto. It covers project files and contained code, but also browser
input, native app control, connected services and unknown extension tools. Auto must not turn one
composer choice into silent external authority.

## Decision

Add three execution modes:

| Mode | Runtime behavior |
|---|---|
| Plan | The catalog omits state-changing actions. The turn may inspect and answer with a plan. |
| Manual | Current behavior. State-changing actions use session and project grants, then ask. |
| Auto | Actions explicitly marked as routine and project-bound run without asking. Every other state-changing action follows Manual. |

Manual is the default for existing and new chats. The selected mode belongs to a chat and has a
revision. Submit carries the mode and revision shown by its initiating composer. The runtime
atomically compares them with the chat, binds them to immutable turn authority and returns the
effective mode with the turn. A mismatch refuses submission and refreshes the control instead of
silently broadening or narrowing authority. Changing the control while a turn runs affects the
next turn only. Continued and crash-restored work keeps the mode with which that turn started.

Add an Auto eligibility field to action descriptors. Its default is false. ARK can opt in a tool
only when its implementation constrains effects to the selected project or a contained runtime.
Extension and MCP tools therefore keep asking until their owner classifies them. `ask_each_time`,
cut-short recovery and run-scoped automation authority always take precedence over Auto.

The first Auto allowlist is deliberately small:

| Action family | Auto |
|---|---|
| ARK Files writes in the selected project | Eligible |
| Contained Run code in the selected project | Eligible only with enforced filesystem, network, credential and native-app confinement |
| Project process start | Manual until it has the same enforced confinement |
| Stop a process ARK started for this project | Eligible after exact process ownership and identity checks |
| Browser input and navigation | Manual |
| Native Computer actions | Manual |
| Mail, Calendar and other connected services | Manual |
| Agent delegation, schedule or automation authority | Manual |
| Unknown extension or MCP mutation | Manual |

Plan removes unavailable actions from the model's catalog instead of letting the model call them
and returning repeated permission errors. The turn instruction states that the requested output
is a plan and that no state may change.

The composer shows one compact mode button beside the model choice. Its menu gives each option a
short name and behavior. The closed control shows only `Plan`, `Manual` or `Auto`. Mode colour is
not the only signifier. Switching uses the same short opacity and scale transition as the model
picker, is interruptible and becomes an instant state change under reduced motion.

## Consequences

- A mode is not a global app preference. Two chats can use different modes safely.
- A stale composer cannot submit with a mode another client changed. It refreshes and asks the
  person to send again.
- A running turn never gains broader authority because the person changed the next turn's mode.
- Auto removes prompts only for actions whose implementation opted into the project boundary.
- Existing grants remain stored but Plan ignores them and Auto does not create new grants.
- Scheduled and event-driven work keeps its explicit saved authority. It does not inherit the
  last composer mode.
- The protocol and session record gain an execution mode before Desktop renders the control.

## Check

- Plan cannot write a file, start a process, use Browser input or invoke a mutating extension,
  even with an existing project grant.
- Manual retains every current approval and grant test unchanged.
- Auto writes a project file and runs contained code without a question, but Auto does not
  authorize Browser, Computer, connected-service or unknown MCP mutations. They use existing
  grants and approvals.
- A process with only a project working directory is not contained and still uses existing grants
  and approvals. Auto cannot start it.
- Changing Auto to Plan during a running Auto turn does not narrow that turn or broaden the next
  one. The next submitted turn is Plan.
- Two clients select different modes. The first stale submit is refused; after refresh, a submit
  records and returns the exact effective mode. Crash recovery, Continue and a background chat use
  the mode saved with the turn and cannot substitute local control state.
- Keyboard-only use, compact width, screen readers and reduced motion can select and identify all
  three modes.
