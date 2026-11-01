import { h, icon } from "./dom.js";
import { calendarView } from "./calendar.js";
import { registerAutomationEditor, automationExample } from "./automation-editor.js";

const places = new Map();
const place = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;

function library(el, _params, api) {
  el.classList.add("sv", "automations-library");
  const saved = places.get(place(api)) || { mode: "canvas", scroll: 0 };
  places.set(place(api), saved);
  let alive = true, generation = 0, agendaGeneration = 0, entries = saved.entries || [], legacy = saved.legacy || [], scheduled = saved.scheduled || [], cursor = saved.cursor || null, pages = saved.pages || 1, calendar, range;
  const status = h("p.work-status", { role: "status" });
  const body = h("div.sv-body");
  const agenda = h("div.sv-calendar", { hidden: saved.mode !== "agenda" });
  const more = h("button.btn", { type: "button", hidden: true, onclick: () => void load(cursor) }, "Load more");
  const modes = h("div.cal-modes", { role: "group", "aria-label": "Automations view" });
  for (const mode of ["canvas", "agenda"]) modes.append(h("button.btn.ghost.small", { type: "button", "aria-pressed": String(saved.mode === mode), onclick: () => {
    saved.mode = mode;
    for (const button of modes.children) button.setAttribute("aria-pressed", String(button.dataset.mode === mode));
    draw();
    if (mode === "agenda") void loadAgenda();
  }, dataset: { mode } }, mode === "canvas" ? "Canvas" : "Agenda"));
  const create = h("button.btn.primary", { type: "button", onclick: () => api.openView("automation-editor", { draft_key: crypto.randomUUID() }) }, icon("plus"), "New automation");
  el.append(h("header.sv-head", h("h1.sv-title", { tabindex: "-1", "data-destination-heading": true }, "Automations"), modes, create), status, body, agenda, more);
  function draw() {
    body.hidden = saved.mode !== "canvas"; agenda.hidden = saved.mode !== "agenda";
    more.hidden = saved.mode !== "canvas" || !cursor;
    const focus = body.contains(document.activeElement) ? document.activeElement.dataset.automation : null;
    const scroll = el.scrollTop;
    body.replaceChildren();
    if (!entries.length && !legacy.length && !saved.loaded) body.append(h("div.sv-loading", "Loading automations…"));
    if (!entries.length && !legacy.length && saved.loaded) body.append(h("div.sv-empty", h("h2", "No automations yet"), ...[["weekly", "Example: Weekly review"], ["approval", "Example: Review and approve"]].map(([kind, text]) => h("button.btn", { type: "button", onclick: () => api.openView("automation-editor", { draft_key: crypto.randomUUID(), draft: automationExample(kind) }) }, text))));
    const list = h("ul.auto-gallery"); body.append(list);
    for (const entry of entries) {
      const head = entry.definition, draft = entry.revision.draft, active = entry.active?.draft, diverged = head.active_revision != null && head.active_revision !== head.revision;
      const prior = scheduled.find(schedule => schedule.id === head.schedule);
      const state = head.enabled ? entry.webhook_availability === "local" ? `Local events enabled · revision ${head.active_revision}` : prior && (active ? active.trigger.kind === "schedule" : !diverged && draft.trigger.kind === "schedule") && prior.next_ms == null ? `Active revision ${head.active_revision} · No future starts` : `Active revision ${head.active_revision}` : !head.active_revision && prior && !prior.target ? prior.paused ? "Existing schedule paused" : "Existing schedule active" : head.active_revision ? "Paused" : "Draft";
      const describe = value => `${value.trigger.kind === "schedule" ? "Scheduled" : value.trigger.kind === "webhook" ? "Webhook" : "Manual"} · ${value.steps.length} steps`;
      const summary = diverged ? `Draft revision ${head.revision}: ${describe(draft)}` : describe(draft);
      const published = diverged ? `${head.enabled ? "Active" : "Paused"} revision ${head.active_revision}${active ? `: ${describe(active)}${active.name !== draft.name ? ` · ${active.name || "Untitled automation"}` : ""}` : ""}` : null;
      list.append(h("li.auto-gallery-item", h("button.auto-gallery-card", { type: "button", dataset: { automation: head.id }, onclick: () => api.openView("automation-editor", { automation: head.id }) }, h("div.auto-gallery-diagram", { "aria-hidden": "true" }, h("span.auto-mini-node", "Start"), draft.steps.map(step => h("span.auto-mini-node", { dataset: { kind: step.action?.kind || "agent" } }, step.name || { agent: "Agent", approval: "Approval", result: "Result" }[step.action?.kind] || "Step"))), h("span.sv-main", h("span.sv-objective", draft.name || "Untitled automation"), h("span.sv-meta", summary), published ? h("span.sv-meta", published) : null), h("span.sv-state", state))));
    }
    for (const schedule of legacy.filter(schedule => !entries.some(entry => entry.definition.schedule === schedule.id))) {
      const counts = [schedule.missed ? `${schedule.missed} missed` : "", schedule.skipped ? `${schedule.skipped} skipped` : ""].filter(Boolean).join(" · ");
      const details = [schedule.says, schedule.admission_error || schedule.status, schedule.last, counts].filter(Boolean);
      list.append(h("li.auto-gallery-item", h("button.auto-gallery-card", { type: "button", dataset: { state: schedule.state, automation: `legacy:${schedule.id}` }, "aria-label": [schedule.objective, ...details].join(" · "), onclick: () => api.openView("automation-editor", { legacy_schedule: schedule.id }) },
        h("div.auto-gallery-diagram", { "aria-hidden": "true" }, ["Schedule", "Agent", "Result"].map(name => h("span.auto-mini-node", name))),
        h("span.sv-main", h("span.sv-objective", schedule.objective), h("span.sv-meta", schedule.says), schedule.last ? h("span.sv-last", schedule.last) : null, counts ? h("span.sv-count", counts) : null), h("span.sv-state", schedule.admission_error || schedule.status))));
    }
    if (focus) [...body.querySelectorAll("button[data-automation]")].find(button => button.dataset.automation === focus)?.focus({ preventScroll: true });
    el.scrollTop = scroll;
    if (saved.mode === "agenda" && !calendar) {
      calendar = calendarView(agenda, { text: JSON.stringify({ type: "ark.calendar", version: 1, title: "Automations", timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC", generated_at: new Date().toISOString(), events: [] }) }, null, { range(first, end) {
        range = { from_ms: Date.parse(`${first}T00:00:00Z`) - 86400000, to_ms: Date.parse(`${end}T00:00:00Z`) + 86400000 };
        queueMicrotask(loadAgenda);
      }, async open(event) {
        try {
          if (event.id.startsWith("automation-ar-")) { const run = await api.call("automation_execution", { run: event.id.slice(11) }); api.openView("work-chat", { work: run.work }); }
          else if (event.id.startsWith("run-t-")) { const result = await api.call("task", { task: event.id.slice(4) }); if (result.task.item) api.openView("work-chat", { work: result.task.item }); else api.openPanel("schedule", { id: event.source_record }); }
          else { const entry = entries.find(entry => entry.definition.schedule === event.source_record || entry.definition.id === event.source_record); const target = entry?.definition.id || scheduled.find(schedule => schedule.id === event.source_record)?.target; if (target) api.openView("automation-editor", { automation: target }); else api.openView("automation-editor", { legacy_schedule: event.source_record }); }
        } catch (error) { if (alive) status.textContent = error.message; }
      } });
    }
  }
  async function loadAgenda() {
    if (!alive || saved.mode !== "agenda" || !range) return;
    const own = ++agendaGeneration;
    try {
      const result = await api.call("schedules", { calendar: { ...range, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC" } });
      if (alive && own === agendaGeneration && saved.mode === "agenda") { calendar.update({ text: JSON.stringify(result) }); status.textContent = ""; }
    } catch (error) { if (alive && own === agendaGeneration) status.textContent = error.message; }
  }
  async function load(next = null) {
    const own = ++generation; status.textContent = "Loading…";
    try {
      const pageCount = next ? 1 : pages;
      const [first, schedules] = await Promise.all([api.call("automations", { cursor: next, limit: 50 }), api.call("schedules", {})]);
      let collected = first.entries, after = first.next, loaded = 1;
      while (!next && after && loaded < pageCount) {
        const page = await api.call("automations", { cursor: after, limit: 50 });
        if (!alive || own !== generation) return;
        collected.push(...page.entries); after = page.next; loaded++;
      }
      if (!alive || own !== generation) return;
      entries = next ? [...entries, ...collected] : collected; pages = next ? pages + 1 : loaded;
      scheduled = schedules; legacy = schedules.filter(schedule => !schedule.target); cursor = after;
      Object.assign(saved, { entries, legacy, scheduled, cursor, pages, loaded: true });
      status.textContent = ""; draw();
      if (saved.mode === "agenda") void loadAgenda();
    } catch (error) { if (alive && own === generation) {
      status.replaceChildren(document.createTextNode(error.message), h("button.btn", { type: "button", onclick: () => void load() }, "Refresh"));
      if (!saved.loaded) body.replaceChildren(h("div.sv-empty", h("h2", "Automations unavailable")));
    } }
  }
  let timer;
  const off = api.on("ark:orch", () => { clearTimeout(timer); timer = setTimeout(() => void load(), 250); });
  draw(); el.scrollTop = saved.scroll; void load();
  return { refresh: () => load(), dispose() { alive = false; generation++; agendaGeneration++; saved.scroll = el.scrollTop; clearTimeout(timer); off?.(); calendar?.dispose(); } };
}

export function registerAutomations(api) {
  api.view({ id: "schedules", title: "Automations", icon: "clock", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => library(el, params, scoped || api) });
  registerAutomationEditor(api);
}
