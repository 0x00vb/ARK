// Automations: saved work that runs on demand, on a schedule or on an event. Each one is a card
// a person reads top to bottom: its name, what happens at each point in plain words, and when.
// Typing saves. Run starts it now and opens its conversation as a normal chat.

import { h, icon } from "./dom.js";

const places = new Map();
const place = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;
const uid = () => `step-${crypto.randomUUID()}`;
const zone = () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
const WEEKDAYS = ["mon", "tue", "wed", "thu", "fri"];
const POINTS = 7; // the runtime takes eight steps; the last one returns the result
const SAVE_MS = 700;
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

const blank = () => ({ name: "", trigger: { kind: "manual" }, inputs: [], authority: { actions: [], max_calls: 256, max_mutations: 32 }, steps: [
  { id: uid(), name: "Step 1", action: { kind: "agent", target: { kind: "general", model: null }, prompt: "", inputs: [] } },
] });
const points = draft => draft.steps.filter(step => step.action.kind !== "result");
const firstLine = text => (text || "").trim().split("\n")[0].replace(/[\u0000-\u001f\u007f]/g, " ").trim();
// By code point, so an emoji is never cut in half.
const clip = (text, length) => [...text].slice(0, length).join("");
const label = (text, index) => clip(firstLine(text), 60) || `Step ${index + 1}`;

function when(trigger) {
  if (trigger.kind === "manual") return "manual";
  if (trigger.kind === "webhook") return "event";
  const w = trigger.when || {};
  if (w.cron) return "cron";
  if (w.every_minutes === 60) return "hourly";
  if (w.every_minutes != null || w.date) return "other";
  if (!w.days?.length || w.days.length === 7) return "daily";
  if (w.days.length === 5 && WEEKDAYS.every(day => w.days.includes(day))) return "weekdays";
  return "other";
}

function describe(w) {
  if (w.every_minutes != null) return `Every ${w.every_minutes} minutes`;
  if (w.date) return `On ${w.date} at ${w.at}`;
  return `${w.days.map(day => day[0].toUpperCase() + day.slice(1)).join(", ")} at ${w.at}`;
}

/// The draft the runtime keeps: each point fed what the point before it produced, and a last step
/// that returns what the last point produced. Step names are set where the person types.
function normalize(draft) {
  const list = points(draft);
  const chain = id => [{ kind: "step", step: id, port: "text" }, { kind: "step", step: id, port: "files" }];
  list.forEach((step, index) => {
    const before = list[index - 1];
    const own = step.action.inputs.filter(binding => binding.kind !== "step");
    step.action.inputs = before?.action.kind === "agent" ? [...chain(before.id), ...own] : [...step.action.inputs.filter(binding => binding.kind === "step" && binding.step === before?.id), ...own];
  });
  const last = list.at(-1);
  let result = draft.steps.find(step => step.action.kind === "result");
  if (!result && list.length < POINTS + 1) result = { id: uid(), name: "Result", action: { kind: "result", inputs: [] } };
  if (result) result.action.inputs = last?.action.kind === "agent" ? chain(last.id) : [];
  draft.steps = result ? [...list, result] : list;
  return draft;
}

/// What gets saved: the card without its empty steps, named after its first step when unnamed.
/// Null while nothing is written.
function savable(source) {
  const draft = structuredClone(source);
  draft.steps = draft.steps.filter(step => step.action.kind !== "agent" || step.action.prompt.trim());
  const first = points(draft).find(step => step.action.kind === "agent");
  if (!first) return null;
  if (!draft.name.trim()) draft.name = clip(firstLine(first.action.prompt), 80) || "Untitled automation";
  return normalize(draft);
}

/// Whether a prepared activation grants nothing the active one did not: the same models and
/// agents, no new actions and the same event endpoint.
function covered(active, prepared) {
  if (!active) return false;
  const models = new Set(Object.values(active.selections || {}).map(value => JSON.stringify(value)));
  return Object.values(prepared.selections || {}).every(value => models.has(JSON.stringify(value)))
    && Object.entries(prepared.agents || {}).every(([name, spec]) => JSON.stringify(active.agents?.[name]) === JSON.stringify(spec))
    && Object.keys(prepared.descriptors || {}).every(name => name in (active.descriptors || {}))
    && JSON.stringify(prepared.webhook || null) === JSON.stringify(active.webhook || null);
}

function dialog(title, lines, confirm) {
  return new Promise(resolve => {
    const box = h("dialog.auto-dialog", h("h2", title), lines.map(line => h("p", line)),
      h("div.auto-dialog-actions", h("button.btn", { type: "button", onclick: () => box.close("cancel") }, "Cancel"), h("button.btn.primary", { type: "button", onclick: () => box.close("ok") }, confirm)));
    box.addEventListener("close", () => { box.remove(); resolve(box.returnValue === "ok"); });
    document.body.append(box); box.showModal();
  });
}

function library(el, _params, api) {
  el.classList.add("sv", "automations-library");
  el.setAttribute("aria-label", "Automations");
  const saved = places.get(place(api)) || { cards: [], legacy: [], loaded: false, scroll: 0 };
  places.set(place(api), saved);
  let alive = true, generation = 0, timer;
  const status = h("p.work-status", { role: "status" });
  const list = h("ul.auto-cards");
  const add = h("button.btn.auto-new", { type: "button", onclick: () => create() }, icon("plus"), "New automation");
  el.append(h("header.auto-lib-head", h("h1.sv-title", { tabindex: "-1", "data-destination-heading": true }, "Automations"), add), status, list);

  function create() {
    const card = fresh();
    saved.cards.unshift(card); draw();
    const node = list.querySelector(`[data-key="${card.key}"]`);
    if (node && !reduced()) node.animate([{ opacity: 0, transform: "translateY(-6px)" }, { opacity: 1, transform: "none" }], { duration: 200, easing: "cubic-bezier(0.23, 1, 0.32, 1)" });
    node?.querySelector("textarea")?.focus();
  }

  function fresh() {
    const draft = blank();
    return { key: crypto.randomUUID(), id: null, revision: null, draft, base: JSON.stringify(draft), head: null, next: "", error: "" };
  }
  const dirty = card => JSON.stringify(card.draft) !== card.base;
  const startable = card => !!card.id || !!savable(card.draft);
  function edit(card, change, repaint = false) {
    change(); normalize(card.draft); card.error = "";
    clearTimeout(card.timer); card.timer = setTimeout(() => void save(card), SAVE_MS);
    if (repaint) paint(card); else sync(card);
  }

  async function save(card) {
    clearTimeout(card.timer);
    if (card.saving) { card.again = true; return card.saving; }
    const snapshot = JSON.stringify(card.draft);
    if (snapshot === card.base) return;
    const draft = savable(card.draft);
    if (!draft) return;
    const text = JSON.stringify(draft);
    if (card.attempt?.text !== text) card.attempt = { request: crypto.randomUUID(), text };
    card.saving = (async () => {
      try {
        const receipt = await api.call("automation_save", { request: card.attempt.request, automation: card.id, expected: card.revision, draft, legacy: null });
        card.id = receipt.automation; card.revision = receipt.revision; card.base = snapshot; card.attempt = null; card.conflict = false;
      } catch (error) { card.error = error.message; card.conflict = !!card.id; }
      if (!card.error) try { await refresh(card); await follow(card); } catch (error) { card.error = error.message; }
      finally {
        card.saving = null;
        if (alive) sync(card);
        if (card.again) { card.again = false; void save(card); }
      }
    })();
    return card.saving;
  }

  /// Save what is typed, including keystrokes made while an earlier save was on its way.
  async function flush(card) {
    while (card.saving || dirty(card) && savable(card.draft)) {
      const before = card.base;
      await save(card);
      if (card.error || card.base === before && !card.saving) return;
    }
  }

  async function refresh(card) {
    const data = await api.call("automation_get", { automation: card.id });
    card.head = data.entry.definition; card.lastStart = data.entry.last_start; card.pending = data.entry.pending_start; card.activation = data.activation || null;
  }

  /// Keep an active automation running what was just saved. A manual one stops its schedule. A
  /// change that would grant more (another model or agent, new actions) waits for the person.
  async function follow(card) {
    card.apply = false;
    if (!card.head?.enabled || card.head.active_revision === card.revision) return;
    if (card.draft.trigger.kind === "manual") { await api.call("automation_pause", { automation: card.id }); return refresh(card); }
    card.apply = true;
    const prepared = await api.call("automation_prepare", { automation: card.id, expected: card.revision });
    if (!card.draft.authority.actions.length && covered(card.activation, prepared)) {
      await api.call("automation_activate", { automation: card.id, activation: prepared.id, digest: prepared.digest });
      card.apply = false;
      return refresh(card);
    }
  }

  async function preview(card) {
    if (card.draft.trigger.kind !== "schedule") { card.next = ""; return note(card); }
    const own = card.previewing = (card.previewing || 0) + 1;
    try {
      const draft = normalize(structuredClone(card.draft));
      draft.name ||= "Preview";
      for (const step of draft.steps) if (step.action.kind === "agent") step.action.prompt ||= "Preview";
      const next = (await api.call("automation_preview", { draft })).next[0];
      if (own === card.previewing) card.next = next ? `Next ${next.label}` : "No future runs";
    } catch (error) { if (own === card.previewing) card.next = error.message; }
    if (alive) note(card);
  }

  /// Turn on the saved revision. The person confirms what it may use unless it only runs general
  /// steps with no actions, or grants nothing the running revision did not.
  async function activate(card) {
    const prepared = await api.call("automation_prepare", { automation: card.id, expected: card.revision });
    const actions = card.draft.authority.actions;
    const agents = Object.keys(prepared.agents || {});
    const quiet = !actions.length && (!agents.length || covered(card.activation, prepared));
    if (!quiet) {
      const lines = [
        ...[...new Set(Object.values(prepared.selections || {}).map(value => value.agent?.title || value.agent?.name ? `${value.agent.title || value.agent.name} · ${value.model}` : value.model))],
        ...actions.map(action => prepared.descriptors?.[action.action]?.summary || action.action),
        ...(actions.length ? [actions.some(action => action.unattended) ? "Some changes happen without asking." : "Changes ask you first."] : []),
      ];
      if (!await dialog("Turn on", lines, "Turn on")) return;
    }
    await api.call("automation_activate", { automation: card.id, activation: prepared.id, digest: prepared.digest });
    card.apply = false;
    await refresh(card);
  }

  async function act(card, run) {
    if (card.busy) return;
    card.busy = true; card.error = ""; paint(card);
    try { await flush(card); if (dirty(card) && savable(card.draft)) return; card.error = ""; await run(); }
    catch (error) { card.error = error.message; }
    finally { card.busy = false; if (alive) paint(card); }
  }

  const toggle = card => act(card, async () => {
    if (!card.id) throw new Error("Write what it should do first.");
    if (card.head?.enabled && !card.apply) { await api.call("automation_pause", { automation: card.id }); await refresh(card); }
    else await activate(card);
  });

  const run = card => act(card, async () => {
    if (!card.id) throw new Error("Write what it should do first.");
    await refresh(card);
    if (card.pending) {
      const recovered = await api.call("automation_cancel_start", card.pending);
      card.pending = null;
      if (recovered.run) return void api.openSession(recovered.run.conversation);
    }
    const attempt = { previous: card.lastStart?.run || null, request: crypto.randomUUID(), automation: card.id, expected: card.revision, inputs: {} };
    const pending = await api.call("automation_begin_start", attempt);
    if (pending.request !== attempt.request) throw new Error("Another start is in progress. Try again in a moment.");
    const started = await api.call("automation_run", pending);
    card.lastStart = await api.call("automation_acknowledge_start", pending);
    if (alive) api.openSession(started.conversation);
  });

  function grow(area) { area.style.height = "auto"; area.style.height = `${area.scrollHeight}px`; }

  function pointRow(card, step, index, count) {
    const remove = count > 1 ? h("button.auto-icon.auto-point-remove", { type: "button", "aria-label": `Remove step ${index + 1}`, title: "Remove", onclick: () => edit(card, () => { card.draft.steps = card.draft.steps.filter(other => other !== step); }, true) }, icon("x")) : null;
    const number = count > 1 ? h("span.auto-point-n", { "aria-hidden": "true" }, String(index + 1)) : null;
    if (step.action.kind !== "agent") return h("li.auto-point", number, h("p.auto-point-fixed", step.action.kind === "approval" ? `Wait for your OK: ${step.action.title}` : step.name), remove);
    const area = h("textarea.auto-point-text", { rows: 1, "aria-label": count > 1 ? `Step ${index + 1}` : "What it does", placeholder: index ? "Then…" : "What should happen?", oninput: event => { grow(event.target); edit(card, () => { step.action.prompt = event.target.value; step.name = label(event.target.value, index); }); }, onblur: () => void save(card) });
    area.value = step.action.prompt;
    requestAnimationFrame(() => grow(area));
    return h("li.auto-point", number, area, remove);
  }

  function whenControls(card) {
    const trigger = card.draft.trigger, mode = when(trigger), w = trigger.when || {};
    const options = [["manual", "When I run it"], ["daily", "Every day"], ["weekdays", "Weekdays"], ["hourly", "Every hour"], ["cron", "Custom"]];
    if (mode === "event") card.event = trigger;
    if (card.event) options.push(["event", "On an event"]);
    if (mode === "other") options.push(["other", describe(w)]);
    const select = h("select.auto-when", { "aria-label": "When", onchange: event => edit(card, () => {
      const value = event.target.value, tz = w.tz || zone(), at = w.at || "09:00";
      card.draft.trigger = value === "event" ? structuredClone(card.event) : value === "manual" ? { kind: "manual" }
        : value === "hourly" ? { kind: "schedule", when: { every_minutes: 60, tz } }
        : value === "cron" ? { kind: "schedule", when: { cron: "0 9 * * *", tz } }
        : { kind: "schedule", when: { at, tz, days: value === "weekdays" ? [...WEEKDAYS] : [], once: false } };
      void preview(card);
    }, true) }, options.map(([value, text]) => h("option", { value }, text)));
    select.value = mode;
    const controls = [select];
    if (mode === "daily" || mode === "weekdays") controls.push(h("input.auto-time", { type: "time", value: w.at, "aria-label": "Time", onchange: event => edit(card, () => { w.at = event.target.value || "09:00"; void preview(card); }) }));
    if (mode === "cron") controls.push(h("input.auto-cron", { value: w.cron, "aria-label": "Cron", spellcheck: "false", onchange: event => edit(card, () => { w.cron = event.target.value.trim(); void preview(card); }) }));
    return controls;
  }

  function note(card) {
    const node = list.querySelector(`[data-key="${card.key}"]`);
    if (!node) return;
    const state = card.error || (card.saving || dirty(card) && savable(card.draft) ? "Saving…" : card.apply ? "Changes are not on yet" : card.next);
    node.querySelector(".auto-note").textContent = state;
    node.querySelector(".auto-note").dataset.tone = card.error ? "error" : "";
  }

  /// Bring a card's state controls up to date without rebuilding what the person may be typing in.
  function sync(card) {
    const node = list.querySelector(`[data-key="${card.key}"]`);
    if (!node) return;
    const on = !!card.head?.enabled;
    const toggle = node.querySelector(".auto-switch");
    if (!!toggle !== shows(card) || !!card.apply !== (toggle?.getAttribute("aria-checked") === "mixed") || !!card.conflict !== !!node.querySelector("[data-conflict]")) return paint(card);
    node.dataset.on = String(on);
    toggle?.setAttribute("aria-checked", String(on));
    const run = node.querySelector(".auto-run");
    if (run) run.disabled = !!card.busy || !startable(card);
    note(card);
  }

  const shows = card => card.draft.trigger.kind !== "manual" || !!card.head?.enabled;

  /// Another window saved this automation while this card had unsaved edits.
  async function resolve(card, keepMine) {
    const data = await api.call("automation_get", { automation: card.id });
    card.revision = data.entry.revision.number; card.conflict = false; card.error = "";
    if (keepMine) { paint(card); return save(card); }
    card.draft = structuredClone(data.entry.revision.draft); card.base = JSON.stringify(card.draft);
    paint(card);
  }

  function cardNode(card) {
    const busy = !!card.busy;
    const name = h("input.auto-card-name", { value: card.draft.name, placeholder: "Untitled", "aria-label": "Automation name", maxlength: 240, oninput: event => edit(card, () => { card.draft.name = event.target.value; }), onblur: () => void save(card) });
    const steps = points(card.draft);
    const on = !!card.head?.enabled;
    const node = h("li.auto-card", { dataset: { key: card.key, on: String(on) } },
      h("div.auto-card-top", name,
        h("button.btn.auto-run", { type: "button", disabled: busy || !startable(card), onclick: () => void run(card) }, icon("play"), "Run")),
      h("ol.auto-points", steps.map((step, index) => pointRow(card, step, index, steps.length))),
      h("div.auto-card-foot",
        steps.length < POINTS ? h("button.auto-add-point", { type: "button", onclick: () => {
          edit(card, () => { const at = points(card.draft).length; card.draft.steps.splice(at, 0, { id: uid(), name: label("", at), action: { kind: "agent", target: { kind: "general", model: null }, prompt: "", inputs: [] } }); }, true);
          [...list.querySelectorAll(`[data-key="${card.key}"] textarea`)].at(-1)?.focus();
        } }, icon("plus"), "Step") : null,
        h("span.auto-note", { role: "status", "aria-live": "polite" }),
        card.conflict ? h("button.auto-add-point", { type: "button", "data-conflict": true, onclick: () => void resolve(card, false).catch(error => { card.error = error.message; note(card); }) }, "Use saved") : null,
        card.conflict ? h("button.auto-add-point", { type: "button", onclick: () => void resolve(card, true).catch(error => { card.error = error.message; note(card); }) }, "Keep mine") : null,
        ...whenControls(card),
        shows(card) ? h("button.auto-switch", { type: "button", role: "switch", "aria-checked": card.apply ? "mixed" : String(on), "aria-label": "On", title: card.apply ? "Turn on these changes" : null, disabled: busy, onclick: () => void toggle(card) }) : null));
    return node;
  }

  function paint(card) {
    const old = list.querySelector(`[data-key="${card.key}"]`);
    if (!old) return;
    const focus = document.activeElement, inside = old.contains(focus);
    const at = inside ? [...old.querySelectorAll("input,textarea,select,button")].indexOf(focus) : -1;
    const selection = inside && "selectionStart" in focus ? [focus.selectionStart, focus.selectionEnd] : null;
    const next = cardNode(card); old.replaceWith(next); note(card);
    if (at >= 0) {
      const target = next.querySelectorAll("input,textarea,select,button")[at];
      target?.focus({ preventScroll: true });
      if (selection && target && "setSelectionRange" in target) try { target.setSelectionRange(...selection); } catch { /* time and select inputs have no text selection */ }
    }
  }

  function legacyNode(schedule) {
    return h("li.auto-card.auto-legacy", { dataset: { state: schedule.state } },
      h("button.auto-legacy-open", { type: "button", "aria-label": `${schedule.objective} · ${schedule.says} · ${schedule.admission_error || schedule.status}`, onclick: () => api.openPanel("schedule", { id: schedule.id }) },
        h("span.auto-legacy-what", schedule.objective),
        h("span.auto-legacy-when", schedule.says), h("span.auto-note", schedule.admission_error || schedule.status)));
  }

  function draw() {
    const empty = !saved.cards.length && !saved.legacy.length;
    if (empty && saved.loaded) saved.cards.push(fresh());
    list.replaceChildren(...saved.cards.map(cardNode), ...saved.legacy.map(legacyNode));
    for (const card of saved.cards) note(card);
  }

  async function load() {
    const own = ++generation;
    try {
      const entries = [];
      let cursor = null;
      do {
        const page = await api.call("automations", { cursor, limit: 50 });
        entries.push(...page.entries); cursor = page.next;
      } while (cursor && alive && own === generation);
      const schedules = await api.call("schedules", {});
      if (!alive || own !== generation) return;
      saved.loaded = true; status.textContent = "";
      // Never swap a card out from under the person: apply once focus leaves the cards.
      if (list.contains(document.activeElement)) { saved.stale = true; return; }
      const known = new Map(saved.cards.filter(card => card.id).map(card => [card.id, card]));
      const cards = entries.map(entry => {
        const card = known.get(entry.definition.id) || { key: crypto.randomUUID(), next: "", error: "" };
        card.head = entry.definition;
        if (card.saving || card.draft && dirty(card)) return card;
        const theirs = structuredClone(entry.revision.draft);
        return Object.assign(card, { id: entry.definition.id, revision: entry.definition.revision, draft: theirs, base: JSON.stringify(theirs) });
      });
      saved.cards = [...saved.cards.filter(card => !card.id), ...cards];
      saved.legacy = schedules.filter(schedule => !schedule.target && !entries.some(entry => entry.definition.schedule === schedule.id));
      draw();
      for (const card of cards) if (card.draft.trigger.kind === "schedule" && !card.next) void preview(card);
    } catch (error) {
      if (!alive || own !== generation) return;
      status.replaceChildren(document.createTextNode(error.message), h("button.btn", { type: "button", onclick: () => void load() }, "Refresh"));
    }
  }

  list.addEventListener("focusout", event => {
    if (!saved.stale || list.contains(event.relatedTarget)) return;
    saved.stale = false; void load();
  });
  draw(); el.scrollTop = saved.scroll;
  if (!saved.loaded) status.textContent = "Loading…";
  void load();
  const off = api.on("ark:orch", () => { clearTimeout(timer); timer = setTimeout(() => void load(), 250); });
  return {
    refresh: () => load(),
    dispose() {
      alive = false; generation++; clearTimeout(timer); off?.(); saved.scroll = el.scrollTop;
      for (const card of saved.cards) if (dirty(card)) void save(card);
    },
  };
}

export function registerAutomations(api) {
  api.view({ id: "schedules", title: "Automations", icon: "clock", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => library(el, params, scoped || api) });
}
