// The Processes panel (ADR 0041): what agents started in the project, with live output and
// Stop. A start opens it; a process that becomes ready with a page to show opens that page in
// the Browser panel. Only for processes of the project the window shows (`here`).
//
//   procs            the list, newest first, running before ended
//   proc_output      everything kept of one process's output
//   proc_stop        stop one, with everything it started

import { h, icon, clear } from "./dom.js";
import { xterm, load, css } from "./workbench.js";

const STATE_WORDS = { running: "Running", ready: "Ready", exited: "Ended", stopped: "Stopped" };

/// What a state says in its row: ready shows the address, ended its exit code.
function stateWord(p) {
  if (p.state === "exited") return `Ended · exit ${p.code ?? "?"}`;
  return STATE_WORDS[p.state] || p.state;
}

const running = (p) => p.state === "running" || p.state === "ready";

export function processes(el, params, ark) {
  el.classList.add("wb-col", "proc");
  const s = { list: [], id: params?.id || null, term: null, fit: null, offs: [] };
  const rows = h("div.proc-list");
  const head = h("div.proc-head");
  const out = h("div.proc-out");
  el.append(rows, head, out);

  const select = async (id) => {
    s.id = id;
    drawRows();
    drawHead();
    if (!s.term) return;
    s.term.reset();
    if (!id) return;
    try {
      const r = await ark.call("proc_output", { id });
      if (s.id === id) s.term.write(r.data || "");
    } catch {}
  };

  const stop = async (id, btn) => {
    if (btn) btn.disabled = true;
    try { await ark.call("proc_stop", { id }); } catch (e) { ark.toast?.(e.message); }
  };

  const drawRows = () => {
    clear(rows);
    if (!s.list.length) {
      rows.append(h("div.wb-empty", "Nothing runs. When an agent needs to run the project, it asks first and its output shows here."));
      return;
    }
    for (const p of s.list) {
      const where = p.folder ? p.folder : "";
      const row = h("button.proc-row", { type: "button", "data-state": p.state, "aria-current": p.id === s.id ? "true" : null, onclick: () => select(p.id) },
        h("span.proc-dot", { "aria-hidden": "true" }),
        h("span.proc-cmd", p.command),
        where ? h("span.proc-where", where) : "",
        h("span.proc-state", stateWord(p)));
      rows.append(row);
    }
  };

  const drawHead = () => {
    clear(head);
    const p = s.list.find((x) => x.id === s.id);
    if (!p) return;
    const open = p.url ? h("button.icon-btn", { type: "button", title: "Open in the Browser", "aria-label": "Open in the Browser", onclick: () => ark.openPanel("browser", { locator: p.url }) }, icon("globe")) : "";
    const stopBtn = running(p)
      ? h("button.proc-stop", { type: "button", onclick: (e) => stop(p.id, e.currentTarget) }, icon("stop"), "Stop")
      : "";
    head.append(
      h("span.proc-url", p.url || (p.folder ? `${p.project}/${p.folder}` : p.project)),
      h("span.spacer"),
      open,
      stopBtn,
    );
  };

  const refresh = async () => {
    try { s.list = await ark.call("procs"); } catch { s.list = []; }
    if (!s.list.some((p) => p.id === s.id)) s.id = (s.list.find(running) || s.list[0])?.id || null;
    drawRows();
    drawHead();
  };

  (async () => {
    let Terminal;
    try {
      Terminal = await xterm();
    } catch (e) {
      return out.append(h("div.wb-empty", e.message));
    }
    s.term = new Terminal({
      disableStdin: true,
      convertEol: true,
      cursorBlink: false,
      fontSize: 12,
      fontFamily: css("--mono") || "ui-monospace, Menlo, monospace",
      theme: { background: css("--pane") || "#181818", foreground: css("--text") || "#ececeb", cursor: "transparent", selectionBackground: "rgba(127,127,127,0.3)" },
      scrollback: 5000,
    });
    s.fit = new window.FitAddon.FitAddon();
    s.term.loadAddon(s.fit);
    s.term.open(out);
    try { s.fit.fit(); } catch {}
    await refresh();
    select(s.id);
  })();

  load("processes.css").catch(() => {});
  const ro = new ResizeObserver(() => { try { s.fit?.fit(); } catch {} });
  ro.observe(out);
  s.offs.push(ark.on("ark:proc", (m) => {
    if (ark.location && (!m.ref || JSON.stringify(m.ref.location) !== JSON.stringify(ark.location))) return;
    const ev = m.event || {};
    if (ev.type === "output") {
      if (ev.id === s.id) s.term?.write(ev.data);
      return;
    }
    if (ev.type === "started" && m.here) s.id = ev.process.id;
    refresh().then(() => { if (ev.type === "started" && m.here) select(ev.process.id); });
  }));

  return {
    focus: () => ({ surface: { kind_tag: "processes", title: "Processes", locator: s.id || undefined } }),
    update: (p) => p?.id && refresh().then(() => select(p.id)),
    refresh: () => { refresh(); setTimeout(() => { try { s.fit?.fit(); } catch {} }, 0); },
    dispose: () => {
      ro.disconnect();
      for (const off of s.offs) off();
      if (ark.location) ark.resource(ark.resourceRef("process", "", ark.location), "unsubscribe").catch(() => {});
      s.term?.dispose();
    },
  };
}

/// Open the panel when a process starts in this window's project, and its page in the Browser
/// once it is ready, when the agent asked for that.
export function watchProcesses(ark) {
  ark.on("ark:proc", (m) => {
    const ev = m.event || {};
    if (!m.here) return;
    const here = ark.summary()?.location;
    if (m.ref && (!here || JSON.stringify(m.ref.location) !== JSON.stringify(here))) return;
    if (ev.type === "started") ark.openPanel("processes", { id: ev.process.id });
    else if (ev.type === "changed" && ev.open) ark.openPanel("browser", { locator: ev.open });
  });
}
