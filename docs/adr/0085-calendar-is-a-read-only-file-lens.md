# 0085. A calendar is a read-only file lens

Status: Local candidate under the owner's29Oct GO for the marketplace/calendar direction.
Date:2026-10-29.

Plugins need a visible post plan and other work needs the same month/week/day view. An Excel
workbook would make execution state ambiguous; a separate scheduler would duplicate authority.

Use `*.calendar.json` v1 through the existing file reader and artifact viewer. Validate once
at the boundary. Timed events are explicit instants, all-day ranges use exclusive dates,
references are text and every state carries the document's snapshot timestamp. Files follow
normal revisions; immutable artifacts use their scoped identity and complete bounded bytes.

This adds a calendar display to ADR0053's older no-calendar interface decision. It changes no
schedule type, approval, retry, missed-run or publication contract. Actual recurrence expansion
and execution stay with the owning scheduler or capability. No new ArtifactKind or provider
transport is needed. See docs/CALENDAR.md and calendar-data/calendar WebKit checks.
