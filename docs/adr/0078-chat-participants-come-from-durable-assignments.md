# 0078. Chat participants come from durable assignments

Status: Accepted by the owner for release, 2026-10-23.

A chat list needs an identity without inferring people from message text or the project roster.

Use persisted orchestration task report_to and parent links. Read one project snapshot and propagate each verified chat identity down the assignment graph. Each task owner is an assigned participant in that chat. Include every reporting ancestor, even when a child reports into a separate agent conversation. Use an iterative queue with visited task/chat pairs, stable sorted names and same-project session IDs. Finished and removed agents remain historical participants. Missing links add no inferred identity. Invalid stored records return an error.

Do not write a second membership store: it would drift on retries and restart. Membership is eventually consistent presentation data, never authority or proof an agent contributed a message.

Native protocol1.5 adds optional ChatSummary.participants. Missing/null means unknown; an empty list is known empty. The minimum supported client version stays unchanged. Consumers render a neutral identity for unknown/empty, one face for one assigned agent, and at most three faces plus an overflow count for groups. Avatars use bundled assets, never server-provided image URLs.

Verification requires durable reopen, ancestry, cycles, deep chains, invalid records, project isolation, legacy decoding and real Desktop/iPhone rendering. Core full CI passed on 2026-10-23. Consumer native release gates remain separate.
