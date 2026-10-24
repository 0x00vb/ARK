// The side pane: room for things worth looking at next to the conversation. An app's panels
// (a terminal, a browser, a viewer), an artifact, or a cited piece of evidence open here as
// tabs; with no tabs the pane is gone. Context, state, usage and traces are commands
// (`/context`, `/state`, `/usage`, `/trace`), answered inline in the conversation.
//
// A tab's controller (what `make` returns) may have `update(params)`, `refresh()`,
// `dispose()` and `focus()`. `focus()` returns what the tab shows, reported to the agent
// while the tab is visible; closing the pane clears it. A `wide` tab gets a wider pane, and
// the person can drag the pane's edge to any width.

import { h, icon, clear, bytes } from "./dom.js";
import { registry } from "./ext.js";
import { markdown } from "./md.js";

export class Pane {
  constructor(app, api) {
    this.app = app;
    this.api = api;
    this.tabsEl = document.getElementById("pane-tabs");
    this.body = document.getElementById("pane-body");
    this.tabs = new Map();
    this.active = null;
    this.width = null;
    document.getElementById("close-pane").addEventListener("click", () => this.close());
    this.grip();
  }

  /// The drag handle on the pane's leading edge. The chosen width is remembered.
  grip() {
    const pane = document.getElementById("pane");
    const handle = h("div.pane-grip", { title: "Drag to resize", "aria-hidden": "true" });
    pane.append(handle);
    try {
      const w = Number(localStorage.getItem("ark-pane-width"));
      if (w > 0) this.setWidth(w);
    } catch {}
    handle.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      handle.setPointerCapture(e.pointerId);
      this.app.classList.add("resizing");
      const move = (m) => this.setWidth(window.innerWidth - m.clientX);
      const up = () => {
        handle.removeEventListener("pointermove", move);
        this.app.classList.remove("resizing");
        try { localStorage.setItem("ark-pane-width", String(this.width)); } catch {}
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", up, { once: true });
    });
    handle.addEventListener("dblclick", () => {
      this.width = null;
      this.app.style.removeProperty("--pw-user");
      delete this.app.dataset.sized;
      try { localStorage.removeItem("ark-pane-width"); } catch {}
      dispatchEvent(new Event("resize"));
    });
  }

  setWidth(px) {
    const max = Math.max(360, window.innerWidth - 420);
    this.width = Math.round(Math.min(Math.max(px, 320), max));
    this.app.style.setProperty("--pw-user", `${this.width}px`);
    const room = Number.parseFloat(getComputedStyle(this.app).getPropertyValue("--pw-room")) || max;
    this.app.style.setProperty("--pw", `${Math.min(this.width, room)}px`);
    this.app.dataset.sized = "";
  }

  get isOpen() {
    return this.app.dataset.pane === "open";
  }

  add(key, title, ic, make, opts = {}) {
    const view = h("div.pane-view", { "data-key": key });
    if (opts.fill) view.classList.add("fill");
    const tab = {
      key, title, icon: ic, make, view, ctl: null, built: false, params: null,
      wide: !!opts.wide, api: opts.api || this.api,
      projectScoped: !!opts.projectScoped, closesWithProject: !!opts.closesWithProject, project: opts.project || null,
      onShow: opts.onShow, onClose: opts.onClose,
    };
    this.tabs.set(key, tab);
    this.body.append(view);
    this.renderTabs();
    return tab;
  }

  renderTabs() {
    this.app.dataset.tabs = String(this.tabs.size);
    clear(this.tabsEl);
    for (const t of this.tabs.values()) {
      const selected = t.key === this.active;
      const b = h("button.pane-tab", { type: "button", role: "tab", "aria-selected": String(selected), title: t.title },
        h("span.pane-tab-title", t.title));
      b.addEventListener("click", () => this.show(t.key));
      const close = h("button.pane-tab-close", { type: "button", title: `Close ${t.title}`, "aria-label": `Close ${t.title}` }, icon("x"));
      close.addEventListener("click", () => this.remove(t.key));
      const menu = typeof t.ctl?.menu === "function" ? h("button.pane-tab-menu", {
        type: "button", "aria-label": `Options for ${t.title}`, "aria-haspopup": "menu",
        onclick: (event) => {
          const items = t.ctl.menu();
          if (items?.length) t.api.popover(event.currentTarget, items);
        },
      }, icon("more")) : null;
      this.tabsEl.append(h("div.pane-tab-item", { "data-selected": String(selected) }, b, menu, close));
    }
  }

  open() {
    if (!this.tabs.size) return;
    this.app.dataset.pane = "open";
    this.show(this.active || [...this.tabs.keys()].pop());
  }

  /// Open (or focus) a tab. `opts.wide` asks for a wider pane, `opts.fill` gives the view
  /// the whole pane body with no padding (editors, terminals, browsers).
  openTab(key, title, ic, make, params, opts) {
    let t = this.tabs.get(key);
    if (!t) t = this.add(key, title, ic, make, opts);
    this.app.dataset.pane = "open";
    this.show(key, params);
  }

  show(key, params) {
    const t = this.tabs.get(key);
    if (!t) return;
    this.active = key;
    t.onShow?.();
    for (const x of this.tabs.values()) x.view.hidden = x.key !== key;
    if (!t.built) {
      t.built = true;
      t.params = params;
      t.ctl = t.make(t.view, params) || {};
      if (t.api.remoteResource) {
        const unavailable = h("div.resource-unavailable", "That computer is not answering");
        t.view.append(unavailable);
        const connection = () => {
          const offline = !t.api.online();
          t.view.toggleAttribute("data-resource-offline", offline);
          unavailable.hidden = !offline;
          for (const child of t.view.children) if (child !== unavailable) child.inert = offline;
          if (!offline) {
            if (typeof t.ctl?.reconnect === "function") t.ctl.reconnect();
            else t.ctl?.refresh?.();
          }
        };
        const off = t.api.on("ark:resource_connection", connection);
        const dispose = t.ctl.dispose?.bind(t.ctl);
        t.ctl.dispose = () => { off(); dispose?.(); };
        connection();
      }
    } else if (params !== undefined && params !== t.params) {
      t.params = params;
      if (t.ctl.update) t.ctl.update(params);
    } else if (t.ctl.refresh) {
      t.ctl.refresh();
    }
    this.app.dataset.wide = String(t.wide);
    this.renderTabs();
    this.reportFocus();
  }

  /// Tell the agent what the visible tab shows (or that nothing beside the chat is shown).
  reportFocus() {
    const t = this.isOpen && this.active ? this.tabs.get(this.active) : null;
    let focus = null;
    try { focus = t?.ctl?.focus?.() || null; } catch {}
    (t?.api || this.api).setFocus(focus);
  }

  remove(key) {
    const t = this.tabs.get(key);
    if (!t) return;
    t.onClose?.();
    try { t.ctl?.dispose?.(); } catch {}
    t.view.remove();
    this.tabs.delete(key);
    if (this.active === key) this.active = [...this.tabs.keys()].pop() || null;
    if (this.active) this.show(this.active);
    else this.close();
    this.renderTabs();
  }

  close() {
    const t = this.active ? this.tabs.get(this.active) : null;
    this.app.dataset.pane = "closed";
    t?.onClose?.();
    (t?.api || this.api).setFocus(null);
  }

  toggle() {
    if (this.isOpen) this.close();
    else this.open();
  }

  /// A new session does not rebind or discard open resources. Each tab retains the location it
  /// captured when it opened and can keep operating after the window changes chats. A tab bound
  /// to its project (a Terminal, a Browser page) closes when the window moves to another one.
  reset(project) {
    for (const [key, tab] of [...this.tabs]) {
      if ((tab.projectScoped || tab.closesWithProject) && tab.project !== project) this.remove(key);
    }
    this.project = project;
  }
}

// ---- helpers -------------------------------------------------------------------------------

function loadInto(el, load, render) {
  let alive = true;
  const run = async () => {
    try {
      const data = await load();
      if (alive) render(data);
    } catch (e) {
      if (alive) clear(el).append(h("div.insp-empty", e.message));
    }
  };
  run();
  return { refresh: run, dispose: () => (alive = false) };
}

const section = (title, ...children) => h("section.insp-section", h("h3", title), ...children);

// ---- evidence and artifacts ------------------------------------------------------------------

export function invocationView(el, api, id) {
  el.classList.add("insp");
  return loadInto(el, () => api.call("invocation", { id }), (inv) => {
    clear(el);
    if (!inv) return el.append(h("div.insp-empty", `No evidence matches ${id}.`));
    el.append(h("div.insp-hero", h("div.hero-title.mono", inv.action), h("div.hero-sub", `inv:${inv.id.slice(0, 8)} · ${inv.status}${inv.finished_at ? ` · ${new Date(inv.finished_at).toLocaleString()}` : ""}`)));
    el.append(section("Input", h("pre.code-block", JSON.stringify(inv.input, null, 2))));
    if (inv.command) el.append(section("Command", h("pre.code-block", inv.command.join(" "))));
    if (inv.error) el.append(h("div.call-err", inv.error));
    el.append(section("Output", h("pre.code-block.output", JSON.stringify(inv.output, null, 2))));
  });
}

/// An artifact with the viewers that match it. A viewer's `render(el, artifact, api, params)`
/// may return `{ focus(), update(params), dispose() }`; `params` is the present directive
/// that opened it (`locator`, `path`, `note`), when there was one. The first matching viewer
/// whose `primary` is not false (or returns true for the artifact) opens by default; with none,
/// the raw view does.
export function artifactView(el, api, artifactId, viewerId, params) {
  el.classList.add("artifact-view");
  const s = { alive: true, artifact: null, viewers: [], current: null, ctl: null, head: null, body: null };
  const dispose = () => { try { s.ctl?.dispose?.(); } catch {} s.ctl = null; };
  const choose = (v, prm) => {
    dispose();
    clear(s.body);
    s.current = v;
    for (const b of s.head.querySelectorAll(".seg")) b.setAttribute("aria-pressed", String(b.dataset.id === (v?.id || "raw")));
    if (v) {
      try { s.ctl = v.render(s.body, s.artifact, api, prm) || null; } catch (e) { s.body.append(h("div.call-err", String(e))); }
    } else {
      genericArtifact(s.body, s.raw);
    }
    api.setFocus(focus());
  };
  const focus = () => {
    try { if (s.ctl?.focus) return s.ctl.focus(); } catch {}
    const a = s.artifact;
    return a ? { surface: { kind_tag: "artifact_viewer", title: a.name, artifact: a.id } } : null;
  };
  api.call("artifact", { id: artifactId, length: 262144 }).then((r) => {
    if (!s.alive) return;
    const a = (s.artifact = r.artifact);
    s.raw = r;
    s.viewers = registry.viewers.filter((v) => {
      try { return v.match(a); } catch { return false; }
    });
    s.head = h("div.art-head", h("div.art-title", a.name), h("div.art-sub", `${a.kind} · ${bytes(r.total)}`));
    s.body = h("div.art-body");
    if (s.viewers.length) {
      s.head.append(h("div.segs", [...s.viewers, null].map((v) =>
        h("button.seg", { "data-id": v?.id || "raw", onclick: () => choose(v) }, v?.label || "Raw"))));
    }
    const actions = registry.actions.filter((x) => {
      if (x.where !== "artifact") return false;
      try { return !x.when || x.when({ artifact: a }); } catch { return false; }
    });
    if (actions.length) {
      s.head.append(h("div.art-actions", actions.map((x) =>
        h("button.btn.small", { onclick: () => x.run({ artifact: a }, api) }, x.icon ? icon(x.icon) : null, x.label))));
    }
    clear(el).append(s.head, s.body);
    const primary = (v) => {
      try { return typeof v.primary === "function" ? v.primary(a) : v.primary !== false; } catch { return false; }
    };
    choose(s.viewers.find((v) => v.id === viewerId) || s.viewers.find(primary) || null, params);
  }).catch((e) => {
    if (s.alive) clear(el).append(h("div.insp-empty", e.message));
  });
  return {
    focus,
    update(p) {
      if (!s.artifact) return;
      const v = s.viewers.find((x) => x.id === p?.viewer) || s.current;
      if (v && v === s.current && s.ctl?.update) {
        s.ctl.update(p);
        api.setFocus(focus());
      } else {
        choose(v, p);
      }
    },
    dispose() {
      s.alive = false;
      dispose();
    },
  };
}

function genericArtifact(body, r) {
  if (r.text != null) {
    const t = r.text;
    let shown = t;
    try {
      if (/^\s*[{[]/.test(t)) shown = JSON.stringify(JSON.parse(t), null, 2);
    } catch {}
    if (r.artifact.kind === "markdown") body.append(h("div.prose", { html: markdown(t) }));
    else body.append(h("pre.code-block.lines", shown));
    if (r.length < r.total) body.append(h("p.insp-note", `Showing the first ${bytes(r.length)} of ${bytes(r.total)}.`));
    return;
  }
  const hex = r.hex || "";
  const lines = [];
  for (let i = 0; i < Math.min(hex.length, 8192); i += 32) {
    const chunk = hex.slice(i, i + 32);
    const off = (r.offset + i / 2).toString(16).padStart(8, "0");
    const pairs = chunk.match(/../g) || [];
    const ascii = pairs.map((p) => { const c = parseInt(p, 16); return c >= 32 && c < 127 ? String.fromCharCode(c) : "."; }).join("");
    lines.push(`${off}  ${pairs.join(" ").padEnd(47)}  ${ascii}`);
  }
  body.append(h("p.insp-note", `Binary, ${bytes(r.total)}. The first ${bytes(Math.min(r.length, 4096))} as hex:`), h("pre.code-block.hex", lines.join("\n")));
}
