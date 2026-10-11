# 0062. Event automations use durable events and run-scoped authority

Status: Proposed
Date: 2026-10-10

## Context

Scheduled work (ADR 0053) starts tasks at times. It cannot express “when Lara emails, reply using
`budget.csv`” or the same job for WhatsApp. Provider webhooks and mailbox polling can repeat,
arrive late and resume after a crash. Message content is untrusted. A project-wide “Always” grant
is too broad for an unattended reply.

ARK must run this on one NAS with SQLite and no broker, while leaving a path to more feeds and
workers. Mail and WhatsApp must be providers of the same primitive, not two orchestration paths.

## Decision

Existing `ScheduleRecord` JSON, ids, APIs and behavior stay unchanged. `EventAutomationRecord`
adds event starts. `ark-interact::AutomationView` is the person-visible union over both. Provider
code authenticates the transport and normalizes an `ExternalEvent`, then calls the orchestrator's
durable `EventSink`. Matching is deterministic over trusted scalar fields. A model never decides
whether an event matches. An authenticated provider event carries an actor claim, not proof of a
human identity.

The released external-client protocol v1.1 stays unchanged. Existing schedule project resource
calls keep their names, parameters and meaning. Future native automation snapshots, resumable
events and ordered mutations are additive capabilities under ADR 0057. They reuse its sequence,
resync, mutation epoch, counter, digest and receipt rules and update schema, fixtures and generated
models together. Missing capabilities leave old clients fully usable.

Provider-neutral event values and source, sink and webhook contracts live in `ark-tools`.
`ark-connect` implements account sources and webhook verification. `ark-orchestrate` implements
the sink, matching and records. `ark-interact::App` wires them, so providers and orchestration do
not depend on each other. `ark-ui` owns the generic hardened webhook route.

The source gives each semantic event an opaque stable key. Re-delivery keeps the key; a distinct
transition gets another. A run id is stable per automation and event. Saving, matching and
submission may repeat after a crash, but one event starts one
task. Cursors advance only after events are durable. Runs use the existing task, retry, attention,
approval and report-to-chat paths. A0 enforces one writer runtime process per store while clients
keep using its protocol; multi-process claims wait for an atomic store primitive.

Creating, changing or resuming always asks. It creates a mandatory `AutomationAuthority`, copied
from trusted orchestration state to every event task. The runtime checks it before every action,
including reads and before the ordinary gate. Broader grants and `--yes` cannot widen it. It
restricts actions, exact or event-bound inputs, project paths, mutation mode and durable quotas.
Event content cannot create or widen an automation, connection, tool set or authority. Provider
sends follow ADR 0056: an uncertain effect is not retried as if it failed.

The first Mail feed polls Gmail history from an outbound connection, so a NAS needs no public
port. The first WhatsApp feed is the official Business Platform webhook through a public HTTPS
route the person supplies. Personal WhatsApp and screen scraping are excluded. Full design,
states, scale path and delivery gates: [`../AUTOMATIONS.md`](../AUTOMATIONS.md).

## Consequences

- One product view covers schedules and external events without migrating shipped schedule data.
  Providers never own task execution.
- SQLite typed records, deterministic ids and recovery scans are enough first. No broker, Redis,
  external scheduler or global Cloud event bus is required.
- A source can use polling, pull delivery, a direct webhook or a later relay without changing the
  event automation record.
- Run-scoped authority adds work to the runtime, but avoids giving every agent permission to read
  every file or send to every recipient.
- Exactly-once external effects are not promised. One run per event and truthful uncertainty are.
- Gmail read access needs restricted-scope review. WhatsApp initially needs a Business account
  and public HTTPS ingress.

## Check

The first implementation follows A0 to A4 in `docs/AUTOMATIONS.md`. Required checks include
duplicate and out-of-order delivery, activation cutovers, cursor expiry, every crash boundary,
authority escape attempts, prompt injection, reply loops, overflow, disconnect and disk full.
Mail and WhatsApp fixtures must pass through the same event and automation APIs.
