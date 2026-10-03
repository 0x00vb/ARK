# 0007. The person's chat creates agents and tasks; forms are not the way in

Status: Accepted
Date: 2026-10-03

## Context
In an app that runs several agents, the only way to start work was a form: pick an agent,
write an objective and a context, choose files, dependencies and limits. Creating an agent
was a longer form. The person had to know what a task is, which agent to pick and what to
put in a brief before anything ran. The chat itself could not delegate: `agents.delegate`
was given only to orchestrated agents.

## Decision
The person's conversation gets `Orchestrator::chat_tools` on every turn when the app
orchestrates: `agents.create` (a new agent from a name, a role and instructions),
`agents.delegate` (to any live agent) and `agents.wait`. Tools reach a turn through
`TurnOptions::tools`, offered directly. The person asks in words; the chat decides what
becomes an agent or a task and writes the brief. Front-ends show agents and tasks and let
the person change them; they do not need a form to create them.

## Consequences
- An orchestrating app's chat requests carry three more tool schemas. Apps without an
  orchestrator are unchanged.
- An agent the chat creates gets every app tool. Its state-changing actions still go to the
  approval gate unless the person turns on `allow_mutate` for it.
- A task the chat creates has `Origin::External`, like one the person submitted.
- Delegation by brief (0005) holds: the chat writes the brief, the agent never sees the chat.
- Whether the chat delegates well is model behavior: measure it before relying on it.

## Check
`crates/ark-interact/tests/interaction.rs`,
`the_chat_creates_agents_and_hands_them_tasks`: the chat creates an agent, hands it a task,
and the task is in the orchestrator's records.
