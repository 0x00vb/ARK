# 0013. Asking for a capability mid-task

Status: Accepted
Date: 2026-10-04

## Context
The product loop is: tell ARK what you want, ARK finds what it needs, you connect or approve
it simply, the work gets done (ROADMAP › Capabilities). When an agent needs Mail and Mail is
not connected, the person should not have to stop, find a settings page and start again.
ARK already had an acquisition seam (`find_capability`, `propose_capability`,
`acquire_capability`, gated by approval) that no app used; its acquire tool asked for an `id`
the runtime never read.

## Decision
The connections are the acquisition source (`impl AcquisitionProvider for Connections`). A
turn's first message lists what is one sign-in away (`== CAN BE ADDED ==`, counted with the
capability index), and the acquisition tools are offered only while something is. The agent
calls `acquire_capability {need}`. The approval gate asks; in ark-interact a `Connector`
approver words it in capability terms ("ARK needs this to finish: Connect Google so ARK can
send email from your account", answers Connect or Not now), and on Connect runs the sign-in in
the person's browser and waits for it. The acquisition hands back the capability's tools and
they join the running turn, offered directly. Each need is its own approval class.

## Consequences
- One question and one sign-in, then the task goes on; the send itself still asks as usual.
- A turn of an app with connections pays for the offer and three meta-tool definitions only
  while a capability is connectable and not connected.
- The runtime now changes a turn's tools mid-turn, but only through an acquisition the
  person approved.
- A front-end that cannot open a browser on the runtime's machine sets `App::sign_in`.
- Not benchmarked: no bench workload has an account to connect. The cost is visible in
  `ContextReport` (`capability_index`).

## Check
`crates/ark-interact/tests/interaction.rs`,
`an_agent_asks_for_mail_mid_task_and_sends_once_it_is_connected`; `crates/ark-ui/tests/page.rs`,
`an_agent_asks_to_connect_mail_and_the_turn_goes_on`; `crates/ark-connect/src/acquire.rs`
tests; `crates/ark-cli/src/approval.rs`, `a_request_to_connect_offers_connect_or_not_now`.
