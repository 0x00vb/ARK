// What this chat set up to run on its own (ADR 0053). One line per schedule above the composer,
// in the chat that asked for it: when, what, and where it is (next run, running now, trying
// again, waits for you, paused, did not finish). The line opens the schedule in the side pane:
// its runs with their results and files, Run now, Pause or Resume, Stop.
//
// Making and changing a schedule is asked in words; the page has no form for it.
//
// Work that starts when a message arrives (an event automation, ADR 0062) gets the same line
// beside its chat and its own pane: which messages start it, what each run may read and change
// as the person approved it, where it is, and the one action that recovers it.

import { h, icon, clear, clip } from "./dom.js";
import { markdown } from "./md.js";

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
const MOVES = new Set(["scheduled", "schedule_changed", "unscheduled", "automation_changed", "unautomated", "task_created", "task_queued", "task_started", "task_finished", "task_retry", "asked", "answered"]);

async function load() {
  const chat = store.ark.summary?.()?.session?.id;
  store.chat = chat || null;
  try {
    store.list = chat ? (await store.ark.call("automations", { session: chat })) || [] : [];
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

/// Every schedule in the current project. This is separate from `store`, whose compact lines
/// intentionally follow only the open chat. A request generation keeps a late reply from one
/// project out of the next project's view.
function schedulesView(el, params, ark) {
  el.classList.add("sv");
  let project = params?.project?.id || ark.project?.()?.id || null;
  let generation = 0;
  let list = [];
  let loading = true;
  let error = "";
  let stale = false;
  let reloadTimer;

  const heading = h("h1.sv-title", { tabindex: "-1", "data-destination-heading": "" }, "Schedules");
  const live = h("div.sr-only", { role: "status", "aria-live": "polite", "aria-atomic": "true" });
  const body = h("div.sv-body");
  el.append(h("header.sv-head", heading), live, body);

  function row(s) {
    const count = counters(s);
    const objective = s.objective || "Schedule";
    const status = s.status || s.state || "Scheduled";
    const says = s.says || "";
    const agent = s.agent || "";
    const last = s.last && !status.startsWith(s.last) ? s.last : "";
    const label = [objective, status, last, count, says, agent].filter(Boolean).join(". ");
    return h("li.sv-item",
      h("button.sv-row", {
        type: "button",
        "data-state": s.state,
        onclick: () => ark.openPanel("schedule", { id: s.id }),
        "aria-label": label,
      },
      h("span.sv-mark", s.state === "running" ? h("span.working-dot") : icon("clock")),
      h("span.sv-main",
        h("span.sv-objective", objective),
        h("span.sv-meta", h("span", says), agent ? h("span.sv-agent", agent) : null)),
      h("span.sv-state", { "data-tone": TONE[s.state] || null },
        h("span.sv-status", status),
        last ? h("span.sv-last", last) : null,
        count ? h("span.sv-count", count) : null)));
  }

  function draw({ announce = "" } = {}) {
    clear(body);
    live.textContent = announce;
    el.setAttribute("aria-busy", String(loading));
    if (loading && !list.length) {
      body.append(h("div.sv-loading", { "aria-hidden": "true" },
        h("span.sv-skeleton"), h("span.sv-skeleton"), h("span.sv-skeleton")));
      return;
    }
    if (error && !list.length) {
      body.append(h("div.sv-empty",
        h("div.sv-empty-icon", icon("alert")),
        h("h2", "Schedules are unavailable"),
        h("p", "Nothing changed. Try again when the connection is back."),
        h("button.btn.small", { type: "button", onclick: () => load() }, "Try again")));
      return;
    }
    if (!list.length) {
      body.append(h("div.sv-empty",
        h("div.sv-empty-icon", icon("clock")),
        h("h2", "No schedules"),
        h("p", "Ask in a chat for work to run at a date or time.")));
      return;
    }
    if (stale) {
      body.append(h("div.sv-stale",
        h("span", "Could not refresh. Showing the last update."),
        h("button.btn.ghost.small", { type: "button", onclick: () => load() }, "Try again")));
    }
    body.append(h("ul.sv-list", list.map(row)));
  }

  async function load() {
    const requestedProject = project;
    const request = ++generation;
    loading = true;
    error = "";
    draw({ announce: list.length ? "" : "Loading schedules" });
    try {
      const next = (await ark.call("schedules", {})) || [];
      if (request !== generation || requestedProject !== project) return;
      list = next;
      stale = false;
      loading = false;
      draw();
    } catch (e) {
      if (request !== generation || requestedProject !== project) return;
      error = String(e?.message || e || "unavailable");
      stale = list.length > 0;
      loading = false;
      draw({ announce: stale ? "Schedules could not refresh" : "Schedules are unavailable" });
    }
  }

  const reload = () => load();
  const soon = () => {
    clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => project && load(), 250);
  };
  const offs = [
    ark.on("ark:orch", (m) => { if (!m.ref && MOVES.has(m.event?.type)) soon(); }),
    ark.on("ark:orch_lagged", reload),
    ark.on("ark:connected", reload),
    ark.on("ark:post", soon),
  ];
  load();
  return {
    update(next) {
      const id = next?.project?.id || ark.project?.()?.id || null;
      if (id === project) return load();
      generation++;
      project = id;
      list = [];
      error = "";
      stale = false;
      loading = true;
      draw({ announce: "Loading schedules" });
      load();
    },
    refresh: load,
    focus: () => ({ surface: { kind_tag: "schedules", title: "Schedules" } }),
    dispose() {
      generation++;
      clearTimeout(reloadTimer);
      for (const off of offs) off?.();
    },
  };
}

function draw() {
  const el = document.getElementById("plans");
  if (!el) return;
  clear(el);
  for (const s of store.list) {
    if (s.kind === "event") {
      el.append(h("button.plan", {
        type: "button",
        "data-tone": s.tone === "muted" ? null : s.tone,
        title: `When ${s.says}: ${s.objective}`,
        onclick: () => store.ark.openPanel("automation", { id: s.id }),
      },
        s.status === "running now" ? h("span.working-dot") : icon("bolt"),
        h("span.plan-when", when(s)),
        h("span.plan-what", s.objective),
        h("span.plan-state", s.status)));
      continue;
    }
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

async function act(method, id, done, ark = store.ark, key = "schedule") {
  try {
    await ark.call(method, { [key]: id });
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
      disabled: open ? true : null,
      title: open ? "A run is under way" : null,
      onclick: () => act("schedule_run", id, undefined, ark).then(fetch),
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
      h("div.sc-actions", run, pause, stop),
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

/// Which messages start an event automation, short: "When lara@example.com writes".
function when(s) {
  return s.from ? `When ${s.from} writes` : `When a message comes to ${s.account}`;
}

// ---- one event automation, in the side pane -----------------------------------------------------

/// The action that recovers an automation from where it is, first among its controls.
const RECOVER = {
  resume: { method: "automation_resume", label: "Resume" },
  discard: { method: "automation_discard", label: "Drop waiting messages", confirm: "Drop them for good?" },
};

function automationPanel(el, params, ark = store.ark) {
  el.classList.add("sc-panel");
  let id = params?.id;
  let data = null;
  let asking = null; // a destructive button asks once more: { which, timer }
  let gone = false;
  let failed = false;
  const opened = new Set();

  async function fetch() {
    try {
      data = await ark.call("automation", { automation: id, limit: 20 });
      gone = false;
      failed = false;
    } catch (e) {
      failed = true;
      if (/no automation/.test(e.message || "")) {
        data = null;
        gone = true;
      }
    }
    draw();
  }

  function twice(which, label, again, run) {
    const b = h("button.btn.ghost.small", { type: "button" }, asking?.which === which ? again : label);
    if (which === "stop") b.classList.add("sc-stop");
    b.onclick = () => {
      if (asking?.which !== which) {
        clearTimeout(asking?.timer);
        asking = { which, timer: setTimeout(() => { asking = null; draw(); }, 4000) };
        return draw();
      }
      clearTimeout(asking.timer);
      asking = null;
      run();
    };
    return b;
  }

  function draw() {
    clear(el);
    if (!data) {
      if (gone) el.append(h("p.sc-gone", "This automation was stopped. Its results are in the chat."));
      else if (failed) el.append(h("p.sc-gone", "Could not reach ARK. It shows again when it is back."));
      return;
    }
    const a = data.automation;
    ark.setPanelTitle("automation", clip(a.objective, 28));
    const paused = a.state === "paused" || a.state === "paused_failing";
    const actions = [];
    const fix = RECOVER[a.recovery];
    if (fix && !fix.confirm) {
      actions.push(h("button.btn.small", { type: "button", onclick: () => act(fix.method, id, undefined, ark, "automation").then(fetch) }, fix.label));
    } else if (fix) {
      actions.push(twice("discard", fix.label, fix.confirm, () => act(fix.method, id, undefined, ark, "automation").then(fetch)));
    }
    if (!paused) {
      actions.push(h("button.btn.ghost.small", { type: "button", onclick: () => act("automation_pause", id, undefined, ark, "automation").then(fetch) }, "Pause"));
    }
    actions.push(twice("stop", "Stop", "Stop for good?", () =>
      act("automation_stop", id, "Stopped", ark, "automation").then(() => ark.closePanel?.("automation"))));
    const may = [`the message${a.can_read.length ? `, ${a.can_read.join(", ")}` : ""}`];
    el.append(
      h("div.sc-head",
        h("div.sc-what", a.objective),
        h("div.sc-when", `${when(a)} to ${a.account}`),
        h("div.sc-state", { "data-tone": a.tone === "muted" ? null : a.tone }, a.status === "running now" ? h("span.working-dot") : null, a.status),
        a.last_event ? h("div.sc-last", a.last_event) : null),
      h("div.sc-actions", actions),
      h("dl.sc-may",
        h("dt", "Reads"), h("dd", may.join("")),
        h("dt", "Changes"), h("dd", a.can_change.length ? a.can_change.join("; ") : "nothing")),
    );
    if (a.context) el.append(h("details.sc-context", h("summary", "What each run is told"), h("div.prose", { html: markdown(a.context) })));
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
        (r.outputs || []).length ? h("div.sc-files", r.outputs.map((o) =>
          h("button.bd-p-file", { type: "button", title: o.name, onclick: () => ark.openArtifact(o.id) }, icon("file"), clip(o.name, 32)))) : null));
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
      clearTimeout(asking?.timer);
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
  ark.panel({ id: "automation", title: "Automation", icon: "bolt", builtin: true, render: (el, params, scoped) => automationPanel(el, params, scoped || ark) });
  ark.view({ id: "schedules", title: "Schedules", icon: "clock", projectScoped: true, render: (el, params, scoped) => schedulesView(el, params, scoped || ark) });
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
