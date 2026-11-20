# 0130. An agent reaches the person's agents by the name they write

Status: Accepted. Date: 2026-11-20. Amends 0117 (who an agent may contact).

## Context

The owner's chat with a named agent (2026-11-20): "delegale el trabajo del magic link al agent
Engineer censu". The agent called `agents.message` three times, with `Engineer censu`,
`Censu-Engineer` and `Censu -Enginner`. Each was refused: "you may not message `...`". It told
the owner to enable the contact or give the exact name, and nothing started.

| Cause | Evidence |
|---|---|
| An agent reached only its contacts: `delegates_to`, its parent, agents it made, its groups. The owner's agents have `delegates_to: []`, and no window sets it | `orch_agent` records in the owner's store |
| The stored name was `censu---engineer`, made from the title "Censu - Engineer". Names matched only exactly | the same records |
| An agent that may create agents got `to` as free text with no list of names, so it guessed | `AgentTools::actions` |
| The refusal said "may not" for a name that did not exist, and named no agent | the three refusals |
| `agents.message` said "send a short message"; work goes through `agents.start` | the agent used `message` for a whole job |

## Decision

| Part | Decision |
|---|---|
| Reach | On the person's own chat turn (`TaskRecord::own_chat_turn`: typed in the agent's chat; not a group turn, Work item, submission, run, schedule or reply) an agent may `agents.start` and `agents.message` any live agent of the orchestrator. The person is asking; the person's own ARK chat could already hand work to any agent. Other work (delegated, started, scheduled, automation, group turns, Work items, `work.later` continuations) keeps the contact rules of 0117. `agents.delegate` still goes only to `delegates_to`; it gets the name matching below |
| Names | `agents.start`, `agents.message`, `agents.delegate` and the person's chat hand-off read the name as written: the exact name, in any case; else the name or title equal once case, spaces, punctuation and word order are ignored; else one typo in each six letters (none under six letters, never in a digit: "analyst 3" is not "analyst2"); else the one agent whose words hold all of the query's words. A name that matches two agents is not guessed, even when the sender reaches only one of them |
| No redirect | The name is matched among every live agent first, then checked for reach. A name that is another agent's is refused ("you may not reach `x`"), never turned into an agent the sender happens to reach |
| Errors | A name that matches nothing lists the agents it may reach as `name (Title)`; a tie lists the tied agents |
| Offer | `to` lists the agents it may reach with their titles. `agents.message` says to use `agents.start` to give work |

Approvals do not change. Work an agent starts runs as started work did (0123): it asks where it
asked before. An agent the person set to act alone (`allow_mutate`) may be started from the
person's turn too: the person asked for it in their own words, and set that agent to act alone.

## Rejected

| Option | Why not |
|---|---|
| Widen contacts for every task | a scheduled or delegated run nobody watches could steer any agent |
| A setting to pick each agent's contacts | the owner never set one; the request in chat is the authority |
| Typos on short names | "coo" became "ceo" in `groups.rs`; a message went to the wrong agent |

## Not covered

A `work.later` continuation is not the person's turn and keeps the contacts. A stored name that
exists but is not reached is named in the refusal.

## Measured

Deterministic: `names` unit tests and two orchestrator tests with the three names from the trace.
Live: not run. The harness (`board/evidence/agent-reach-20261120/delegate.py`) drives ARK Desktop
`--serve` with the owner's request and each name; the Codex account had hit its usage limit, so
the baseline runs failed before any call, and live Claude runs wait for the owner's OK.
