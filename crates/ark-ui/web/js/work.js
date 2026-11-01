import { h, icon } from "./dom.js";
import { markdown } from "./md.js";
import { boardColumn, boardCard } from "./board.js";
import { ticketShell, ticketProperty, ticketMenu } from "./ticket.js";

const workDrafts = new Map();
const listStates = new Map();
const placeKey = api => `${api.location?.runtime || ""}:${api.location?.project || ""}`;

const groups = [
  ["attention", "Needs your attention"], ["in_progress", "In progress"],
  ["waiting", "Waiting"], ["failed", "Could not finish"],
  ["interrupted", "Interrupted"], ["completed", "Completed"], ["resolved", "Resolved"], ["cancelled", "Cancelled"],
];

function workList(el, _params, api) {
  el.classList.add("sv", "work-board");
  const key = placeKey(api);
  const saved = listStates.get(key) || { archived: false, origin: "", pages: 1, scroll: 0 };
  listStates.set(key, saved);
  let alive = true, generation = 0, archived = saved.archived, items = [], next = null, starter = null, starterControl = null, listReady = false;
  const body = h("div.bd-cols.work-columns", { "aria-label": "Work board" });
  const startArea = h("div.work-start-area", { hidden: true });
  const status = h("p.work-status", { role: "status" });
  const archive = h("button.btn", { type: "button", "aria-pressed": "false", onclick: () => {
    archived = !archived; saved.archived = archived; saved.pages = 1; archive.setAttribute("aria-pressed", String(archived)); void load();
  } }, "Archived");
  const origin = h("select.work-source", { "aria-label": "Work source", onchange: () => { saved.origin = origin.value; void load(); } }, [["", "All sources"], ["manual", "Manual"], ["schedule", "Schedule"], ["webhook", "Webhook"]].map(([value, text]) => h("option", { value }, text)));
  origin.value = saved.origin || "";
  const more = h("button.btn", { type: "button", hidden: true, onclick: () => void load(next) }, "Load more");
  el.append(h("header.sv-head.work-list-head", h("h1.sv-title", { tabindex: "-1", "data-destination-heading": true }, "Work"), origin, archive,
    h("button.btn.primary", { type: "button", onclick: () => api.newSession() }, icon("plus"), "New work")), status, body, more, startArea);
  async function load(cursor = null) {
    const own = ++generation;
    status.textContent = "Loading…";
    try {
      const focused = document.activeElement?.closest("[data-work]")?.dataset.work;
      const boardScroll = listReady ? body.scrollLeft : saved.boardScroll || 0;
      const scroll = listReady ? el.scrollTop : saved.scroll;
      let page = await api.call("work_list", { archived, cursor, limit: 50 });
      let loaded = page.items;
      if (!cursor) {
        for (let index = 1; index < saved.pages && page.next; index++) {
          page = await api.call("work_list", { archived, cursor: page.next, limit: 50 });
          loaded.push(...page.items);
        }
      }
      if (!alive || own !== generation) return;
      if (cursor) saved.pages++;
      items = cursor ? [...items, ...loaded] : loaded;
      next = page.next;
      body.replaceChildren();
      for (const [state, label] of groups) {
        const rows = items.filter(item => item.state === state && (!saved.origin || item.origin === saved.origin));
        if (!rows.length && !["attention", "in_progress", "waiting", "completed"].includes(state)) continue;
        const column = boardColumn(state, label);
        column.count.textContent = String(rows.length);
        for (const item of rows) {
          const open = () => api.openView("work-chat", { work: item.id, archived });
          column.list.append(boardCard({ "data-work": item.id, "data-state": state, "aria-label": item.title,
            onclick: open, onkeydown: event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); open(); } } },
            h("div.bd-body", h("span.work-card-id", item.id.slice(0, 8).toUpperCase()), h("div.bd-title", item.title),
              h("div.work-card-meta", h("span", item.target?.kind === "agent" ? item.target.name : item.run ? "Automation" : "General"), h("span", item.origin)),
              item.reason || item.error ? h("span.bd-tag.you", item.reason || item.error) : null)));
        }
        body.append(column.element);
      }
      status.textContent = items.some(item => !saved.origin || item.origin === saved.origin) ? "" : next ? "No matching work in loaded results" : archived ? "No archived work" : "No work yet";
      if (!archived && !saved.origin && !next && (!items.length || starter)) {
        if (!items.length && starterControl?.isComplete?.()) {
          starterControl.dispose?.();
          starter.remove();
          starter = null;
          starterControl = null;
        }
        if (!starter) {
          starter = h("div.work-empty-chat");
          starterControl = api.newChatView?.(starter);
          queueMicrotask(() => { if (alive && !archived && !items.length) starterControl?.focusInput?.(); });
          if (!starterControl) starter.append(h("button.btn", { type: "button", onclick: () => api.newSession() }, "New chat"));
        }
        if (!starter.parentNode) startArea.append(starter);
        startArea.hidden = false;
        if (!items.length) status.textContent = "";
      } else startArea.hidden = true;
      more.hidden = !next;
      body.scrollLeft = boardScroll;
      listReady = true;
      if (focused) body.querySelector(`[data-work="${CSS.escape(focused)}"]`)?.focus({ preventScroll: true });
      el.scrollTop = scroll;
    } catch (error) { if (alive && own === generation) status.replaceChildren(document.createTextNode(error.message), h("button.btn", { type: "button", onclick: () => void load() }, "Refresh")); }
  }
  archive.setAttribute("aria-pressed", String(archived));
  let refreshTimer;
  const off = api.on("ark:orch", () => {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => { if (alive) void load(); }, 250);
  });
  void load();
  return { refresh: () => load(), dispose() { saved.scroll = el.scrollTop; saved.boardScroll = body.scrollLeft; starterControl?.dispose(); alive = false; generation++; clearTimeout(refreshTimer); off?.(); } };
}

function workChat(el, params, api) {
  el.classList.add("work-ticket");
  const key = `${placeKey(api)}:${params.work}:${params.receipt || ""}`;
  const draft = workDrafts.get(key) || { text: "", attempt: null, uploads: 0 };
  workDrafts.set(key, draft);
  let alive = true, generation = 0, data = null, sending = false;
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
  const view = api.chatView(conversation, { submit: send, stop, files: { captureOrigin: () => {
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
    if (!draft.attempt) draft.attempt = { request_id: crypto.randomUUID(), conversationReceipt: data.active_receipt || null, target: data.item.target || { kind: "general", model: null }, task: { objective: text, item: params.work }, receipt: null };
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
    properties.replaceChildren(h("h2", "Properties"),
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
  const timer = setInterval(() => { if (alive && !el.hidden && !document.hidden) void load(); }, 1500);
  const off = api.on("ark:orch", () => void load());
  void load();
  return { refresh: load, focus: () => view.focus(), dispose() { alive = false; generation++; clearInterval(timer); off?.(); view.dispose(); } };
}

export function registerWork(api) {
  api.view({ id: "work", title: "Work", icon: "check", nav: "Work", projectScoped: true, resourceScoped: true, render: (el, params, scoped) => workList(el, params, scoped || api) });
  api.view({ id: "work-chat", title: "Work", icon: "chat", resourceScoped: true, render: (el, params, scoped) => {
    const bound = scoped || api;
    let controller = workChat(el, params, bound);
    return {
      update(next) { controller.dispose(); el.replaceChildren(); controller = workChat(el, next, bound); },
      refresh: () => controller.refresh(),
      focusInput: () => controller.focus(),
      dispose: () => controller.dispose(),
    };
  } });
}
