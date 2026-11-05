# 0097: One chat for work and automation runs

Status: Accepted. Date: 2026-11-05. Revises the page parts of ADR 0094; its records are unchanged.

## Context

The owner found the Automations canvas and the Work ticket page hard to understand: cards with
execution inputs, sources, revisions and history, and a second chat page (Status, Assigned to,
Source, Updated, Usage) for each run. The second page duplicated the chat surface.

## Decision

| Part | Now |
|---|---|
| A run's conversation | Opens in the normal chat (`openSession`). The `work-chat` view is removed |
| Automations page | One compact card per automation: name, one plain-text box per step, when, On, Run. Typing saves. The canvas and the `automation-editor` view are removed |
| Card steps | Agent steps in order. Each receives the previous step's text and files; a final Result step returns the last one's. Unshown saved fields (inputs, authority, targets, models, webhook) are kept |
| Work page | The shared composer over a list grouped by what each item waits on. View all shows the board |

The runtime protocol, records, revisions, activation and webhook endpoints do not change.

## Consequences

- Consumers that opened `work-chat` open the receipt's conversation instead (Desktop's agent chat).
- Webhook setup, execution inputs, per-step agents and models have no page control. They can be
  set by an agent or the API and are preserved by the card. A later change can add them back as
  options of a card if people need them.
- The Studio selection hand-off that only the Work ticket page offered is gone; Studio docks the
  normal chat.
- Automations cannot be deleted from the page yet (no delete operation exists); pausing stops them.
