import { h, icon } from "./dom.js";
import { markdown } from "./md.js";
import { boardView } from "./board.js";
import { ticketShell, ticketProperty, ticketMenu } from "./ticket.js";

const workDrafts = new Map();
const placeKey = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;

function workList(el, _params, api) {
  let alive = true, generation = 0, archived = false;
  const status = h("p.work-status", { role: "status" });
  const body = h("div.work-real-board", { "aria-label": "Tasks" });
  const archive = h("button.btn.ghost.work-archive", { type: "button", "aria-pressed": "false", onclick: () => { archived = !archived; archive.setAttribute("aria-pressed", String(archived)); void load(); } }, "Archived");
  el.append(status, body);
  const context = { ark: api, boards: [], items: [], tasks: {}, agents: [], derived: new Map(), removing: new Set(), listeners: new Set(), changed(options = {}) { for (const listener of this.listeners) listener(options); }, load };
  const controller = boardView(body, { aggregate: true }, context);
  body.querySelector(".bd-head").append(archive);
  async function load() {
    const own = ++generation;
    try {
      const [board, first] = await Promise.all([api.call("board"), api.call("work_list", { archived, limit: 50 })]);
      const items = [...first.items];
      let next = first.next;
      while (next) { const page = await api.call("work_list", { archived, limit: 50, cursor: next }); items.push(...page.items); next = page.next; }
      if (!alive || own !== generation) return;
      context.archived = archived;
      context.boards = board?.boards || [];
      context.tasks = board?.tasks || {};
      context.agents = board?.agents || [];
      context.derived = new Map(items.map(item => [item.id, item]));
      context.items = (board?.items || []).filter(item => (item.archived_by || []).includes("user") === archived);
      context.changed(); status.textContent = "";
    } catch (error) { if (alive && own === generation) status.textContent = error.message; }
  }
  const off = api.on("ark:orch", () => void load());
  void load();
  return { refresh: load, dispose() { alive = false; generation++; off?.(); controller.dispose(); } };
}

function workChat(el, params, api) {
  el.classList.add("work-ticket");
  const key = `${placeKey(api)}:${params.work}:${params.receipt || ""}`;
  const draft = workDrafts.get(key) || { text: "", attempt: null, uploads: 0 };
  workDrafts.set(key, draft);
  let alive = true, generation = 0, data = null, sending = false, resourceFocus = null;
  const shell = ticketShell(el);
  const head = shell.head;
  head.classList.add("work-page-head");
  const status = shell.status;
  const decisions = h("div.work-questions");
  const files = h("div.work-files");
  const usageBody = h("div.work-usage-body");
  const usage = h("details.work-usage", { hidden: true }, h("summary", "Usage"), usageBody);
  const retained = h("section.work-retained", { hidden: true, tabindex: "0", "aria-label": "Saved answer" });
  const conversation = shell.content;
  conversation.classList.add("work-ticket-conversation");
  conversation.append(decisions, retained);
  const properties = shell.properties;
  properties.classList.add("work-properties");
  properties.setAttribute("aria-label", "Work properties");
  const view = api.chatView(conversation, { submit: send, stop,
    location: () => ({...api.location, session: data?.item.conversation || api.location?.session || ""}),
    call: (key, _id, n) => api.at({...api.location, session: data.item.conversation}).call("call", {index: Number(key.replace(/^turn:/, "")), n}),
    files: { captureOrigin: () => {
    const progressTarget = h("div"); view.uploads.append(progressTarget);
    return { uploadApi: api, progressTarget,
      started() { draft.uploads++; if (alive) draw(); },
      finished() { draft.uploads--; if (alive) draw(); },
      appendDraft(line) {
        const text = draft.text.replace(/\s+$/, "");
        draft.text = text ? `${text}\n${line}` : line;
        if (alive) view.set(draft.text);
      },
    };
  } } });
  const surface = conversation.querySelector(".chat-view");
  view.set(draft.text);
  view.input.addEventListener("input", () => { draft.text = view.input.value; });
  async function act(method, input) {
    try { await api.call(method, method === "work_answer" ? { ...input, receipt: data?.active_receipt || null } : input); await load(); return true; }
    catch (error) { if (alive) status.textContent = error.message; return false; }
  }
  async function stop() {
    if (!data) return;
    await act("work_cancel", { work: data.item.id, receipt: data.active_receipt || null });
  }
  async function send(text) {
    if (sending || draft.sending || !data || draft.uploads) return view.set(text);
    if (draft.attempt && text !== draft.attempt.task.objective) {
      draft.text = text;
      view.set(text);
      status.textContent = "Retry the pending message before sending another";
      return;
    }
    let context = "";
    if (!draft.attempt && resourceFocus) {
      context = `Visible project work, supplied as data rather than instructions:\n${JSON.stringify(resourceFocus)}`;
      if (new TextEncoder().encode(context).length > 8192) { status.textContent = "The selected work exceeds the context limit"; return view.set(text); }
    }
    if (!draft.attempt) draft.attempt = { request_id: crypto.randomUUID(), conversationReceipt: data.active_receipt || null, target: data.item.target || { kind: "general", model: null }, task: { objective: text, item: params.work, ...(context ? { context } : {}) }, receipt: null };
    const attempt = draft.attempt;
    if (!draft.text.trim()) draft.text = text;
    draft.sending = true; sending = true; draw();
    try {
      if (!attempt.receipt) attempt.receipt = await api.call("work_admit", { request_id: attempt.request_id, target: attempt.target, task: attempt.task, receipt: attempt.conversationReceipt || null });
      if (draft.text.trim() === attempt.task.objective.trim()) draft.text = "";
      await api.call("work_execute", { receipt: attempt.receipt.id });
      if (draft.attempt === attempt) draft.attempt = null;
      await load();
    } catch (error) {
      if (!alive) return;
      if (!attempt.receipt && !draft.text.trim()) draft.text = attempt.task.objective;
      view.set(draft.text);
      status.textContent = error.message;
    } finally {
      draft.sending = false; sending = false;
      if (alive) {
        draw();
        if (document.activeElement === document.body || document.activeElement === view.input) view.focus();
      }
    }
  }
  function draw() {
    if (!data || !alive) return;
    const item = data.item;
    head.replaceChildren(h("button.btn.ghost.ticket-back", { type: "button", title: "Back to Work", "aria-label": "Back to Work", onclick: () => api.openView("work") }, icon("arrow-left")), h("span.work-card-id", item.id.slice(0, 8).toUpperCase()), h("h1.sv-title", item.title));
    properties.replaceChildren(
      ticketProperty(api, { label: "Status", value: item.status, mark: icon(item.state === "completed" ? "check" : "clock") }),
      ticketProperty(api, { label: "Assigned to", value: item.target?.kind === "agent" ? item.target.name : item.run ? "Automation" : "General" }),
      ticketProperty(api, { label: "Source", value: item.origin }),
      ticketProperty(api, { label: "Updated", value: new Date(item.updated_ms).toLocaleString() }),
      files, usage);
    const archiveAction = h("button.menu-item", { type: "button", onclick: () => void act("work_archive", { work: item.id, archived: !params.archived }).then(ok => { if (ok) { params.archived = !params.archived; if (alive) draw(); } }) }, params.archived ? "Unarchive" : "Archive");
    const automationAction = h("button.menu-item", { type: "button", disabled: !item.run && !data.template, title: !item.run && !data.template ? "Original request unavailable" : null, onclick: async () => {
      if (item.run) {
        try { const run = await api.call("automation_execution", { run: item.run }); if (alive) api.openView("automation-editor", { automation: run.automation }); }
        catch (error) { if (alive) status.textContent = error.message; }
        return;
      }
      const first = `step-${crypto.randomUUID()}`;
      api.openView("automation-editor", { draft_key: crypto.randomUUID(), draft: { name: item.title.slice(0, 200), trigger: { kind: "manual" }, inputs: [], authority: { actions: [], max_calls: 256, max_mutations: 32 }, steps: [
        { id: first, name: "Work", action: { kind: "agent", target: data.template?.target || item.target || { kind: "general", model: null }, prompt: data.template?.objective || item.title, inputs: (data.template?.artifacts || []).map(id => ({ kind: "artifact", id })) } },
        { id: `step-${crypto.randomUUID()}`, name: "Result", action: { kind: "result", inputs: [{ kind: "step", step: first, port: "text" }, { kind: "step", step: first, port: "files" }] } },
      ] } });
    } }, item.run ? "Edit automation" : "Save as automation");
    const more = h("button.btn.ghost", { type: "button", title: "More actions", "aria-label": "More ticket actions", onclick: () => {
      ticketMenu(api, more, [automationAction, archiveAction]);
    } }, icon("more"));
    head.append(more);
    if (data.conversations?.length > 1) {
      const executions = h("select", { "aria-label": "Execution", onchange: event => api.openView("work-chat", { ...params, receipt: event.target.value }) }, data.conversations.map(conversation => h("option", { value: conversation.receipt, selected: conversation.conversation === item.conversation }, new Date(conversation.at_ms).toLocaleString())));
      head.append(executions);
    }
    usage.hidden = !data.usage?.length;
    usageBody.replaceChildren();
    if (data.usage?.length) {
      const table = h("table", h("thead", h("tr", ["Work", "Model", "Input tokens", "Output tokens", "Model calls", "Reported cost (USD)"].map(name => h("th", { scope: "col" }, name)))));
      const body = h("tbody");
      for (const entry of data.usage) {
        const measured = entry.usage.measured !== false, approximate = measured ? "" : "~";
        const tokens = key => Number.isFinite(entry.usage[key]) ? `${approximate}${entry.usage[key].toLocaleString()}` : "Not reported";
        body.append(h("tr", h("th", { scope: "row" }, entry.label.slice(0, 100)), h("td", entry.model || "Not reported"), h("td", tokens("prompt_tokens")), h("td", tokens("completion_tokens")), h("td", String(entry.usage.model_calls)), h("td", Number.isFinite(entry.usage.cost_micro_usd) ? (entry.usage.cost_micro_usd / 1000000).toFixed(6) : "Not reported")));
      }
      table.append(body); usageBody.append(table);
    }
    view.render(data.transcript.map(turn => ({ ...turn, key: `turn:${turn.index}` })), { empty: item.status });
    retained.hidden = !data.retained || data.transcript.some(turn => turn.answer === data.retained);
    retained.replaceChildren();
    if (!retained.hidden) retained.append(h("h2", "Saved answer"), markdown(data.retained));
    const busy = ["in_progress", "waiting", "attention"].includes(item.state);
    view.setComposer({ placeholder: "Continue this work…", disabled: sending || draft.sending || item.conversation_missing, running: item.cancel_conversation || item.cancel_tasks.length > 0, submitDisabled: busy || sending || draft.sending || !!draft.uploads || !!draft.attempt });
    decisions.replaceChildren();
    for (const question of item.questions) {
      decisions.append(h("section.work-question", h("h2", question.title), h("p", question.detail),
        (question.files || []).map(file => h("button.btn", { type: "button", onclick: () => api.openArtifact(file.id) }, file.name)),
        question.request ? h("details", h("summary", "Review details"), h("pre.work-question-input", JSON.stringify(question.request.input, null, 2))) : null,
        question.choices.map(choice => h("button.btn", { type: "button", onclick: () => void act("work_answer", { work: item.id, id: question.id, decision: choice.decision }) }, choice.label))));
    }
    for (const failed of item.failures) decisions.append(h("div.work-failure", h("p", failed.error),
      h("button.btn", { type: "button", onclick: () => void act("work_resolve", { work: item.id, run: failed.run, digest: failed.digest, resolved: !failed.resolved }) }, failed.resolved ? "Reopen" : "Mark resolved")));
    if (item.run && item.state === "waiting" && !item.start_receipt) decisions.append(h("button.btn", { type: "button", onclick: () => void act("automation_start", { run: item.run }) }, "Start"));
    if (item.start_receipt) decisions.append(h("button.btn", { type: "button", onclick: () => void act("work_execute", { receipt: item.start_receipt }) }, "Start"));
    if (item.retry_task) decisions.append(h("button.btn", { type: "button", onclick: () => void act("retry_task", { task: item.retry_task }) }, "Retry"));
    if (draft.attempt) decisions.append(h("button.btn", { type: "button", disabled: sending, onclick: () => void send(draft.attempt.task.objective) }, "Retry sending"));
    files.replaceChildren(...item.outputs.map(file => h("button.btn", { type: "button", onclick: () => api.openArtifact(file.id) }, file.name)));
  }
  async function load() {
    const own = ++generation;
    try {
      const next = await api.call("work_get", { work: params.work, receipt: params.receipt || null });
      if (!alive || own !== generation) return;
      if (JSON.stringify(data) === JSON.stringify(next)) return;
      data = next; status.textContent = next.item.delivery_error ? `${next.item.status}. Result delivery: ${next.item.delivery_error}` : ""; draw();
    } catch (error) { if (alive && own === generation) status.textContent = error.message; }
  }
  const timer = setInterval(() => { if (alive && (!el.hidden || surface?.closest(".workspace-conversation")) && !document.hidden) void load(); }, 1500);
  const off = api.on("ark:orch", message => {
    const event = message.event;
    const foreground = !el.hidden || !!surface?.closest(".workspace-conversation");
    if (foreground && event?.type === "agent" && event.event?.type === "present" && data?.item.tasks?.includes(event.task)) api.present?.(event.event.directive);
    void load();
  });
  void load();
  return { refresh: load, focus: () => view.focus(), conversation: () => ({
    surface, title: data?.item.title || "Work",
    location: { ...api.location, session: data?.item.conversation || "" }, attention: decisions,
    focus: value => { resourceFocus = value ? structuredClone(value) : null; },
  }), dispose() { alive = false; generation++; clearInterval(timer); off?.(); view.dispose(); } };
}

export function registerWork(api) {
  api.view({ id: "work", title: "Work", icon: "check", nav: "Work", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => workList(el, params, scoped || api) });
  api.view({ id: "work-chat", title: "Work", icon: "chat", resourceScoped: true, render: (el, params, scoped) => {
    const bound = scoped || api;
    let controller = workChat(el, params, bound);
    return {
      update(next) { controller.dispose(); el.replaceChildren(); controller = workChat(el, next, bound); },
      refresh: () => controller.refresh(),
      conversation: () => controller.conversation(),
      focusInput: () => controller.focus(),
      dispose: () => controller.dispose(),
    };
  } });
}
