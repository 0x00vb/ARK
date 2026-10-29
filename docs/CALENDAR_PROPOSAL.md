29Oct implementation update: the generic file/artifact lens is now a local candidate described in CALENDAR.md. Scheduler projections and real provider delivery remain separate. No actual jobs/posts created.

# Reusable calendar view

Status: proposed,2026-10-29. Board calendar-artifact-schedules-20261029.

Use a calendar with month/week/day views and per-item details. A spreadsheet can be an export,
but must not be the source of execution state. Changing a cell must not silently schedule work.

ARK already supports cron/date schedules, IANA zones, pause/resume, missed runs and restart
deduplication (ADR0053). Reuse that scheduler, not OS crontab. The owner's request proposes
changing its older no-calendar interface decision while retaining its authority/recovery rules.

Start with JSON artifacts and the existing viewer seam before adding a new core ArtifactKind.
Events carry stable id, source record/revision, title, start/end, IANA zone, all-day flag, state,
draft/result references and delivery kind. Delivery is a deadline, manual reminder, ARK run or
provider-accepted schedule. The owning capability keeps editing and execution authority.

Month shows the overview, week/day reveal timing. Selecting an event opens its draft, account,
authorization, next action and actual receipt. Keep Today/previous/next, a visible timezone,
keyboard controls and a narrow agenda view. Stale snapshots cannot claim current execution state.

With unavailable or paid API access, prepare a draft and state who must publish it and when.
If the person requests a reminder, use the existing schedule authority. Firing a reminder or
opening a composer does not mean Published. A scheduled agent run and a verified publication
are distinct outcomes. Reconcile Unknown sends before retrying.

First acceptance: a post draft, manual reminder and unrelated recurring project task render
month/week/day and details without duplicating records. Cover DST folds/gaps, all-day dates,
long titles, stale responses, cancellation and project/computer scope. Rendering creates no
standing work or publication authority.
