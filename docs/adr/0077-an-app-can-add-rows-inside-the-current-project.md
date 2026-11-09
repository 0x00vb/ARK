# 0077. An app can add rows inside the current project

Date: 2026-10-17. Status: Superseded by 0110 (2026-11-09).

## Context

ARK Desktop's `desktop-kept` branch keeps chosen agents at the foot of the sidebar. They
belong to the current project, but `ark.sidebar` only supplies rows above the project list.
Changing the project tree from a consumer would depend on ARK's internal DOM and cleanup.

## Decision

Add `ark.projectSection({ id, label, render(el, { project }, api) })`. Its rows live inside
the current unfolded project, before chats, including when that project has no visible chat.
The app supplies the accessible group label and its own contents, controls and styles.
ARK does not name agents or keep application state.

Registration requires a unique nonempty string ID, a nonempty string label and a render
function. It schedules a project-tree refresh through the existing extension change batch.
Render is synchronous. Its container is connected before the call. It may return
`{ dispose() }`, called once while still connected before the next successful current draw.
The app owns cancellation of any asynchronous work it starts, including failure cleanup
before render returns. A render failure is logged and removes that section's container.
A disposer failure is logged; cleanup of other sections continues.

## Consequences

- Folded, hidden and other projects have no mounts. A project switch or refresh replaces the
  current mounts. Applications release listeners and cancel stale work in `dispose`.
- A failed projects read retains the previous draw. Generation checks before state changes
  and after session reads prevent old responses from changing the newer project or mounts.
- The hook adds no visible heading, persistence, RPC operation, authority or domain concept.
- Desktop migration remains separate, after a released ARK tag and consumer repin. This
  change does not decide whether to ship Desktop's existing sidebar-foot placement.

## Check

`crates/ark-ui/tests/web/project-section.test.mjs` runs ten cases in WebKit. Nine import the
production components with deterministic projects and sessions. They cover validation, connected mount/disposal,
empty chats, folding, switching, hiding, overlapping responses, read failure, error isolation,
keyboard activation and light/dark/narrow CSS states. The fixture sets the compact/drawer
state explicitly. The tenth loads the original index, app.js and HTTP transport against
strict intercepted fixture RPCs. It checks same-tick registration batches one tree refresh,
an unrelated extension causes no redraw, connected fold/remount and chat navigation through
an RPC and summary event. It also opens the actual frontend drawer at 390 px. The negative
control without the app's project-tree notification fails at late registration with clean
startup. These are browser/frontend checks, not a Rust-host or native-window run.

The host page scenario `project-sections` checks late registration through `app.js` and folding
with the real host. It is run by `an_apps_project_section_registers_late_and_follows_the_project_fold`
in the existing page-enabled CI. That host test and full Core CI have not run on this change:
free disk is below the workspace 30 GiB build floor. No native window or Desktop migration
acceptance is claimed by the component checks.
