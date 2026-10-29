# Social publishing capabilities proposal

Status: proposed, 2026-10-29. Board: social-publishing-pack-20261029.

## Product shape

One content workflow plans the campaign, maintains drafts and reviews outcomes. X, Instagram,
Facebook and WhatsApp appear as separate marketplace capabilities with accurate prerequisites.
They share content and scheduling records; they do not each create a separate app or scheduler.
Content Operations is a companion skill. Marketing and Sales supplies audience/offer work.

Start with research, draft variants, editorial calendar, assets, approval and manual publishing
handoff. That useful path needs no paid social API. A reminder is not a platform-scheduled post.
Automatic publishing is added only for a supported, authorized channel with a verified outcome.
No live posting, account connection, message, scheduled job, payment or public launch occurs here.

## Current primary evidence

| Channel | Verified current source | Consequence for this proposal |
|---|---|---|
| X | X automation rules explicitly prohibit non-API automation. Current official pricing is pay per use, not a fixed subscription. The pricing page lists create-post and URL-post costs separately. | Do not promise unattended website/computer posting as a workaround. Draft/manual handoff first; optional official integration or supported publishing provider with a quoted, approved cost cap. |
| Instagram | Meta's own Postman collection describes publishing for professional accounts and separate login configurations. | Verify the chosen account/login route, permissions and media flow before adding automatic publishing. Consumer-account support is not promised. |
| Facebook | Meta describes Business Suite as a shared Facebook/Instagram publishing surface. Current direct Pages developer docs could not be fetched (429); public help pages require login. | Prepare channel-specific draft/export and Business Suite handoff. Keep exact Pages scheduling permissions and limits unverified until primary docs/account access are available. |
| WhatsApp | Current Business Messaging Policy requires recipient opt-in. Platform-initiated conversations use approved templates; free-form replies are limited to the customer-service window. | Separate customer conversations from feed publishing. No cold-message growth bot, personal-account broadcast promise or invented Channel/Status API. Reuse existing WhatsApp Business work on the board. |

Sources checked 2026-10-29:
- https://help.x.com/en/rules-and-policies/x-automation (updated April2026)
- https://docs.x.com/x-api/getting-started/pricing
- https://www.postman.com/meta/instagram/documentation/6yqw8pt/instagram-api
- https://about.fb.com/news/2020/09/a-faster-and-easier-way-to-manage-your-business-on-facebook-and-instagram/ (historical product context, not current API specification)
- https://whatsappbusiness.com/policy/ (updated September23,2026)
- https://developers.facebook.com/docs/instagram-platform/content-publishing/ and https://developers.facebook.com/docs/pages-api/posts/ (fetches429, recorded as unavailable evidence)

The current X page lists a plain create-post request at $0.015 and one with a URL at $0.200.
For illustration,30 such posts would be $0.45 or $6 in those write fees alone. Reads, media,
provider fees and future rate changes are outside that example. This is a dated estimate, not
a purchase or promise; check the actual console quote before authorizing spend.

Computer usage is a channel-specific implementation choice. It is not automatically more
permitted, cheaper or more reliable than an API. Do not assume a visible button makes
unattended automation supported. Where permitted, a supervised flow can prepare work for the
person; account checks, consent, platform restrictions and uncertainty recovery still apply.
Do not bypass platform restrictions, challenges or access controls to provide automation.

## Shared durable record, proposed

| Field | Meaning |
|---|---|
| draft_id and revision | Stable content identity and exact revision |
| channel and destination_account_id | Exact account/page/person, not just a display name |
| body and asset digests | Exact approved publication payload |
| schedule | Original local time, IANA timezone and resolved UTC occurrence |
| authorization | Exact account, content revision, action, expiry and any spend cap |
| delivery | Manual handoff, provider schedule or own due job, explicitly distinguished |
| attempt | Durable operation/idempotency key and provider receipt |
| outcome | Draft, Awaiting approval, Scheduled, Sending, Published, Failed, Unknown, Cancelled |

Changing content, destination or schedule invalidates the relevant approval. Timezone changes,
DST gaps/overlaps, edits near due time and machine sleep must have explicit behavior. Start
with UTC plus a retained human timezone; ask only for genuinely ambiguous intended times.
Local scheduling discloses that the chosen computer must be available. A provider-accepted
schedule is reported separately from eventual publication.

Before effect, persist the exact attempt. After a timeout, inspect provider receipt/account
history using the attempt identity where supported. Unknown does not silently retry a send.
Retry only after determining the prior attempt did not publish or with a provider's proven
idempotency guarantee. Cancellation of a draft differs from deletion of an already public post.
For a first scheduled channel, test restart recovery, expired auth, revoked permission, rate
limit, duplicate clicks, uncertain timeout, cross-account isolation and stop during submission.

## Success and staged proof

The first learning measure is completion of one useful approved content batch, measured editing
time and proportion of drafts accepted. Follower totals and impressions do not establish sales.
Use the owner's real voice and evidence, not generated claims. No new telemetry is added here.

1. A synthetic brief produces a calendar and channel-specific drafts with sources, assets and
   one stated next action. All drafts remain local and clearly unpublished.
2. A manual handoff identifies the exact account and preserves the draft. Human verification,
   not an inferred button click, records whether a post was published.
3. One supported channel with a test account proves a single authorized publication and its
   receipt, then a single scheduled occurrence with duplicate prevention and restart recovery.
4. Add further channels only after their transport and account requirements are separately
   proven. Existing WhatsApp Business setup/event cards are dependencies, not duplicate tasks.

Package/skill installation does not authorize publishing, contact scraping, bulk outreach or ad
spend. Paid access, real-account authority and public content approval are concrete later gates.
The MVP does not require agreeing to an API purchase now.
