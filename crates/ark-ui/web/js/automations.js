import { h, icon } from "./dom.js";
import { canvasScene, canvasTools } from "./canvas-tools.js";
import { registerAutomationEditor, editor } from "./automation-editor.js";

const places = new Map();
const place = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;

function library(el, _params, api) {
  el.classList.add("sv", "automations-library");
  const saved = places.get(place(api)) || { scroll: 0 };
  places.set(place(api), saved);
  let alive = true, generation = 0, entries = saved.entries || [], legacy = saved.legacy || [], scheduled = saved.scheduled || [];
  const status = h("p.work-status", { role: "status" });
  const body = h("div.sv-body");
  const notice = h("p.canvas-notice", { role: "status" });
  const scene = canvasScene(`overview:${place(api)}`, message => { notice.textContent = message; });
  const viewport = h("div.auto-canvas-viewport", { tabindex: "0", "data-destination-heading": true, "aria-label": "Automations canvas" });
  const plane = h("div.auto-canvas-plane");
  const toolbar = h("div.auto-canvas-tools");
  let zoom = 1, inline = null, anchor = null;
  const shell = h("div.auto-overview", viewport, toolbar, notice);
  plane.append(body); viewport.append(plane); el.append(status, shell);
  el.setAttribute("aria-label", "Automations");
  function closeEditor() {
    if (!inline) return;
    if (inline.draft) inline.draft.name = inline.controller.summary().name;
    inline.controller.dispose(); inline.element.remove(); inline = null; shell.hidden = false;
    const target = [...body.querySelectorAll("[data-automation]")].find(control => control.dataset.automation === anchor);
    (target || add).focus({ preventScroll: true }); void load();
  }
  function openEditor(params, point, pendingDraft = null) {
    if (inline) return;
    anchor = document.activeElement?.dataset.automation || null;
    const element = h("section.auto-inline-editor", { "aria-label": "Automation configuration" });
    shell.hidden = true; el.append(element);
    const bound = { ...api, openView(id, next) {
      if (id === "schedules") closeEditor();
      else api.openView(id, next);
    }, async call(method, input) {
      const result = await api.call(method, input);
      if (method === "automation_save" && point) {
        const receipt = result.automation ? result : result.saved;
        if (receipt?.automation) {
          scene.data.positions[receipt.automation] = { ...(pendingDraft?.point || point) };
          if (pendingDraft) delete scene.data.positions[`draft:${pendingDraft.key}`];
          saved.drafts = (saved.drafts || []).filter(draft => draft !== pendingDraft); scene.save();
        }
      }
      return result;
    } };
    const controller = editor(element, { ...params, selected: "@start" }, bound);
    inline = { element, controller, draft: pendingDraft };
    controller.focus();
  }
  const create = point => {
    const draft = { key: crypto.randomUUID(), name: "", point };
    (saved.drafts ||= []).push(draft);
    openEditor({ draft_key: draft.key }, point, draft);
  };
  const add = h("button.btn", { type: "button", onclick: () => create({ x: (viewport.scrollLeft + 80) / zoom, y: (viewport.scrollTop + 80) / zoom }) }, icon("plus"), "Add automation");
  const percent = h("span.canvas-zoom", "100%");
  const changeZoom = delta => {
    zoom = Math.min(1.5, Math.max(.3, zoom + delta));
    body.parentElement.style.transform = `scale(${zoom})`; percent.textContent = `${Math.round(zoom * 100)}%`;
  };
  toolbar.append(h("button.auto-icon", { type: "button", "aria-label": "Zoom out", onclick: () => changeZoom(-.1) }, "−"), percent,
    h("button.auto-icon", { type: "button", "aria-label": "Zoom in", onclick: () => changeZoom(.1) }, icon("plus")), add);
  canvasTools({ viewport, plane, scene, toolbar, scale: () => zoom, create });
  const keydown = event => {
    if (event.key === "Escape" && inline && !event.target.closest("dialog,.auto-more[open]")) { event.preventDefault(); closeEditor(); }
  };
  el.addEventListener("keydown", keydown);
  function draw() {
    const focus = body.contains(document.activeElement) ? document.activeElement.dataset.automation : null;
    const scroll = el.scrollTop;
    body.replaceChildren();
    if (!entries.length && !legacy.length && !saved.loaded) body.append(h("div.sv-loading", "Loading automations…"));
    const list = h("ul.auto-gallery"); body.append(list);
    for (const entry of entries) {
      const head = entry.definition, draft = entry.revision.draft, active = entry.active?.draft, diverged = head.active_revision != null && head.active_revision !== head.revision;
      const prior = scheduled.find(schedule => schedule.id === head.schedule);
      const state = head.enabled ? entry.webhook_availability === "local" ? `Local events enabled · revision ${head.active_revision}` : prior && (active ? active.trigger.kind === "schedule" : !diverged && draft.trigger.kind === "schedule") && prior.next_ms == null ? `Active revision ${head.active_revision} · No future starts` : `Active revision ${head.active_revision}` : !head.active_revision && prior && !prior.target ? prior.paused ? "Existing schedule paused" : "Existing schedule active" : head.active_revision ? "Paused" : "Draft";
      const describe = value => `${value.trigger.kind === "schedule" ? "Scheduled" : value.trigger.kind === "webhook" ? "Webhook" : "Manual"} · ${value.steps.length} steps`;
      const summary = diverged ? `Draft revision ${head.revision}: ${describe(draft)}` : describe(draft);
      const published = diverged ? `${head.enabled ? "Active" : "Paused"} revision ${head.active_revision}${active ? `: ${describe(active)}${active.name !== draft.name ? ` · ${active.name || "Untitled automation"}` : ""}` : ""}` : null;
      list.append(h("li.auto-gallery-item", h("button.auto-gallery-card", { type: "button", dataset: { automation: head.id }, onclick: () => openEditor({ automation: head.id }) }, h("div.auto-gallery-diagram", { "aria-hidden": "true" }, h("span.auto-mini-node", "Start"), draft.steps.map(step => h("span.auto-mini-node", { dataset: { kind: step.action?.kind || "agent" } }, step.name || { agent: "Agent", approval: "Approval", result: "Result" }[step.action?.kind] || "Step"))), h("span.sv-main", h("span.sv-objective", draft.name || "Untitled automation"), h("span.sv-meta", summary), published ? h("span.sv-meta", published) : null), h("span.sv-state", state))));
    }
    for (const schedule of legacy.filter(schedule => !entries.some(entry => entry.definition.schedule === schedule.id))) {
      const counts = [schedule.missed ? `${schedule.missed} missed` : "", schedule.skipped ? `${schedule.skipped} skipped` : ""].filter(Boolean).join(" · ");
      const details = [schedule.says, schedule.admission_error || schedule.status, schedule.last, counts].filter(Boolean);
      list.append(h("li.auto-gallery-item", h("button.auto-gallery-card", { type: "button", dataset: { state: schedule.state, automation: `legacy:${schedule.id}` }, "aria-label": [schedule.objective, ...details].join(" · "), onclick: () => openEditor({ legacy_schedule: schedule.id }) },
        h("div.auto-gallery-diagram", { "aria-hidden": "true" }, ["Schedule", "Agent", "Result"].map(name => h("span.auto-mini-node", name))),
        h("span.sv-main", h("span.sv-objective", schedule.objective), h("span.sv-meta", schedule.says), schedule.last ? h("span.sv-last", schedule.last) : null, counts ? h("span.sv-count", counts) : null), h("span.sv-state", schedule.admission_error || schedule.status))));
    }
    for (const draft of saved.drafts || []) {
      const id = `draft:${draft.key}`;
      scene.data.positions[id] = draft.point;
      list.append(h("li.auto-gallery-item", h("button.auto-gallery-card", { type: "button", dataset: { automation: id }, onclick: () => openEditor({ draft_key: draft.key }, scene.data.positions[id], draft) },
        h("span.sv-objective", draft.name || "Untitled automation"), h("span.sv-state", "Draft"))));
    }
    for (const [index, item] of [...list.children].entries()) {
      const card = item.querySelector("button");
      const id = card.dataset.automation;
      const position = scene.data.positions[id] || { x: 64 + index % 3 * 360, y: 64 + Math.floor(index / 3) * 280 };
      Object.assign(item.style, { left: `${position.x}px`, top: `${position.y}px` });
      let drag = null, moved = false;
      card.addEventListener("pointerdown", event => {
        if (event.button !== 0 || !event.isPrimary || viewport.dataset.tool !== "select") return;
        drag = { id: event.pointerId, x: event.clientX, y: event.clientY, position: { ...(scene.data.positions[id] || position) } }; moved = false;
        card.setPointerCapture(event.pointerId);
      });
      card.addEventListener("pointermove", event => {
        if (!drag || event.pointerId !== drag.id) return;
        const dx = (event.clientX - drag.x) / zoom, dy = (event.clientY - drag.y) / zoom;
        if (Math.abs(dx) + Math.abs(dy) < 4 && !moved) return;
        moved = true;
        const next = { x: Math.max(0, Math.min(5000, drag.position.x + dx)), y: Math.max(0, Math.min(5000, drag.position.y + dy)) };
        scene.data.positions[id] = next;
        Object.assign(item.style, { left: `${next.x}px`, top: `${next.y}px` });
      });
      card.addEventListener("pointerup", event => {
        if (!drag || event.pointerId !== drag.id) return;
        drag = null;
        if (card.hasPointerCapture(event.pointerId)) card.releasePointerCapture(event.pointerId);
        if (moved) { const draft = saved.drafts?.find(draft => `draft:${draft.key}` === id); if (draft) draft.point = scene.data.positions[id]; scene.save(); }
      });
      card.addEventListener("pointercancel", () => { drag = null; moved = false; Object.assign(item.style, { left: `${position.x}px`, top: `${position.y}px` }); scene.data.positions[id] = position; });
      card.addEventListener("click", event => { if (moved) { moved = false; event.stopImmediatePropagation(); event.preventDefault(); } }, true);
      card.addEventListener("keydown", event => {
        const delta = { ArrowLeft: [-10, 0], ArrowRight: [10, 0], ArrowUp: [0, -10], ArrowDown: [0, 10] }[event.key];
        if (!event.altKey || !delta) return;
        event.preventDefault();
        const current = scene.data.positions[id] || position;
        scene.data.positions[id] = { x: Math.max(0, Math.min(5000, current.x + delta[0])), y: Math.max(0, Math.min(5000, current.y + delta[1])) };
        const draft = saved.drafts?.find(draft => `draft:${draft.key}` === id);
        if (draft) draft.point = scene.data.positions[id];
        scene.save(); draw();
      });
    }
    const positions = Object.values(scene.data.positions);
    plane.style.width = `${Math.max(2000, ...positions.map(p => p.x + 400))}px`;
    plane.style.height = `${Math.max(1400, ...positions.map(p => p.y + 320), 64 + Math.ceil(list.children.length / 3) * 280)}px`;
    if (focus) [...body.querySelectorAll("button[data-automation]")].find(button => button.dataset.automation === focus)?.focus({ preventScroll: true });
    el.scrollTop = scroll;
  }
  async function load() {
    const own = ++generation; status.textContent = "Loading…";
    try {
      const [first, schedules] = await Promise.all([api.call("automations", { cursor: null, limit: 50 }), api.call("schedules", {})]);
      const collected = [...first.entries];
      let after = first.next;
      while (after) {
        const page = await api.call("automations", { cursor: after, limit: 50 });
        if (!alive || own !== generation) return;
        collected.push(...page.entries); after = page.next;
      }
      if (!alive || own !== generation) return;
      entries = collected;
      scheduled = schedules; legacy = schedules.filter(schedule => !schedule.target);
      Object.assign(saved, { entries, legacy, scheduled, loaded: true });
      status.textContent = ""; draw();
    } catch (error) { if (alive && own === generation) {
      status.replaceChildren(document.createTextNode(error.message), h("button.btn", { type: "button", onclick: () => void load() }, "Refresh"));
      if (!saved.loaded) body.replaceChildren(h("div.sv-empty", h("h2", "Automations unavailable")));
    } }
  }
  let timer;
  const off = api.on("ark:orch", () => { clearTimeout(timer); timer = setTimeout(() => void load(), 250); });
  draw(); el.scrollTop = saved.scroll; void load();
  return { refresh: () => load(), dispose() { alive = false; generation++; saved.scroll = el.scrollTop; clearTimeout(timer); off?.(); el.removeEventListener("keydown", keydown); if (inline) { inline.controller.dispose(); inline = null; } } };
}

export function registerAutomations(api) {
  api.view({ id: "schedules", title: "Automations", icon: "clock", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => library(el, params, scoped || api) });
  registerAutomationEditor(api);
}
