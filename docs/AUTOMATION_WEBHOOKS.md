# Sequence webhook transport

Implementation contract for ADR0094. Independent design review: conditional GO on 1 November 2026. Implementation and real ingress gates remain open.

## Records and ownership

| Data | Contract |
|---|---|
| Endpoint | One stable endpoint per project and definition. Stable generated account identity. It never accepts a payload-selected project, account or definition. |
| Connection | Configuration identity, base URL, local/verified/pending state and rate limit. The secret lives in the existing Secrets store. A setup request has a stable receipt. |
| Draft connection | Configuring a replacement does not change the active connection. Preview may use the draft credential. Delivery uses the active credential. |
| Active connection | Activation freezes its configuration in the grant. Switching it is atomic. The previous credential stops admitting deliveries. The endpoint/account/event identity remains stable through rotation, so a re-signed old event recovers its original run. |
| Run receipt | Endpoint + stable account + event ID, scoped to the definition and project. The same canonical inputs return the original revision, including through pause and publication. Different inputs under the same identity are refused. |
| Delivery log | Bounded metadata only: time, received/rejected/duplicate/admitted, reason and run reference. No raw body, credential or signature. |

Each activation stores an admission watermark on the endpoint. After recovering an existing receipt, new events must carry a signed timestamp strictly newer than this watermark. The final admission transaction rechecks it against the current connection. An event at the same millisecond is refused and needs a new delivery timestamp. Publication cannot run an old unadmitted event under the new revision.

The definition's draft connection and the activation's active connection are separate. Saving a revision or configuring a draft connection does not stop the active revision. Pause stops new admissions. Grant revocation stops further effects and step starts on already admitted runs. Credential rotation does not invent a new event namespace.

## Signed request

POST `/webhooks/v1/<endpoint>` with a body of at most 64KiB. The headers carry a connection ID, `preview` or `deliver` mode, decimal Unix milliseconds, a bounded stable event ID, and a base64url HMAC-SHA256 signature. Endpoint, connection, mode, timestamp and event ID contain no newline. The signed bytes are the fixed protocol version, those fields separated by newline, and the exact remaining body bytes. That encoding has one interpretation.

Check field grammar, timestamp window (five minutes) and constant-time MAC before JSON parsing. Reject repeated security headers. Delivery JSON has a closed schema containing typed inputs only. Account and routing identities come from the verified endpoint. Untrusted inputs never become authority configuration.

Preview uses a distinct signed mode and a closed challenge body. It never creates a Run or executes a model. The response signs the challenge, endpoint and connection with the same key and a distinct response prefix. An arbitrary echo server cannot establish that the configured runtime received the probe.

## Input preview and mapping

The owner can test the saved draft's named text and file inputs before delivery. Preview
checks the current revision, closed input types, required names, project-owned files and the
same64KiB delivery-body limit. It returns the exact `inputs` payload and the step bindings
that consume each named input. It does not create Run, Work, sessions, approvals or provider
calls. The sender maps its event fields into these declared names; arbitrary routing and
authority fields are rejected by the delivery schema. Connection verification remains a
separate signed challenge and is not an input preview.

## Availability and limits

No listener means Pending connection. The app does not create a tunnel or open a port. A Full listener may advertise its actual loopback port for local development. A supplied external base must use HTTPS on port443 without URL credentials, query, fragment or an unrelated path. Resolve and pin public destination addresses for the probe. Reject private, loopback, link-local and reserved destinations on the external path. Do not follow redirects.

A successful self-probe means Verified from this runtime. It does not prove that an Internet sender can reach it. Loopback stays Local. Activation requires a recent successful probe and shows the actual connection state.

The Full listener accepts at most 16 concurrent connections. Webhook ingress has a shared burst of 16 requests, refills at eight requests per second, and admits at most eight incomplete or executing requests. These limits apply before reading the body or accessing the store, including invalid signatures and unknown endpoints. Excess webhook requests receive 429 with Retry-After. Saturated listener connections receive 503. Dropping a connection releases its capacity. Event streams observe peer closure directly, so rapid browser reconnects do not retain permits until the next heartbeat.

New admission reserves the endpoint rate and store-wide rate in the same transaction as Run, WorkItem and conversations. Duplicate recovery does not consume another allowance. Bound pending runs and delivery logs. Quota read or write failure refuses admission. Dispatch starts only after commit.

## Required evidence

Changed body/mode/endpoint/connection, duplicate headers, expired timestamps, wrong account, unknown fields, oversized body, failed probe, redirect/private destination, simultaneous duplicate delivery, changed-input replay, rotation/cutover, paused replay, quota failure, recovery before dispatch, and an actual signed request through the running app. Unit fixtures do not establish reachability or real provider execution.
