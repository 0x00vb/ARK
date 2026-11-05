// What this chat set up to run on its own (ADR 0053). One line per schedule above the composer,
// in the chat that asked for it: when, what, and where it is (next run, running now, trying
// again, waits for you, paused, did not finish). The line opens the schedule in the side pane:
// its runs with their results and files, Run now, Pause or Resume, Stop.
//

import { h, icon, clear, clip } from "./dom.js";
import { markdown } from "./md.js";
import { registerAutomations } from "./automations.js";
import { openWork } from "./work.js";

const store = {
  ark: null,
  chat: null,
  list: [],
  listeners: new Set(),
  changed() {
    for (const fn of this.listeners) fn();
  },
};

/// Orchestrator events that can change a schedule or its last run.
const MOVES = new Set(["scheduled", "schedule_changed", "unscheduled", "task_created", "task_queued", "task_started", "task_finished", "task_retry", "asked", "answered"]);

async function load() {
  const chat = store.ark.summary?.()?.session?.id;
  store.chat = chat || null;
  try {
    store.list = chat ? (await store.ark.call("schedules", { session: chat })) || [] : [];
  } catch {
    store.list = [];
  }
  draw();
  store.changed();
}

let timer;
function loadSoon() {
  clearTimeout(timer);
  timer = setTimeout(load, 250);
}

/// The tone of a state: what needs the person stands out, the rest stays quiet.
const TONE = { asking: "ask", failed: "err", retrying: "warn", running: "live", paused: "muted" };

function counters(s) {
  const out = [];
  if (s.missed) out.push(`${s.missed} missed`);
  if (s.skipped) out.push(`${s.skipped} skipped`);
  return out.join(" · ");
}

function draw() {
  const el = document.getElementById("plans");
  if (!el) return;
  clear(el);
  for (const s of store.list) {
    el.append(h("button.plan", {
      type: "button",
      "data-tone": TONE[s.state] || null,
      title: `${s.says}: ${s.objective}`,
      onclick: () => store.ark.openPanel("schedule", { id: s.id }),
    },
      s.state === "running" ? h("span.working-dot") : icon("clock"),
      h("span.plan-when", s.says),
      h("span.plan-what", s.objective),
      h("span.plan-state", s.status)));
  }
}

async function act(method, id, done, ark = store.ark) {
  try {
    await ark.call(method, { schedule: id });
    if (done) ark.toast(done);
  } catch (e) {
    ark.toast(e.message);
  }
}

// ---- one schedule, in the side pane -------------------------------------------------------------

function schedulePanel(el, params, ark = store.ark) {
  el.classList.add("sc-panel");
  let id = params?.id;
  let data = null;
  let runRequest = null, running = false;
  let stopping = null; // the Stop button asks once more before it stops
  const opened = new Set();

  let gone = false; // the schedule ended or was stopped; a failed call keeps what is shown
  let failed = false; // the last call failed (offline): said when there is nothing to show
  async function fetch() {
    try {
      data = await ark.call("schedule", { schedule: id, limit: 20 });
      gone = false;
      failed = false;
    } catch (e) {
      failed = true;
      if (/no schedule/.test(e.message || "")) {
        data = null;
        gone = true;
      }
    }
    draw();
  }

  function draw() {
    clear(el);
    if (!data) {
      if (gone) el.append(h("p.sc-gone", "This schedule has ended. Its results are in the chat."));
      else if (failed) el.append(h("p.sc-gone", "Could not reach ARK. It shows again when it is back."));
      return;
    }
    const s = data.schedule;
    ark.setPanelTitle("schedule", clip(s.objective, 28));
    const open = ["running", "asking", "retrying"].includes(s.state);
    const run = h("button.btn.small", {
      type: "button",
      disabled: open || running ? true : null,
      title: open ? "A run is under way" : null,
      onclick: async () => {
        if (running) return;
        runRequest ||= crypto.randomUUID(); running = true; draw();
        try {
          const result = await ark.call("schedule_run", { schedule: id, request: runRequest });
          runRequest = null;
          if (result.receipt.kind === "automation") {
            const execution = await ark.call("automation_execution", { run: result.execution });
            await ark.openSession(execution.conversation);
          } else {
            const execution = await ark.call("task", { task: result.task });
            if (execution.task.item) await openWork(ark, execution.task.item);
          }
        } catch (error) { ark.toast(error.message); }
        finally { running = false; await fetch(); }
      },
    }, icon("play"), "Run now");
    const pause = s.paused
      ? h("button.btn.ghost.small", { type: "button", onclick: () => act("schedule_resume", id, undefined, ark).then(fetch) }, "Resume")
      : h("button.btn.ghost.small", { type: "button", onclick: () => act("schedule_pause", id, undefined, ark).then(fetch) }, "Pause");
    const stop = h("button.btn.ghost.small.sc-stop", { type: "button" }, stopping ? "Stop for good?" : "Stop");
    stop.onclick = () => {
      if (!stopping) {
        stopping = setTimeout(() => { stopping = null; draw(); }, 4000);
        return draw();
      }
      clearTimeout(stopping);
      stopping = null;
      act("unschedule", id, "Stopped", ark).then(() => ark.closePanel?.("schedule"));
    };
    el.append(
      h("div.sc-head",
        h("div.sc-what", s.objective),
        h("div.sc-when", s.says, s.tz && s.tz !== here() ? h("span.sc-tz", s.tz) : null),
        h("div.sc-state", { "data-tone": TONE[s.state] || null }, s.state === "running" ? h("span.working-dot") : null, s.status),
        s.last && !s.status.startsWith(s.last) ? h("div.sc-last", s.last) : null),
      h("div.sc-actions", run, pause, s.target ? null : stop, s.target ? h("button.btn.small", { type: "button", onclick: () => ark.openView("schedules") }, "Edit automation") : null),
    );
    if (s.context) el.append(h("details.sc-context", h("summary", "What each run is told"), h("div.prose", { html: markdown(s.context) })));

    const runs = data.runs || [];
    if (!runs.length) return;
    const list = h("div.sc-runs");
    for (const r of runs) {
      const said = r.result || r.error || (r.waiting_on ? `Waiting: ${r.waiting_on}` : "");
      const body = h("div.sc-said.prose", { html: markdown(said) });
      if (said.length > 280 && !opened.has(r.task)) {
        body.classList.add("more");
        body.onclick = () => { body.classList.remove("more"); opened.add(r.task); };
      }
      list.append(h("div.sc-run", { "data-state": r.state },
        h("div.sc-run-head", h("span.sc-run-at", r.at), h("span.sc-run-state", word(r.state)), r.attempts > 1 ? h("span.sc-run-tries", `${r.attempts} tries`) : null),
        said ? body : null,
        (r.outputs || []).length ? h("div.sc-files", r.outputs.map((a) =>
          h("button.bd-p-file", { type: "button", title: a.name, onclick: () => ark.openArtifact(a.id) }, icon("file"), clip(a.name, 32)))) : null));
    }
    el.append(list);
  }

  const off = () => fetch();
  store.listeners.add(off);
  fetch();
  return {
    update(p) {
      if (p?.id && p.id !== id) {
        id = p.id;
        opened.clear();
        fetch();
      }
    },
    refresh: () => fetch(),
    dispose() {
      store.listeners.delete(off);
      clearTimeout(stopping);
    },
  };
}

/// The person's own zone: a schedule in it does not name it.
function here() {
  try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; }
}

function word(state) {
  return { done: "done", failed: "did not finish", cancelled: "cancelled", running: "running", queued: "waiting", waiting: "waiting" }[state] || state;
}

export function registerSchedules(ark) {
  store.ark = ark;
  ark.panel({ id: "schedule", title: "Schedule", icon: "clock", builtin: true, render: (el, params, scoped) => schedulePanel(el, params, scoped || ark) });
  registerAutomations(ark);
  ark.on("ark:orch", (m) => {
    if (m.ref) return;
    if (MOVES.has(m.event?.type)) loadSoon();
  });
  ark.on("ark:orch_lagged", load);
  ark.on("ark:connected", load);
  ark.on("ark:post", loadSoon);
  ark.on("ark:summary", (m) => {
    if (m.summary?.session?.id !== store.chat) load();
  });
  // A "next" time that has passed reads again.
  setInterval(() => store.list.length && load(), 60_000);
  load();
}

/// The chat on show changed: its own schedules.
export function schedulesFollow() {
  if (store.ark) load();
}
