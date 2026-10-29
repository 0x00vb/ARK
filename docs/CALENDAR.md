# Calendar snapshots

A project file or stored artifact named `*.calendar.json` opens as a calendar. Month, Week and
Day show the same items. Select an item for its delivery, state, account, next action, draft,
receipt and source revision. Previous, Next and Today change the period. Narrow month views
use an agenda. A crowded month shows three items per date and a button for the full day.

Project files retain Source for the existing editor. File version updates refresh the view;
a removed item closes its details. Invalid input reports the error and leaves Source available.
Stored artifacts retain Raw and use the scoped artifact reader. Identity, name, size, offset,
complete length, hex and UTF-8 are checked before rendering. Closing ignores a late reply.

## Version 1

```json
{
  "type": "ark.calendar",
  "version": 1,
  "title": "Project calendar",
  "timezone": "America/Argentina/Buenos_Aires",
  "generated_at": "2026-10-29T12:00:00Z",
  "events": [
    {
      "id": "launch-draft-20261030",
      "title": "Review launch draft",
      "start": "2026-10-30T10:00:00-03:00",
      "end": "2026-10-30T10:30:00-03:00",
      "all_day": false,
      "state": "draft",
      "delivery": "manual_reminder",
      "account": "Example account",
      "next_action": "You review the draft and publish at10:00",
      "draft": "drafts/launch.md",
      "source_record": "post-17",
      "source_revision": "r3"
    },
    {
      "id": "inventory-20261030",
      "title": "Inventory review",
      "start": "2026-10-30",
      "end": "2026-10-31",
      "all_day": true,
      "state": "planned",
      "delivery": "agent_run"
    }
  ]
}
```

| Field | Contract |
|---|---|
| `type`, `version` | `ark.calendar`, `1` |
| `title` | Nonempty, at most240 characters |
| `timezone` | Valid IANA zone used for the calendar's dates and clock labels |
| `generated_at` | RFC3339 instant with seconds and explicit offset, optional1to3 fraction digits |
| `events` | At most1,000 items; whole UTF-8 document at most1MiB |
| `id` | Stable unique string, at most128 characters |
| `start`, `end` | Explicit-offset instants for timed items; exact `YYYY-MM-DD` for all-day items |
| `all_day` | Required boolean; all-day end is exclusive |
| Range | End after start, at most366days; dates from1970 through9998 |
| `state` | `draft`, `planned`, `paused`, `running`, `needs_you`, `completed`, `failed`, `cancelled`, `unknown`, `published` |
| `delivery` | `deadline`, `manual_reminder`, `agent_run`, `provider_schedule` |
| Optional text | `account`, `draft`, `receipt`, `source_record`, `source_revision`: at most512characters; `next_action`: at most2,000 |

Floating local times are refused. A producer must resolve a daylight-saving gap or fold with
its owning scheduler before emitting an instant. Both occurrences of a repeated clock time
remain distinct. All-day dates stay dates; they are not shifted through the viewer's zone.
Every displayed state is a snapshot claim. `published` displays Reported published. The view
does not verify a provider receipt or treat a reminder firing as publication.

## Authority

Opening, navigating and selecting create no schedule, agent run, network request to a social
provider, account grant or post. Draft and receipt references are plain text, not arbitrary
links or actions. The owning capability keeps records, edits, revisions and delivery authority.
Recurring occurrences must be supplied by that owner with stable ids; this view does not expand
cron or create a second scheduler. Use the existing ARK scheduler and approvals for real runs
or requested reminders. A stale snapshot must be regenerated from those records.

CSV or spreadsheets can be separate exports. Editing a calendar Source file changes its
snapshot, and never silently creates standing work.
