import { h, icon } from "./dom.js";
import { canvasScene, canvasTools } from "./canvas-tools.js";

const drafts = new Map();
const clone = value => structuredClone(value);
const keyFor = (api, params) => `${api.location?.runtime || ""}:${api.location?.project || ""}:${params.automation || params.legacy_schedule || params.draft_key}`;
const uid = () => `step-${crypto.randomUUID()}`;
const initial = () => {
  const first = uid();
  return { name: "", trigger: { kind: "manual" }, inputs: [], authority: { actions: [], max_calls: 256, max_mutations: 32 }, steps: [
    { id: first, name: "Work", action: { kind: "agent", target: { kind: "general", model: null }, prompt: "", inputs: [] } },
    { id: uid(), name: "Result", action: { kind: "result", inputs: [{ kind: "step", step: first, port: "text" }, { kind: "step", step: first, port: "files" }] } },
  ] };
};
const button = (text, run, attrs = {}) => h("button.btn", { type: "button", "data-action": text, onclick: event => { event.currentTarget.focus({ preventScroll: true }); run(event); }, ...attrs }, text);
const compact = (text, glyph, run, attrs = {}) => button(glyph instanceof Node ? glyph : icon(glyph), run, { "data-action": text, "aria-label": text, title: text, class: "auto-icon", ...attrs });
const label = (text, control) => h("label.auto-field", h("span", text), control);

export function editor(el, params, api) {
  params.draft_key ||= crypto.randomUUID();
  const key = keyFor(api, params);
  const state = drafts.get(key) || { id: params.automation || null, draft: clone(params.draft || initial()), saved: null, revision: null, head: null, layout: { version: 0, positions: {} }, selected: params.selected || null, undo: [], redo: [], save: null, run: null };
  drafts.set(key, state);
  el.classList.add("auto-editor");
  let canvasObserver;
  let alive = true, busy = false, loadError = false, loading = (!!state.id || !!params.legacy_schedule && !state.legacyLoaded), agents = [], artifacts = [], actions = [], models = [], previewTimer, previewGeneration = 0, historyCursor = null, historyRuns = [], validationTimer, validationGeneration = 0, validating = true, validationReady = false, canSave = true, issues = [], activationIssues = [], hook = null, hookSecret = null, zoom = state.zoom ?? 1;
  const status = h("p.auto-status", { role: "status", "aria-live": "polite" });
  const header = h("header.auto-head");
  const validation = h("div.auto-validation", { "aria-live": "polite" });
  const content = h("fieldset.auto-content");
  const history = h("section.auto-history");
  const changed = () => JSON.stringify(state.draft) !== state.saved;
  function checkpoint() {
    const value = { draft: clone(state.draft), positions: clone(state.layout.positions) };
    if (JSON.stringify(state.undo.at(-1)) !== JSON.stringify(value)) state.undo.push(value);
    if (state.undo.length > 50) state.undo.shift();
    state.redo = [];
  }
  function edit(run, redraw = true) {
    if (busy || loading || state.save || state.run) { status.textContent = state.run ? "Recover the pending start before editing" : "Retry the pending save before editing"; return; }
    checkpoint(); run(); updateHeader(); queuePreview(); if (redraw) draw();
  }
  function field(text, value, set, type = "text", attrs = {}) {
    const input = h(type === "textarea" ? "textarea" : "input", { ...(type === "textarea" ? { rows: 4 } : { type }), ...attrs, oninput: event => edit(() => set(event.target.value), false) });
    input.value = value ?? ""; input.disabled = busy || loading || !!state.save || !!state.run;
    return label(text, input);
  }
  function select(text, value, values, set) {
    const input = h("select", { disabled: busy || loading || !!state.save || !!state.run, onchange: event => edit(() => set(event.target.value)) }, values.map(([id, name]) => h("option", { value: id }, name)));
    input.value = value;
    return label(text, input);
  }
  function check(text, value, set) {
    return label(text, h("input", { type: "checkbox", checked: value, disabled: busy || loading || !!state.save || !!state.run, onchange: event => edit(() => set(event.target.checked)) }));
  }
  function updateHeader() {
    const focusedElement = document.activeElement;
    const focused = header.contains(focusedElement) ? focusedElement.dataset.action || focusedElement.getAttribute("aria-label") : null;
    const menuOpen = !!header.querySelector(".auto-more[open]");
    const inputPreview = content.querySelector("[data-preview-inputs]");
    if (inputPreview) inputPreview.disabled = busy || loading || validating || !validationReady || changed() || issues.length > 0;
    const pending = state.run ? `Start pending · revision ${state.run.expected}` : state.save ? "Save pending" : changed() ? "Unsaved changes" : state.revision ? `Saved revision ${state.revision}` : "New automation";
    const active = state.head?.enabled ? state.webhookAvailability === "local" ? `Local events enabled · revision ${state.head.active_revision}` : `Active revision ${state.head.active_revision}` : state.head?.active_revision ? "Paused" : "";
    header.replaceChildren(compact("Back", "arrow-left", () => api.openView("schedules")), h("h1.sv-title", state.draft.name || "New automation"), h("span.auto-revision", [pending, active].filter(Boolean).join(" · ")),
      compact("Undo", "undo", () => undo(false), { disabled: !state.undo.length || busy || loading || !!state.save || !!state.run }), compact("Redo", h("span.auto-flip", icon("undo")), () => undo(true), { disabled: !state.redo.length || busy || loading || !!state.save || !!state.run }),
      button(state.save ? "Retry saving" : "Save", () => void save(), { "data-action": "save", class: changed() ? "primary" : "", disabled: busy || loading || validating || !validationReady || !canSave || !!state.run || !changed() && !state.save }),
      button(state.run ? "Retry starting" : state.lastStart ? "Run again" : "Run now", () => void runDialog(), { "data-action": "run", class: changed() ? "" : "primary", disabled: busy || loading || !state.run && (validating || !validationReady || issues.length > 0) || !state.id || changed() || !!state.save }),
      button("Activate", () => void prepare(), { disabled: busy || loading || validating || !validationReady || issues.length > 0 || activationIssues.length > 0 || !state.id || changed() || !!state.save || !!state.run || state.head?.enabled && state.head.active_revision === state.revision }));
    if (params.legacy_schedule) header.append(button("Current schedule", () => api.openPanel("schedule", { id: params.legacy_schedule })));
    if (state.layoutDirty && !state.layoutError) header.append(h("span.auto-revision", state.layoutSaving ? "Saving canvas…" : "Canvas positions pending"));
    if (state.layoutError) header.append(button("Recover canvas", () => void recoverLayout(), { disabled: busy }));
    if (state.lastStart) header.append(button("Open last run", () => api.openView("work-chat", { work: state.lastStart.work })));
    if (state.run) header.append(button("Recover start", () => void recoverStart(), { disabled: busy }));
    if (state.save) header.append(button("Recover draft", () => void recoverSave(), { disabled: busy }));
    if (state.head && state.head.revision !== state.revision) header.append(button("Review latest revision", () => void recoverSave(), { disabled: busy }));
    if (loadError) header.append(button("Retry loading", () => void hydrate(), { disabled: busy }));
    if (state.head?.enabled) header.append(button("Pause", () => void perform(async () => { await api.call("automation_pause", { automation: state.id }); await reloadHead(); updateHeader(); })));
    if (state.head?.activation) header.append(button("Revoke permission", () => void revokeDialog()));
    const secondary = [...header.querySelectorAll(":scope > button")].filter(control => ["Activate", "Current schedule", "Open last run", "Pause", "Revoke permission"].includes(control.dataset.action));
    if (secondary.length) {
      const more = h("details.auto-more", h("summary.btn.ghost", { "aria-label": "More automation actions", title: "More actions" }, icon("more")), h("div.auto-more-actions", secondary));
      more.open = menuOpen;
      more.addEventListener("click", event => { if (event.target.closest("button")) more.open = false; });
      more.addEventListener("keydown", event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); more.open = false; more.querySelector("summary").focus(); } });
      header.append(more);
    }
    if (focused) [...header.querySelectorAll("button,summary")].find(control => (control.dataset.action || control.getAttribute("aria-label")) === focused)?.focus({ preventScroll: true });
  }
  function undo(redo) {
    const from = redo ? state.redo : state.undo, to = redo ? state.undo : state.redo;
    if (!from.length || busy || state.save || state.run) return;
    to.push({ draft: clone(state.draft), positions: clone(state.layout.positions) });
    const previous = from.pop(); state.draft = previous.draft; state.layout.positions = previous.positions; queueLayoutSave(); draw(); queuePreview();
  }
  async function perform(run) {
    if (busy) return;
    busy = true; content.disabled = true; status.textContent = "Working…"; updateHeader();
    try { await run(); if (alive) status.textContent = ""; }
    catch (error) { if (alive) status.textContent = error.message; }
    finally { busy = false; if (alive) { content.disabled = !!state.save || !!state.run; draw(); } }
  }
  async function reloadHead() {
    const data = await api.call("automation_get", { automation: state.id });
    if (!alive) return;
    state.lastStart = data.entry.last_start; state.head = data.entry.definition; state.webhookAvailability = data.entry.webhook_availability;
  }
  async function save() {
    state.save ||= { request: crypto.randomUUID(), automation: state.id, expected: state.revision, draft: clone(state.draft), legacy: state.adoption || null };
    const attempt = state.save;
    await perform(async () => {
      const receipt = await api.call("automation_save", attempt);
      if (state.save !== attempt) return;
      state.id = receipt.automation; state.revision = receipt.revision; state.saved = JSON.stringify(attempt.draft); state.save = null; state.adoption = null;
      drafts.set(keyFor(api, { automation: state.id }), state);
      await reloadHead();
      if (state.layoutDirty) void queueLayoutSave();
      if (alive) { draw(); await loadHistory(); if (state.draft.trigger.kind === "webhook") await loadHook(); }
    });
  }
  async function recoverSave() {
    await perform(async () => {
      if (state.save) {
        const attempt = state.save;
        const result = await api.call("automation_save", { ...attempt, cancel: true });
        if (state.save !== attempt) return;
        if (result.saved) { state.id = result.saved.automation; state.revision = result.saved.revision; state.saved = JSON.stringify(attempt.draft); state.adoption = null; }
        state.save = null;
      }
      const source = state.adoption?.schedule || (!state.head?.active_revision && state.legacyPolicies?.schedule);
      if (source) {
        const data = await api.call("automation_get", { legacy_schedule: source });
        if (!alive) return;
        dialog("Review existing schedule", (body, controls, close) => {
          body.append(h("h3", data.legacy.draft.name));
          body.append(h("p", "Existing schedule policies will apply to your draft."));
          controls.append(button("Keep reviewing", close), button("Keep my draft with these policies", () => {
            state.adoption = { schedule: data.legacy.schedule, expected: data.legacy.version, config: data.legacy.policies.source_digest };
            state.legacyPolicies = data.legacy.policies; close(); draw();
          }));
        });
        return;
      }
      if (!state.id) return;
      const data = await api.call("automation_get", { automation: state.id });
      if (!alive) return;
      if (data.entry.definition.revision === state.revision) { state.lastStart = data.entry.last_start; state.head = data.entry.definition; state.webhookAvailability = data.entry.webhook_availability; return; }
      dialog(`Saved revision ${data.entry.revision.number}`, (body, controls, close) => {
        body.append(h("h3", data.entry.revision.draft.name));
        for (const step of data.entry.revision.draft.steps) body.append(h("p", step.name));
        controls.append(button("Keep reviewing", close), button("Keep my draft on this revision", () => { state.revision = data.entry.revision.number; state.saved = JSON.stringify(data.entry.revision.draft); state.lastStart = data.entry.last_start; state.head = data.entry.definition; state.webhookAvailability = data.entry.webhook_availability; close(); draw(); }));
      });
    });
  }
  async function hydrate() {
    await perform(loadInitial);
  }
  function move(index, direction) {
    const next = index + direction;
    if (next < 0 || next >= state.draft.steps.length) return;
    edit(() => { [state.draft.steps[index], state.draft.steps[next]] = [state.draft.steps[next], state.draft.steps[index]]; });
  }
  function inputsFor(step, index) {
    const area = h("fieldset.auto-bindings", h("legend", "Inputs"));
    for (const [number, binding] of step.action.inputs.entries()) {
      const source = binding.kind === "step" ? `previous:${binding.port}` : binding.kind === "input" ? `input:${binding.name}` : binding.kind;
      const values = [["literal", "Text"], ["artifact", "Project file"], ...state.draft.inputs.map(input => [`input:${input.name}`, input.name])];
      if (index) values.push(["previous:text", `Text from ${state.draft.steps[index - 1].name}`], ["previous:files", `Files from ${state.draft.steps[index - 1].name}`]);
      const disconnected = binding.kind === "step" && (!index || binding.step !== state.draft.steps[index - 1].id);
      if (disconnected) values.push(["disconnected", `Reconnect input from ${state.draft.steps.find(step => step.id === binding.step)?.name || "removed step"}`]);
      const row = h("div.auto-binding", { dataset: { binding: number } }, select("Source", disconnected ? "disconnected" : source, values, value => {
        if (value === "disconnected") return;
        step.action.inputs[number] = value.startsWith("previous:") ? { kind: "step", step: state.draft.steps[index - 1].id, port: value.split(":")[1] } : value.startsWith("input:") ? { kind: "input", name: value.slice(6), port: state.draft.inputs.find(input => input.name === value.slice(6)).port } : value === "artifact" ? { kind: "artifact", id: artifacts[0]?.id || "" } : { kind: "literal", text: "" };
      }));
      if (binding.kind === "literal") row.append(field("Text", binding.text, value => { binding.text = value; }, "textarea"));
      if (binding.kind === "artifact") row.append(select("File", binding.id, [["", "Choose file"], ...artifacts.map(file => [file.id, file.name])], value => { binding.id = value; }));
      row.append(compact("Remove input", "x", () => edit(() => step.action.inputs.splice(number, 1)))); area.append(row);
    }
    area.append(button("Add input", () => edit(() => step.action.inputs.push(index ? { kind: "step", step: state.draft.steps[index - 1].id, port: "text" } : { kind: "literal", text: "" })), { disabled: step.action.inputs.length >= 32 }));
    return area;
  }
  function stepForm(step, index) {
    const inherited = state.legacyPolicies?.step === step.id;
    const controls = h("div.auto-step-controls", compact("Move up", "arrow-up", () => move(index, -1), { disabled: !index || index === 1 && state.legacyPolicies?.step === state.draft.steps[0].id }), compact("Move down", h("span.auto-flip-down", icon("arrow-up")), () => move(index, 1), { disabled: inherited || index === state.draft.steps.length - 1 }), compact("Remove step", "trash", () => edit(() => { state.draft.steps.splice(index, 1); }), { disabled: inherited || state.draft.steps.length === 1 }));
    const form = h("section.auto-step", { dataset: { step: step.id } }, h("header", h("h2", `${index + 1}. ${step.name}`), controls), field("Name", step.name, value => { step.name = value; }), h("span.auto-kind", { role: "status" }, { agent: "Agent", approval: "Approval", result: "Result" }[step.action.kind]));
    if (step.action.kind === "agent") {
      form.append(select("Responsible", step.action.target.kind === "general" ? "" : step.action.target.name, [...(inherited ? [] : [["", "General"]]), ...agents.filter(agent => agent.state !== "terminated").map(agent => [agent.name, agent.title || agent.name])], value => { step.action.target = value ? { kind: "agent", name: value } : { kind: "general", model: null }; }), field("Task", step.action.prompt, value => { step.action.prompt = value; }, "textarea"));
    }
    if (step.action.kind === "agent" && step.action.target.kind === "general") {
      const current = step.action.target.model || "";
      const choices = [["", "Default"], ...models.filter(backend => backend.ready).flatMap(backend => (backend.models || []).map(model => [`${backend.provider}:${model}`, `${backend.label} · ${model}`]))];
      if (current && !choices.some(([value]) => value === current)) choices.push([current, current]);
      form.append(select("Model", current, choices, value => { step.action.target.model = value || null; }));
    }
    if (step.action.kind === "approval") form.append(field("Question", step.action.title, value => { step.action.title = value; }));
    form.append(inputsFor(step, index), h("div.auto-step-issues", { role: "status" })); return form;
  }
  function drawValidation() {
    const activationBox = content.querySelector(".auto-activation-issues");
    if (activationBox) activationBox.replaceChildren(...activationIssues.map(issue => h("p.auto-issue", issue.message)));
    validation.replaceChildren(...issues.filter(issue => !issue.step).map(issue => h("p.auto-issue", `${issue.field}: ${issue.message}`)));
    for (const form of content.querySelectorAll(".auto-step")) {
      const own = issues.filter(issue => issue.step === form.dataset.step);
      form.setAttribute("aria-invalid", String(own.length > 0));
      const box = form.querySelector(".auto-step-issues");
      const text = own.map(issue => `${issue.field}: ${issue.message}`).join("\n");
      if (box.textContent !== text) box.textContent = text;
    }
    for (const issue of issues.filter(issue => issue.step && issue.step !== state.selected)) validation.append(button(`Review ${state.draft.steps.find(step => step.id === issue.step)?.name || "step"}: ${issue.message}`, () => { state.selected = issue.step; draw(); }));
  }
  function queueValidation() {
    clearTimeout(validationTimer); const own = ++validationGeneration;
    validating = true; updateHeader();
    validationTimer = setTimeout(async () => {
      try {
        const result = await api.call("automation_validate", { draft: clone(state.draft) });
        if (!alive || own !== validationGeneration) return;
        issues = result.issues; activationIssues = result.activation_issues || []; canSave = result.can_save; validationReady = true;
      } catch (error) {
        if (!alive || own !== validationGeneration) return;
        validationReady = false; issues = [{ step: null, field: "Validation", message: error.message }];
      } finally { if (alive && own === validationGeneration) { validating = false; drawValidation(); updateHeader(); } }
    }, 300);
  }
  const nextDates = h("ol.auto-next", { "aria-label": "Next three occurrences", "aria-live": "polite" });
  function queuePreview() {
    queueValidation();
    clearTimeout(previewTimer); const own = ++previewGeneration;
    if (state.draft.trigger.kind !== "schedule") { nextDates.replaceChildren(); return; }
    nextDates.replaceChildren(h("li", "Calculating…"));
    previewTimer = setTimeout(async () => {
      try {
        const draft = clone(state.draft); draft.name ||= "Preview";
        for (const step of draft.steps) { step.name ||= "Step"; if (step.action.kind === "agent") step.action.prompt ||= "Preview"; }
        const preview = await api.call("automation_preview", { draft });
        if (alive && own === previewGeneration) nextDates.replaceChildren(...(preview.next.length ? preview.next.map(occurrence => h("li", occurrence.label)) : [h("li", "No future occurrences")]));
      } catch (error) { if (alive && own === previewGeneration) nextDates.replaceChildren(h("li", error.message)); }
    }, 300);
  }
  function triggerForm() {
    const trigger = state.draft.trigger;
    const section = h("section.auto-section", select("Trigger", trigger.kind, [["manual", "Manual"], ["schedule", "Schedule"], ["webhook", "Webhook"]], value => { state.draft.trigger = value === "schedule" ? { kind: value, when: { at: "09:00", tz: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC", days: [], once: false } } : { kind: value }; if (value === "webhook" && state.id) void loadHook(); }));
    if (trigger.kind === "schedule") {
      const when = trigger.when, mode = when.every_minutes != null ? "interval" : when.cron ? "cron" : when.date ? "date" : "daily";
      section.append(select("Frequency", mode, [["daily", "Days and time"], ["interval", "Interval"], ["date", "Once"], ["cron", "Advanced: cron"]], value => { trigger.when = value === "interval" ? { every_minutes: 60, tz: when.tz } : value === "cron" ? { cron: "0 9 * * *", tz: when.tz } : { at: "09:00", tz: when.tz, days: [], ...(value === "date" ? { date: new Date().toISOString().slice(0, 10), once: true } : {}) }; }));
      if (mode === "interval") section.append(field("Every minutes", when.every_minutes, value => { when.every_minutes = Number(value); }, "number", { min: 5 }));
      else if (mode === "cron") section.append(field("Cron", when.cron, value => { when.cron = value; }));
      else {
        section.append(field("Time", when.at, value => { when.at = value; }, "time"));
        if (mode === "date") section.append(field("Date", when.date, value => { when.date = value; }, "date"));
        else { const days = h("fieldset.auto-days", h("legend", "Days")); for (const day of ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]) days.append(check(day, when.days?.includes(day), checked => { const set = new Set(when.days || []); checked ? set.add(day) : set.delete(day); when.days = [...set]; })); section.append(days); }
      }
      section.append(field("Time zone", when.tz || "UTC", value => { when.tz = value; }), nextDates, h("div.auto-activation-issues", { role: "status" }));
    }
    if (trigger.kind === "webhook") section.append(hookForm());
    return section;
  }
  function externalInputs() {
    const section = h("details.auto-section", h("summary", "Execution inputs"));
    for (const [index, input] of state.draft.inputs.entries()) section.append(h("div.auto-input-row", field("Name", input.name, value => { const old = input.name; input.name = value; for (const step of state.draft.steps) for (const binding of step.action.inputs) if (binding.kind === "input" && binding.name === old) binding.name = value; }), select("Type", input.port, [["text", "Text"], ["files", "Files"]], value => { input.port = value; for (const step of state.draft.steps) for (const binding of step.action.inputs) if (binding.kind === "input" && binding.name === input.name) binding.port = value; }), check("Required", input.required, value => { input.required = value; }), button("Remove", () => edit(() => { state.draft.inputs.splice(index, 1); for (const step of state.draft.steps) step.action.inputs = step.action.inputs.filter(binding => binding.kind !== "input" || binding.name !== input.name); }))));
    section.append(button("Add execution input", () => edit(() => state.draft.inputs.push({ name: `input_${state.draft.inputs.length + 1}`, port: "text", required: true })), { disabled: state.draft.inputs.length >= 32 })); return section;
  }
  function inheritedPolicies() {
    const policies = state.legacyPolicies;
    if (!policies) return null;
    const section = h("details.auto-section", h("summary", "Original task policies"), h("p", `${policies.max_attempts} attempts · ${Math.round(policies.retry_delay_ms / 60000)} minutes before retry`));
    if (policies.report_to) section.append(h("p", "Final result returns to the original conversation."));
    for (const [name, value] of Object.entries(policies.budget || {})) if (value != null) section.append(h("p", `${{max_steps:"Maximum steps",max_tokens:"Maximum tokens",max_model_calls:"Maximum model calls",timeout_secs:"Time limit in seconds",max_delegations:"Maximum delegations"}[name] || name}: ${value}`));
    for (const rule of policies.rules) section.append(h("p", `${rule.ruleset} · revision ${rule.version}`));
    if (policies.after.length) section.append(h("p", `Waits for ${policies.after.length} existing work items`));
    return section;
  }
  function permissions() {
    const section = h("details.auto-section", h("summary", "Permissions"), field("Maximum calls", state.draft.authority.max_calls, value => { state.draft.authority.max_calls = Number(value); }, "number", { min: 1, max: 1000 }), field("Maximum changes", state.draft.authority.max_mutations, value => { state.draft.authority.max_mutations = Number(value); }, "number", { min: 0, max: 1000 }));
    for (const [index, allowed] of state.draft.authority.actions.entries()) {
      const descriptor = actions.find(action => action.name === allowed.action);
      const row = h("fieldset.auto-permission", h("legend", descriptor?.summary || allowed.action), field("Maximum per execution", allowed.per_run, value => { allowed.per_run = Number(value); }, "number", { min: 1, max: 1000 }), field("Maximum per hour", allowed.quota.count, value => { allowed.quota.count = Number(value); allowed.quota.window_ms = 3600000; }, "number", { min: 1, max: 1000 }));
      if (descriptor?.auto_eligible && !descriptor.person_only && !descriptor.ask_each_time) row.append(check("Allow changes without asking", allowed.unattended, value => { allowed.unattended = value; }));
      for (const [name, bound] of Object.entries(allowed.inputs)) {
        row.append(select(name, bound.kind, [["free", "Any value"], ["literal", "Exact value"], ...(state.draft.inputs.some(input => input.port === "text") ? [["input", "Execution input"]] : [])], value => { allowed.inputs[name] = value === "literal" ? { kind: value, value: "" } : value === "input" ? { kind: value, name: state.draft.inputs.find(input => input.port === "text").name } : { kind: value }; }));
        if (bound.kind === "literal") row.append(field("Exact value", typeof bound.value === "string" ? bound.value : JSON.stringify(bound.value), value => { const type = descriptor?.input_schema?.properties?.[name]?.type; bound.value = type === "number" || type === "integer" ? Number(value) : type === "boolean" ? value === "true" : value; }));
        if (bound.kind === "input") row.append(select("Execution input", bound.name, state.draft.inputs.filter(input => input.port === "text").map(input => [input.name, input.name]), value => { bound.name = value; }));
      }
      row.append(button("Remove permission", () => edit(() => state.draft.authority.actions.splice(index, 1)))); section.append(row);
    }
    const picker = h("select", { "aria-label": "Add permission" }, h("option", { value: "" }, "Choose action"), actions.filter(action => !action.person_only && !state.draft.authority.actions.some(allowed => allowed.action === action.name)).map(action => h("option", { value: action.name }, action.summary)));
    section.append(picker, button("Add permission", () => { const descriptor = actions.find(action => action.name === picker.value); if (descriptor) edit(() => state.draft.authority.actions.push({ action: descriptor.name, inputs: Object.fromEntries(Object.keys(descriptor.input_schema?.properties || {}).map(name => [name, { kind: "free" }])), unattended: false, per_run: 8, quota: { count: 60, window_ms: 3600000 } })); })); return section;
  }
  async function queueLayoutSave() {
    state.layoutDirty = true;
    if (!state.id || changed() || state.layoutSaving || state.layoutError) return;
    state.layoutSaving = true;
    try {
      while (state.layoutDirty) {
        state.layoutDirty = false;
        const positions = Object.fromEntries(Object.entries(state.layout.positions).filter(([id]) => state.draft.steps.some(step => step.id === id)));
        const result = await api.call("automation_layout", { automation: state.id, expected: state.layout.version, layout: { version: state.layout.version, positions } });
        state.layout.version = result.version;
      }
    } catch (error) {
      state.layoutError = error.message; state.layoutDirty = true;
      if (alive) status.textContent = error.message;
    } finally { state.layoutSaving = false; if (alive) updateHeader(); }
  }
  async function recoverLayout() {
    await perform(async () => {
      const current = await api.call("automation_get", { automation: state.id });
      if (!alive) return;
      dialog("Recover canvas positions", (body, controls, close) => {
        body.append(h("p", `Saved layout ${current.layout.version}`));
        controls.append(button("Use saved positions", () => { state.layout = current.layout; state.layoutError = null; state.layoutDirty = false; close(); draw(); }), button("Keep my positions", () => { state.layout.version = current.layout.version; state.layoutError = null; close(); void queueLayoutSave(); draw(); }));
      });
    });
  }
  function canvas() {
    const viewport = h("div.auto-canvas-viewport", { tabindex: "0", "aria-label": "Automation canvas" });
    viewport.addEventListener("scroll", () => { state.pan = { left: viewport.scrollLeft, top: viewport.scrollTop }; }, { passive: true });
    const plane = h("div.auto-canvas-plane", { style: { transform: `scale(${zoom})` } });
    const bounds = h("div.auto-canvas-bounds", plane); viewport.append(bounds);
    function sizeBounds() {
      bounds.style.width = `${Number.parseFloat(plane.style.width) * zoom}px`;
      bounds.style.height = `${Number.parseFloat(plane.style.height) * zoom}px`;
    }
    const positions = state.layout.positions;
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg"); svg.classList.add("auto-connections"); svg.setAttribute("aria-hidden", "true"); plane.append(svg);
    const portPoint = (id, selector, fallback) => {
      const dot = plane.querySelector(`[data-step="${CSS.escape(id)}"] ${selector}`);
      if (!dot?.isConnected) return fallback;
      const rect = dot.getBoundingClientRect(), origin = plane.getBoundingClientRect();
      return { x: (rect.left + rect.width / 2 - origin.left) / zoom, y: (rect.top + rect.height / 2 - origin.top) / zoom };
    };
    const coords = (step, index) => positions[step.id] || { x: 40 + index * 320, y: 180 };
    let connecting = null;
    function connect(target) {
      if (!connecting) return;
      const source = connecting; connecting = null;
      for (const control of plane.querySelectorAll("[data-port]")) control.setAttribute("aria-pressed", "false");
      const index = state.draft.steps.findIndex(step => step.id === target);
      const step = state.draft.steps[index];
      if (!step || index < 1 || state.draft.steps[index - 1].id !== source.step) {
        status.textContent = "Connect to the next step"; return;
      }
      if (step.action.inputs.some(input => input.kind === "step" && input.step === source.step && input.port === source.port)) {
        status.textContent = "Already connected"; return;
      }
      if (step.action.inputs.length >= 32) { status.textContent = "This step has 32 inputs"; return; }
      edit(() => { step.action.inputs.push({ kind: "step", ...source }); state.selected = target; });
      status.textContent = "Connected";
    }
    viewport.addEventListener("keydown", event => {
      if (event.key === "Escape" && connecting) {
        connecting = null; status.textContent = "Connection cancelled";
        for (const control of plane.querySelectorAll("[data-port]")) control.setAttribute("aria-pressed", "false");
        event.preventDefault(); event.stopPropagation();
      }
    });
    const startPosition = Array.from({ length: state.draft.steps.length * 2 + 1 }, (_, index) => ({ x: 40 + index * 320, y: 30 })).find(position => !state.draft.steps.some((step, index) => {
      const node = coords(step, index);
      return node.x < position.x + 240 && node.x + 240 > position.x && node.y < position.y + 100 && node.y + 172 > position.y;
    }));
    const start = h("section.auto-node.auto-start", { style: { left: `${startPosition.x}px`, top: `${startPosition.y}px` }, dataset: { selected: String(state.selected === "@start") } },
      button("Start", () => { state.selected = "@start"; draw(); }, { class: "auto-node-handle", "aria-label": "Select Start", "aria-pressed": String(state.selected === "@start") }),
      h("span.auto-kind", { manual: "Manual", schedule: "Schedule", webhook: "Webhook" }[state.draft.trigger.kind]));
    plane.append(start);
    function edges() {
      plane.style.width = `${Math.max(640, startPosition.x + 280, ...state.draft.steps.map((step,index) => coords(step,index).x + 280))}px`;
      plane.style.height = `${Math.max(400, ...state.draft.steps.map((step,index) => coords(step,index).y + (plane.querySelector(`[data-step="${CSS.escape(step.id)}"]`)?.offsetHeight || 220) + 20))}px`;
      sizeBounds();
      svg.replaceChildren();
      state.draft.steps.forEach((step, index) => {
        const before = index ? coords(state.draft.steps[index - 1], index - 1) : startPosition, after = coords(step, index);
        const path = document.createElementNS(svg.namespaceURI, "path");
        path.setAttribute("class", "auto-sequence-edge");
        path.setAttribute("d", index ? `M ${before.x + 240} ${before.y + 26} C ${before.x + 280} ${before.y + 26}, ${after.x - 40} ${after.y + 26}, ${after.x} ${after.y + 26}` : `M ${before.x + 120} ${before.y + 84} C ${before.x + 120} ${before.y + 134}, ${after.x + 120} ${after.y - 50}, ${after.x + 120} ${after.y}`);
        svg.append(path);
        for (const input of step.action.inputs.filter(input => input.kind === "step")) {
          const sourceIndex = state.draft.steps.findIndex(source => source.id === input.step);
          if (sourceIndex < 0) continue;
          const source = coords(state.draft.steps[sourceIndex], sourceIndex);
          const output = portPoint(input.step, `[data-port="${input.port}"] .auto-port-dot`, { x: source.x + 240, y: source.y + (input.port === "text" ? 114 : 146) });
          const target = portPoint(step.id, ".auto-port-input .auto-port-dot", { x: after.x, y: after.y + 114 });
          const edge = document.createElementNS(svg.namespaceURI, "path");
          edge.setAttribute("d", `M ${output.x} ${output.y} C ${output.x + 40} ${output.y}, ${target.x - 40} ${target.y}, ${target.x} ${target.y}`);
          edge.dataset.port = input.port; svg.append(edge);
        }
      });
    }
    state.draft.steps.forEach((step, index) => {
      const position = coords(step, index);
      const node = h("section.auto-node", { style: { left: `${position.x}px`, top: `${position.y}px` }, dataset: { step: step.id, selected: String(state.selected === step.id) } });
      const handle = button(`${index + 1}. ${step.name}`, () => { state.selected = step.id; draw(); }, { "aria-label": `Select ${step.name}`, "aria-pressed": String(state.selected === step.id), class: "auto-node-handle" });
      let drag;
      handle.addEventListener("pointerdown", event => {
        if (event.button !== 0) return;
        const current = coords(step, index); drag = { pointer: event.pointerId, x: event.clientX, y: event.clientY, start: { ...current }, moved: false };
        handle.setPointerCapture(event.pointerId);
      });
      handle.addEventListener("pointermove", event => {
        if (!drag || drag.pointer !== event.pointerId) return;
        const dx = (event.clientX - drag.x) / zoom, dy = (event.clientY - drag.y) / zoom;
        if (Math.abs(dx) + Math.abs(dy) < 4 && !drag.moved) return;
        if (!drag.moved) checkpoint();
        drag.moved = true; positions[step.id] = { x: Math.max(0, Math.min(5000, drag.start.x + dx)), y: Math.max(0, Math.min(5000, drag.start.y + dy)) };
        node.style.left = `${positions[step.id].x}px`; node.style.top = `${positions[step.id].y}px`; edges();
      });
      handle.addEventListener("pointerup", event => {
        if (!drag) return;
        const moved = drag.moved; drag = null;
        if (handle.hasPointerCapture(event.pointerId)) handle.releasePointerCapture(event.pointerId);
        if (moved) { event.preventDefault(); void queueLayoutSave(); }
      });
      handle.addEventListener("pointercancel", () => { if (drag?.moved) { positions[step.id] = drag.start; node.style.left = `${drag.start.x}px`; node.style.top = `${drag.start.y}px`; edges(); } drag = null; });
      handle.addEventListener("keydown", event => {
        const delta = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[event.key];
        if (!delta || !event.altKey) return;
        event.preventDefault(); checkpoint(); const current = coords(step, index), distance = event.shiftKey ? 50 : 10;
        positions[step.id] = { x: Math.max(0, Math.min(5000, current.x + delta[0] * distance)), y: Math.max(0, Math.min(5000, current.y + delta[1] * distance)) };
        node.style.left = `${positions[step.id].x}px`; node.style.top = `${positions[step.id].y}px`; edges(); void queueLayoutSave();
      });
      const ports = h("div.auto-node-ports");
      if (index) {
        const input = button("Inputs", () => connect(step.id), { class: "auto-port-input", "data-input-step": step.id, "aria-label": `Connect input to ${step.name}` });
        input.prepend(h("span.auto-port-dot", { "aria-hidden": "true" })); ports.append(input);
      }
      for (const port of ["text", "files"]) {
        let dragged = false, pointerStart, preview;
        const control = button(port === "text" ? "Text" : "Files", () => {
          if (dragged) { dragged = false; return; }
          connecting = { step: step.id, port }; control.setAttribute("aria-pressed", "true"); status.textContent = "Choose the next step's input";
        }, { class: "auto-port-output", "data-port": port, "aria-pressed": "false", "aria-label": `Connect ${port} from ${step.name}` });
        control.append(h("span.auto-port-dot", { "aria-hidden": "true" }));
        control.addEventListener("pointerdown", event => {
          if (event.button !== 0 || !event.isPrimary) return;
          dragged = false; pointerStart = { x: event.clientX, y: event.clientY }; connecting = { step: step.id, port }; control.setPointerCapture(event.pointerId);
        });
        control.addEventListener("pointermove", event => {
          if (!control.hasPointerCapture(event.pointerId)) return;
          if (!dragged && Math.abs(event.clientX - pointerStart.x) + Math.abs(event.clientY - pointerStart.y) < 4) return;
          dragged = true;
          if (!preview) { preview = document.createElementNS(svg.namespaceURI, "path"); svg.append(preview); }
          const bounds = plane.getBoundingClientRect(), current = coords(step, index);
          const x = (event.clientX - bounds.left) / zoom, y = (event.clientY - bounds.top) / zoom;
          const output = portPoint(step.id, `[data-port="${port}"] .auto-port-dot`, { x: current.x + 240, y: current.y + (port === "text" ? 114 : 146) });
          preview.setAttribute("d", `M ${output.x} ${output.y} C ${output.x + 40} ${y}, ${x - 40} ${y}, ${x} ${y}`);
        });
        control.addEventListener("pointerup", event => {
          if (!control.hasPointerCapture(event.pointerId)) return;
          control.releasePointerCapture(event.pointerId);
          preview?.remove(); preview = null;
          if (dragged) {
            const input = document.elementFromPoint(event.clientX, event.clientY)?.closest("[data-input-step]");
            if (input) { input.focus({ preventScroll: true }); connect(input.dataset.inputStep); }
            else { connecting = null; status.textContent = "Connection cancelled"; }
          }
        });
        control.addEventListener("pointercancel", () => { connecting = null; dragged = false; preview?.remove(); preview = null; });
        ports.append(control);
      }
      node.append(handle, h("span.auto-kind", { agent: "Agent", approval: "Approval", result: "Result" }[step.action.kind]), h("span.auto-node-summary", step.action.prompt || step.action.title || ""), ports); plane.append(node);
    });
    edges();
    canvasObserver?.disconnect(); canvasObserver = new ResizeObserver(edges); canvasObserver.observe(viewport);
    queueMicrotask(() => { if (alive && viewport.isConnected) edges(); });
    function zoomTo(value, fit = false) {
      const width = Number.parseFloat(plane.style.width), height = Number.parseFloat(plane.style.height);
      const x = width * zoom <= viewport.clientWidth ? width / 2 : (viewport.scrollLeft + viewport.clientWidth / 2) / zoom;
      const y = height * zoom <= viewport.clientHeight ? height / 2 : (viewport.scrollTop + viewport.clientHeight / 2) / zoom;
      zoom = value; state.zoom = zoom; plane.style.transform = `scale(${zoom})`; sizeBounds(); edges();
      viewport.scrollTo({ left: fit ? 0 : Math.max(0, x * zoom - viewport.clientWidth / 2), top: fit ? 0 : Math.max(0, y * zoom - viewport.clientHeight / 2) });
      state.pan = { left: viewport.scrollLeft, top: viewport.scrollTop };
    }
    const toolbar = h("div.auto-canvas-tools", compact("Zoom out", h("span.auto-minus", "−"), () => zoomTo(Math.max(.05, zoom - .1))), compact("Zoom in", "plus", () => zoomTo(Math.min(1.5, zoom + .1))), button("Fit", () => {
      const width = Number.parseFloat(plane.style.width), height = Number.parseFloat(plane.style.height);
      zoomTo(Math.min(1, Math.max(.05, Math.min(viewport.clientWidth / width, viewport.clientHeight / height))), true);
    }));
    const notice = h("p.canvas-notice", { role: "status" });
    const scene = canvasScene(`editor:${key}`, message => { notice.textContent = message; });
    canvasTools({ viewport, plane, scene, toolbar, scale: () => zoom, create: point => {
      state.insertPoint = point;
      const add = content.querySelector(".auto-add");
      if (add && add.querySelector("summary").getAttribute("aria-disabled") !== "true") { add.open = true; add.querySelector("button")?.focus(); }
    } });
    return h("div.auto-canvas", toolbar, viewport, notice);
  }
  function draw() {
    if (!alive) return;
    const pageScroll = el.scrollTop;
    const active = document.activeElement;
    const viewport = content.querySelector(".auto-canvas-viewport");
    const viewportWidth = viewport?.clientWidth;
    if (viewport) state.pan = { left: viewport.scrollLeft, top: viewport.scrollTop };
    const controlKey = control => JSON.stringify([
      control.closest("[data-step]")?.dataset.step || "", control.closest("[data-binding]")?.dataset.binding || "",
      control.tagName, control.getAttribute("aria-label") || control.closest("label")?.firstElementChild?.textContent || control.textContent,
    ]);
    const focused = content.contains(active) ? controlKey(active) : null;
    const selectedNode = content.querySelector(".auto-node[data-selected=true]");
    const returnFocus = selectedNode?.classList.contains("auto-start") ? ".auto-start .auto-node-handle" : selectedNode?.dataset.step ? `.auto-node[data-step="${CSS.escape(selectedNode.dataset.step)}"] .auto-node-handle` : null;
    const headerLabel = header.contains(active) ? active.dataset.action || active.getAttribute("aria-label") || active.textContent : null;
    const selection = active?.tagName === "TEXTAREA" || active?.tagName === "INPUT" && ["text", "search", "url"].includes(active.type) ? [active.selectionStart, active.selectionEnd] : null;
    updateHeader(); content.disabled = busy || loading || !!state.save || !!state.run; content.replaceChildren();
    const inspector = h("aside.auto-inspector", { "aria-label": "Selected node" });
    content.append(h("div.auto-workspace", canvas(), inspector));
    const index = state.draft.steps.findIndex(step => step.id === state.selected);
    const inspectorBody = h("div.auto-inspector-body");
    if (state.selected === "@start") inspectorBody.append(field("Name", state.draft.name, value => { state.draft.name = value; }), triggerForm(), externalInputs());
    else if (index >= 0) inspectorBody.append(stepForm(state.draft.steps[index], index));
    else inspector.hidden = true;
    if (!inspector.hidden) {
      const controls = inspectorBody.querySelector(".auto-step-controls");
      let more = null;
      if (controls) {
        for (const control of controls.querySelectorAll("button")) control.append(h("span", control.getAttribute("aria-label")));
        more = h("details.auto-more.auto-step-menu", h("summary.btn", { "aria-label": "Step actions", title: "Step actions" }, icon("more")), controls);
        more.addEventListener("click", event => { if (event.target.closest("button")) more.open = false; });
        more.addEventListener("keydown", event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); more.open = false; more.querySelector("summary").focus({ preventScroll: true }); } });
        inspectorBody.querySelector(".auto-step > header").remove();
      }
      inspector.append(h("header.auto-inspector-head", h("h2", state.selected === "@start" ? "Start" : state.draft.steps[index].name), more, compact("Close inspector", "x", () => { state.selected = null; draw(); }, { class: "auto-inspector-close ghost" })), inspectorBody);
    }
    const addDisabled = state.draft.steps.length >= 8 || busy || loading || !!state.save || !!state.run;
    const addChoices = h("div.auto-more-actions");
    const add = h("details.auto-add.auto-more", h("summary.btn", { "aria-label": "Add step", "aria-disabled": String(addDisabled), onclick: event => { if (addDisabled) event.preventDefault(); } }, icon("plus"), "Add node"), addChoices);
    add.addEventListener("click", event => { if (event.target.closest("button")) add.open = false; });
    add.addEventListener("keydown", event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); add.open = false; add.querySelector("summary").focus(); } });
    for (const kind of ["agent", "approval", "result"]) addChoices.append(button(`Add ${kind === "agent" ? "agent" : kind}`, () => edit(() => {
      const previous = state.draft.steps.at(-1), inputs = previous ? [{ kind: "step", step: previous.id, port: "text" }, { kind: "step", step: previous.id, port: "files" }] : [];
      const step = { id: uid(), name: { agent: "Work", approval: "Approval", result: "Result" }[kind], action: { kind, inputs, ...(kind === "agent" ? { target: { kind: "general", model: null }, prompt: "" } : kind === "approval" ? { title: "Continue?" } : {}) } };
      state.draft.steps.push(step); state.selected = step.id; if (state.insertPoint) { state.layout.positions[step.id] = state.insertPoint; state.insertPoint = null; state.layoutDirty = true; }
    }), { disabled: state.draft.steps.length >= 8 || busy || loading || !!state.save || !!state.run }));
    content.querySelector(".auto-canvas-tools").append(add);
    const inherited = inheritedPolicies();
    const settings = permissions();
    if (inherited) settings.append(inherited);
    content.append(settings);
    drawValidation();
    const nextViewport = content.querySelector(".auto-canvas-viewport");
    if (nextViewport && state.pan) { nextViewport.scrollLeft = state.pan.left; nextViewport.scrollTop = state.pan.top; }
    if (nextViewport && viewportWidth != null && viewportWidth !== nextViewport.clientWidth) {
      nextViewport.querySelector(".auto-node[data-selected=true]")?.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "auto" });
      state.pan = { left: nextViewport.scrollLeft, top: nextViewport.scrollTop };
    }
    if (focused) {
      const controls = [...content.querySelectorAll("input,textarea,select,button,summary")];
      const control = controls.find(control => controlKey(control) === focused)
        || (returnFocus ? content.querySelector(returnFocus) : null);
      control?.focus({ preventScroll: true });
      if (selection && control?.setSelectionRange && (control.tagName === "TEXTAREA" || ["text", "search", "url"].includes(control.type))) control.setSelectionRange(...selection);
    } else if (headerLabel) [...header.querySelectorAll("button,summary")].find(button => (button.dataset.action || button.getAttribute("aria-label") || button.textContent) === headerLabel)?.focus({ preventScroll: true });
    el.scrollTop = pageScroll;
  }
  function dialog(title, render) {
    const dialog = h("dialog.auto-dialog"), heading = h("h2", title), body = h("div.auto-dialog-body"), actions = h("div.auto-dialog-actions");
    const close = () => { dialog.close(); dialog.remove(); };
    dialog.addEventListener("close", () => dialog.remove(), { once: true });
    dialog.append(heading, body, actions); el.append(dialog); render(body, actions, close); dialog.showModal(); return dialog;
  }
  async function prepare() {
    await perform(async () => {
      const activation = await api.call("automation_prepare", { automation: state.id, expected: state.revision });
      if (!alive) return;
      dialog(`Activate revision ${activation.revision}`, (body, controls, close) => {
        for (const [step, selected] of Object.entries(activation.selections)) body.append(h("p", `${state.draft.steps.find(value => value.id === step)?.name || step}: ${selected.model}`));
        if (state.legacyPolicies) body.append(inheritedPolicies());
        body.append(h("p", `Maximum ${state.draft.authority.max_calls} calls and ${state.draft.authority.max_mutations} changes per execution`));
        for (const allowed of state.draft.authority.actions) {
          const descriptor = activation.descriptors[allowed.action];
          const bounds = Object.entries(allowed.inputs).map(([name, bound]) => `${name}: ${bound.kind === "literal" ? JSON.stringify(bound.value) : bound.kind === "input" ? bound.name : bound.kind === "workspace_path" ? bound.prefix : "any value"}`).join(", ");
          body.append(h("section.auto-grant", h("h3", descriptor.summary), h("p", bounds), h("p", `${allowed.per_run} per execution · ${allowed.quota.count} per ${Math.round(allowed.quota.window_ms / 60000)} minutes`), h("strong", allowed.unattended ? "Changes allowed without asking" : "Changes require approval")));
        }
        controls.append(button("Cancel", close), button(`Activate revision ${activation.revision}`, () => { close(); void perform(async () => { await api.call("automation_activate", { automation: state.id, activation: activation.id, digest: activation.digest }); await reloadHead(); }); }, { class: "primary" }));
      });
    });
  }
  function revokeDialog() {
    dialog("Revoke permission", (body, controls, close) => {
      body.append(h("p", "This stops new actions in executions using the active permission."));
      controls.append(button("Cancel", close), button("Revoke", () => { close(); void perform(async () => {
        const current = await api.call("automation_get", { automation: state.id });
        const activation = current.activation;
        if (!activation || activation.id !== state.head.activation) throw new Error("Reload the active permission before revoking it");
        await api.call("automation_revoke", { activation: activation.id, digest: activation.digest }); await reloadHead();
      }); }));
    });
  }
  async function runDialog() {
    if (state.run) return perform(start);
    if (!state.draft.inputs.length) { state.run = { previous: state.lastStart?.run || null, request: crypto.randomUUID(), automation: state.id, expected: state.revision, inputs: {} }; return perform(start); }
    dialog("Run automation", (body, controls, close) => {
      const fields = new Map();
      for (const input of state.draft.inputs) {
        const control = input.port === "text" ? h("textarea", { rows: 3, required: input.required }) : h("select", { multiple: true, required: input.required }, artifacts.map(file => h("option", { value: file.id }, file.name)));
        fields.set(input.name, control); body.append(label(input.name, control));
      }
      controls.append(button("Cancel", close), button("Run now", () => {
        const inputs = {};
        for (const input of state.draft.inputs) {
          const control = fields.get(input.name);
          if (!control.reportValidity()) return;
          inputs[input.name] = input.port === "text" ? { kind: "text", value: control.value } : { kind: "files", ids: [...control.selectedOptions].map(option => option.value) };
        }
        state.run = { previous: state.lastStart?.run || null, request: crypto.randomUUID(), automation: state.id, expected: state.revision, inputs }; close(); void perform(start);
      }, { class: "primary" }));
    });
  }
  async function recoverStart() {
    await perform(async () => {
      if (!state.run) return;
      const attempt = state.run;
      const result = await api.call("automation_cancel_start", attempt);
      if (state.run?.request !== attempt.request) return;
      state.run = null;
      if (alive && result.run) api.openView("work-chat", { work: result.run.work });
    });
  }
  async function start() {
    const attempt = state.run;
    const pending = await api.call("automation_begin_start", attempt);
    if (state.run?.request !== attempt.request) return;
    const same = pending.request === attempt.request && pending.automation === attempt.automation && pending.expected === attempt.expected && Object.keys(pending.inputs).length === Object.keys(attempt.inputs).length && Object.entries(pending.inputs).every(([key, value]) => JSON.stringify(value) === JSON.stringify(attempt.inputs[key]));
    state.run = pending;
    if (!same) throw new Error("Another start is pending. Recover it before starting again.");
    const run = await api.call("automation_run", pending);
    if (state.run?.request !== pending.request) return;
    const acknowledged = await api.call("automation_acknowledge_start", pending);
    if (state.run?.request !== pending.request) return;
    state.lastStart = acknowledged; state.run = null;
    if (alive) api.openView("work-chat", { work: run.work });
  }
  function testWebhookInputs() {
    const automation = state.id, expected = state.revision;
    dialog("Test webhook inputs", (body, controls, close) => {
      const fields = new Map(), result = h("div", { "aria-live": "polite" });
      for (const input of state.draft.inputs) {
        const control = input.port === "text" ? h("textarea", { rows: 3, required: input.required }) : h("select", { multiple: true, required: input.required }, artifacts.map(file => h("option", { value: file.id }, file.name)));
        fields.set(input.name, control); body.append(label(input.name, control));
      }
      body.append(result);
      const preview = button("Preview inputs", async () => {
        const inputs = {};
        for (const input of state.draft.inputs) {
          const control = fields.get(input.name);
          if (!control.reportValidity()) return;
          inputs[input.name] = input.port === "text" ? { kind: "text", value: control.value } : { kind: "files", ids: [...control.selectedOptions].map(option => option.value) };
        }
        preview.disabled = true;
        try {
          const checked = await api.call("automation_preview_inputs", { automation, expected, inputs });
          if (!alive || !result.isConnected) return;
          const payload = h("textarea", { rows: 8, readonly: true });
          payload.value = JSON.stringify({ inputs: checked.inputs }, null, 2);
          result.replaceChildren(h("p", "Inputs valid · no execution started"), label("Delivery payload", payload), h("h3", "Input bindings"), ...checked.bindings.map(binding => h("p", `${state.draft.steps.find(step => step.id === binding.step)?.name || binding.step}: ${binding.input} (${binding.port})`)));
        } catch (error) { if (alive && result.isConnected) result.replaceChildren(h("p.auto-issue", error.message)); }
        finally { if (alive && preview.isConnected) preview.disabled = false; }
      });
      controls.append(button("Close", close), preview);
    });
  }
  function hookForm() {
    if (!state.id) return h("p", "Save this automation to configure its webhook.");
    const section = h("div.auto-hook", button("Test inputs", testWebhookInputs, { dataset: { previewInputs: "true" }, disabled: busy || loading || validating || !validationReady || changed() || issues.length > 0 }));
    if (!hook) { section.append(button("Configure webhook", () => void loadHook())); return section; }
    if (!hook.connection.available) { section.append(h("p", { role: "status" }, "No webhook connection is available.")); return section; }
    const endpoint = hook.endpoint, draft = endpoint?.draft;
    const base = h("input", { type: "url", value: hook.connection.bases[0] || "", "aria-label": "Webhook address" });
    section.append(label("Connection address", base), button(draft ? "New signing key" : "Set up webhook", () => void perform(async () => {
      state.hookSetup ||= { automation: state.id, expected: state.revision, request: crypto.randomUUID(), base: base.value, max_per_hour: 60 };
      const result = await api.call("webhook_setup", state.hookSetup); state.hookSetup = null; hookSecret = result.secret; await loadHook();
    }), { disabled: changed() }));
    if (draft) {
      section.append(h("p", draft.availability === "local" ? "Local connection" : draft.availability === "verified" ? "Connection verified from this app" : "Connection not verified"), fieldRead("Webhook URL", draft.url), button("Verify connection", () => void perform(async () => { await api.call("webhook_probe", { endpoint: endpoint.id, connection: draft.id }); await loadHook(); })));
      if (hookSecret) {
        const secret = h("input", { type: "password", readonly: true, value: hookSecret });
        section.append(label("Signing key", secret), button("Show signing key", () => { secret.type = secret.type === "password" ? "text" : "password"; }), button("Copy signing key", () => void navigator.clipboard.writeText(hookSecret).catch(error => { status.textContent = error.message; })));
      }
      section.append(fieldRead("Connection ID", draft.id));
      for (const delivery of hook.deliveries || []) section.append(h("p", `${new Date(delivery.at_ms).toLocaleString()} · ${delivery.state}`));
    }
    return section;
  }
  function fieldRead(text, value) { return label(text, h("input", { value, readonly: true })); }
  async function loadHook() {
    try { const result = await api.call("webhook_get", { automation: state.id }); if (alive) { hook = result; draw(); } }
    catch (error) { if (alive) status.textContent = error.message; }
  }
  async function loadHistory(cursor = null) {
    if (!state.id) return;
    const page = await api.call("automation_history", { automation: state.id, cursor, limit: 20 });
    if (!alive) return;
    historyCursor = page.next; historyRuns = cursor ? [...historyRuns, ...page.runs] : page.runs;
    history.replaceChildren(h("h2", "History"));
    for (const run of historyRuns) history.append(h("div.auto-history-row", button(`${new Date(run.created_ms).toLocaleString()} · revision ${run.revision} · ${run.phase}`, () => void perform(async () => {
      const execution = await api.call("automation_execution", { run: run.id });
      if (!alive) return;
      dialog(`Revision ${execution.revision.number} · ${execution.phase}`, (body, controls, close) => {
        for (const step of execution.revision.draft.steps) {
          const recorded = execution.steps.find(value => value.id === step.id), selected = execution.selections[step.id];
          body.append(h("section.auto-recorded-step", h("h3", step.name), selected ? h("p", selected.model) : null, h("p", recorded.phase), recorded.output?.text ? h("pre", recorded.output.text) : null));
          for (const file of recorded.output?.files || []) body.append(button(file.name, () => api.openArtifact(file.id)));
        }
        controls.append(button("Close", close), button("Open work", () => { close(); api.openView("work-chat", { work: execution.work }); }));
      });
    })), button("Open work", () => api.openView("work-chat", { work: run.work }))));
    if (historyCursor) history.append(button("Load more", () => void perform(() => loadHistory(historyCursor))));
  }
  const keydown = event => {
    if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "z" || event.target.closest("input,textarea,[contenteditable]")) return;
    event.preventDefault(); undo(event.shiftKey);
  };
  let historyTimer;
  const off = api.on("ark:orch", () => {
    clearTimeout(historyTimer);
    historyTimer = setTimeout(() => { if (alive && state.id && !busy) void loadHistory().catch(error => { if (alive) status.textContent = error.message; }); }, 250);
  });
  const outsideActions = event => {
    for (const more of el.querySelectorAll(".auto-more[open]")) if (!more.contains(event.target)) more.open = false;
  };
  document.addEventListener("pointerdown", outsideActions);
  el.addEventListener("keydown", keydown); el.append(header, status, validation, content, history); draw(); queuePreview();
  async function loadInitial() {
    loadError = false;
    try {
      const [roster, files, offered, catalog] = await Promise.all([api.call("agents", {}), api.call("artifacts", {}), api.call("tools", {}), api.call("project_models", {})]);
      if (!alive) return;
      agents = roster; artifacts = files; actions = offered; models = catalog;
      if (params.legacy_schedule && !state.legacyLoaded && state.saved === null && !state.id) {
        const data = await api.call("automation_get", { legacy_schedule: params.legacy_schedule });
        if (!alive) return;
        state.draft = data.legacy.draft; state.adoption = { schedule: data.legacy.schedule, expected: data.legacy.version, config: data.legacy.policies.source_digest }; state.legacyPolicies = data.legacy.policies; state.legacyLoaded = true; loading = false;
      }
      if (state.id) {
        const data = await api.call("automation_get", { automation: state.id });
        if (!alive) return;
        loading = false;
        state.run = data.entry.pending_start || state.run;
        if (state.saved !== null) { state.lastStart = data.entry.last_start; state.head = data.entry.definition; state.webhookAvailability = data.entry.webhook_availability; }
        else { state.draft = data.entry.revision.draft; state.revision = data.entry.revision.number; state.saved = JSON.stringify(state.draft); state.lastStart = data.entry.last_start; state.head = data.entry.definition; state.webhookAvailability = data.entry.webhook_availability; state.layout = data.layout; state.legacyPolicies = data.entry.revision.legacy; }
      }
      draw(); queuePreview(); if (state.layoutDirty && !state.layoutError) void queueLayoutSave(); await loadHistory(); if (state.draft.trigger.kind === "webhook" && state.id) await loadHook();
    } catch (error) { loadError = true; if (alive) updateHeader(); throw error; }
  }
  void loadInitial().catch(error => { if (alive) status.textContent = error.message; });
  return { summary: () => ({ name: state.draft.name }), focus: () => content.querySelector("input")?.focus(), dispose() { alive = false; canvasObserver?.disconnect(); off?.(); clearTimeout(historyTimer); clearTimeout(previewTimer); clearTimeout(validationTimer); validationGeneration++; previewGeneration++; hookSecret = null; el.removeEventListener("keydown", keydown); document.removeEventListener("pointerdown", outsideActions); } };
}

export function registerAutomationEditor(api) {
  api.view({ id: "automation-editor", title: "Automation", icon: "clock", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => {
    const bound = scoped || api;
    let controller = editor(el, params, bound);
    return {
      update(next) { controller.dispose(); el.replaceChildren(); controller = editor(el, next, bound); },
      focusInput: () => controller.focus(),
      dispose: () => controller.dispose(),
    };
  } });
}
