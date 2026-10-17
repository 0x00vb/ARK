# 0045. A capability the person turns on, asked for mid-task

Status: Accepted (amends 0013 and 0018)
Date: 2026-10-07

## Context
A conversation in ARK Desktop (2026-10-07, in Spanish): "open WhatsApp and write 'Hola q tal?'
to Notas". The agent opened WhatsApp Web in the Browser, which stuck loading. The person said
they meant the app on their computer. The agent called `find_capability` and
`propose_capability`, then answered that it could not control native apps.

| Found | Where |
|---|---|
| ARK Desktop never turned the Computer on | `ark-desktop/src/main.rs`: `.web`, `.connections`, `.browser`, no `.computer` |
| Mid-task acquisition (0013) knew accounts and program installs only | `ark-sdk` `build`: the sources were `Connections` and `Installs` |
| `find_capability {need: "computer"}` answered "Computer is not connected" | `Connections::capability_for` resolved an id with no provider |
| Turning the Computer on in every turn adds its 10 action schemas to every turn | per-agent actions are core actions (`with_tools`) |

Turning it on in Desktop alone would make every turn pay for a capability few turns use, and the
next capability that is off would fail the same way.

## Decision
A capability an app offers can be off until the person turns it on. An agent that needs it asks
mid-task, as it asks to connect Mail (0013). The Computer is the first.

| Question | Decision |
|---|---|
| What is off | `ArkBuilder::computer(true)`: the native Computer with a switch in the store (`computer.json`), off until turned on. `computer_with(svc)` is as the service says (on without a switch) |
| What a turn sees while off | One offer line under `== CAN BE ADDED ==`: "computer: use the apps on this computer (open one, read its window, click and type in it). When the person names an app, use the app, not its website". No computer schema. `computer.open` says the same about websites |
| How an agent asks | `acquire_capability {need}`; a need that names the computer by whole words (`computer`, `app`, `apps`, `application`, `desktop`, `native`, `screen`, `computer.*`) goes to the Computer. The app's own sources (accounts, installs) are asked first, so "calendar app" is Calendar |
| Why a new seam | The Computer's tools are each owner's own (one screen lease per owner, 0018), and the harness does not know the owner. `PerAgent::acquisition(&Owner)` makes the source per owner; `ark-sdk` and `ark-orchestrate` put it after the app's sources in each turn's or task's profile |
| What the person sees | "ARK needs this to finish: Allow ARK to use the apps on this computer. It still asks before each step.", Allow or Not now (`ark-interact`). Allow waits for macOS's Accessibility permission (asking the system once), then turns it on and hands the owner's tools to the running turn. Without the permission it stays off and the agent is told how to allow it |
| No grant covers it | Every `acquire_capability` asks each time: no "for this session" or "always" answer covers a later request. A capability turned off is asked for again |
| Nobody unattended turns it on | `AcquisitionProvider::asks_a_person`: the Computer's request waits for a person also when the agent may change things without asking (`allow_mutate`) |
| An agent's allow-list | An orchestrated agent can ask only when its `tools` allow a `computer.*` action, and gets only the actions they allow |
| Turning it off | Plugins › On this computer (`Turn on`, `Turn off`, the list state `off`; Settings › Capabilities until ADR 0069 moved connections out of Settings), `/connect computer`, `/disconnect computer`. Off takes the screen back from whoever holds it, and every computer action refuses from then on, also in a turn that still holds the tools |
| Each action | Unchanged from 0018 and 0042: every action but `apps` asks each time |

## Consequences
- A turn with the Computer off pays one offer line, not ten schemas.
- One more question the first time: Allow, then the system's permission, then each step.
- A client of a runtime elsewhere (a phone through a one-time link, 0040) can answer Allow for
  the runtime's machine, as it can answer each action already. `writer --serve --tokens` keeps
  the Computer out entirely.
- The seam lets the Browser or Run code be on request later; nothing uses it for them yet.
- Not in the bench: no workload has a screen.

## Check
- `crates/ark-interact/tests/interaction.rs`,
  `an_agent_asks_to_use_the_apps_and_the_turn_goes_on_in_the_app` (the question and its answers,
  the tools joining the turn, the next turn on from its first step, asked again after Turn off
  although the first answer was "Always", Not now leaves it off, asked under `allow_mutate`);
  `a_need_reaches_the_source_that_serves_it`.
- `crates/ark-computer/tests/computer.rs`: off until turned on and remembered, off refusing a
  holder's next action, the permission asked when turned on, whole-word needs, no switch is on.
- `crates/ark-orchestrate/src/orchestrator.rs`, `an_agent_gets_only_the_acquired_actions_its_tools_allow`.
- `crates/ark-connect/src/acquire.rs`: `computer` is not an account capability.
