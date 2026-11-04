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
  constructor(app, api, { conversation } = {}) {
    this.app = app;
    this.api = api;
    this.tabsEl = document.getElementById("pane-tabs");
    this.body = document.getElementById("pane-body");
    this.tabs = new Map();
    this.active = null;
    this.width = null;
    this.expanded = false;
    this.conversation = conversation;
    this.conversationMount = null;
    this.workspace = null;
    this.expandButton = h("button.icon-btn", {
      type: "button", hidden: true, "aria-label": "Expand workspace", title: "Expand workspace",
      onclick: () => this.setExpanded(!this.expanded),
    }, icon("external"));
    document.getElementById("close-pane").before(this.expandButton);
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
      wide: !!opts.wide, expandable: !!opts.expandable, api: opts.api || this.api,
      projectScoped: !!opts.projectScoped, closesWithProject: !!opts.closesWithProject, project: opts.project || null,
      onShow: opts.onShow, onClose: opts.onClose,
    };
    this.tabs.set(key, tab);
    this.body.append(view);
    this.renderTabs();
    return tab;
  }

  renderTabs() {
    const current = this.tabs.get(this.active);
    this.expandButton.hidden = !current?.expandable || !this.isOpen;
    this.expandButton.disabled = !!current?.removing;
    const label = this.expanded ? "Restore workspace" : "Expand workspace";
    this.expandButton.setAttribute("aria-label", label);
    this.expandButton.setAttribute("title", label);
    this.expandButton.setAttribute("aria-pressed", String(this.expanded));
    this.app.dataset.tabs = String([...this.tabs.values()].filter(tab => !tab.suspended).length);
    clear(this.tabsEl);
    for (const t of this.tabs.values()) {
      if (t.suspended) continue;
      const selected = t.key === this.active;
      const b = h("button.pane-tab", { type: "button", role: "tab", "aria-selected": String(selected), title: t.title },
        h("span.pane-tab-title", t.title));
      b.addEventListener("click", () => this.show(t.key));
      const close = h("button.pane-tab-close", { type: "button", disabled: !!t.removing, title: `Close ${t.title}`, "aria-label": `Close ${t.title}` }, icon("x"));
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
    const visible = [...this.tabs.values()].filter(tab => !tab.suspended);
    if (!visible.length) return;
    this.app.dataset.pane = "open";
    this.show(visible.some(tab => tab.key === this.active) ? this.active : visible.at(-1).key);
  }

  /// Open (or focus) a tab. `opts.wide` asks for a wider pane, `opts.fill` gives the view
  /// the whole pane body with no padding (editors, terminals, browsers).
  openTab(key, title, ic, make, params, opts) {
    let t = this.tabs.get(key);
    if (t?.removing) {
      return t.removing.then(() => {
        if ((opts?.projectScoped || opts?.closesWithProject) && opts.project !== this.project) return;
        return this.openTab(key, title, ic, make, params, opts);
      });
    }
    if (!t) t = this.add(key, title, ic, make, opts);
    if ((t.projectScoped || t.closesWithProject) && t.project !== this.project) {
      t.suspended = true; t.view.hidden = true; t.view.inert = true; this.renderTabs(); return;
    }
    this.app.dataset.pane = "open";
    this.show(key, params);
  }

  show(key, params) {
    const t = this.tabs.get(key);
    if (!t || t.suspended) return;
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
    if (this.expanded && !t.expandable) this.setExpanded(false);
    this.renderTabs();
    this.syncConversation();
    this.reportFocus();
  }

  setExpanded(value) {
    const tab = this.tabs.get(this.active);
    const next = !!value && this.isOpen && !!tab?.expandable && !tab.removing;
    if (next === this.expanded) return;
    this.expanded = next;
    this.app.dataset.paneExpanded = String(next);
    if (next) this.syncConversation();
    else this.restoreConversation();
    this.renderTabs();
    this.reportFocus();
    dispatchEvent(new Event("resize"));
  }

  setWorkspace(element, focus = null) {
    if (this.workspace?.element !== element) this.restoreConversation();
    this.workspace = element ? { element, focus } : null;
    this.syncConversation();
  }

  restoreConversation() {
    const mounted = this.conversationMount;
    if (!mounted) return;
    const focused = mounted.surface.contains(document.activeElement) ? document.activeElement : null;
    mounted.observer.disconnect();
    mounted.insetObserver.disconnect();
    mounted.events.abort();
    mounted.insetTarget.style.removeProperty("--workspace-composer-inset");
    mounted.insetTarget.style.removeProperty("--workspace-conversation-height");
    if (mounted.anchor.parentNode && mounted.surface.parentNode === mounted.root) mounted.anchor.replaceWith(mounted.surface);
    else mounted.anchor.remove();
    if (mounted.attentionAnchor?.parentNode) mounted.attentionAnchor.replaceWith(mounted.attention);
    mounted.attention?.classList.remove("workspace-conversation-attention");
    mounted.root.remove();
    this.conversationMount = null;
    if (focused?.isConnected && !focused.disabled) focused.focus({ preventScroll: true });
  }

  syncConversation() {
    if (!this.expanded && !this.workspace) return;
    const current = this.conversation?.();
    const tab = this.expanded ? this.tabs.get(this.active) : null;
    const at = tab?.api?.location;
    if (at && current?.location && (at.runtime !== current.location.runtime || at.project !== current.location.project)) {
      this.setExpanded(false); return;
    }
    if (!current?.surface) { this.restoreConversation(); return; }
    if (this.conversationMount?.surface === current.surface && current.surface.parentNode === this.conversationMount.root) {
      this.conversationMount.label.textContent = current.title;
      this.conversationMount.root.setAttribute("aria-label", `Conversation: ${current.title}`);
      return;
    }
    this.restoreConversation();
    const surface = current.surface;
    const anchor = document.createComment("conversation position");
    surface.before(anchor);
    const root = h("section.workspace-conversation", { "aria-label": `Conversation: ${current.title}`, "data-history": "false", "data-composer": "collapsed" });
    const label = h("span.workspace-conversation-title", current.title);
    const toggle = h("button.workspace-conversation-toggle", {
      type: "button", "aria-expanded": "false", "aria-label": "Show conversation",
      onclick: event => {
        if (dragged && event.detail !== 0) { event.preventDefault(); dragged = false; return; }
        dragged = false;
        if (root.dataset.composer === "collapsed") { expandComposer(); return; }
        const open = root.dataset.history !== "true";
        root.dataset.history = String(open);
        toggle.setAttribute("aria-expanded", String(open));
        toggle.setAttribute("aria-label", open ? "Hide conversation" : "Show conversation");
      },
    }, icon("chat"), label, icon("chevron-down"));
    const events = new AbortController();
    let dragged = false, drag = null, moved = false;
    const expandComposer = () => {
      root.dataset.composer = "expanded";
      toggle.setAttribute("aria-label", "Show conversation");
      surface.querySelector("textarea")?.focus({ preventScroll: true });
    };
    const minimize = h("button.workspace-conversation-minimize", {
      type: "button", "aria-label": "Minimize composer", title: "Minimize",
      onclick: () => {
        root.dataset.composer = "collapsed"; root.dataset.history = "false";
        toggle.setAttribute("aria-expanded", "false");
        toggle.setAttribute("aria-label", "Expand composer");
        toggle.focus({ preventScroll: true });
      },
    }, icon("chevron-down"));
    const place = (x, y) => {
      const bounds = root.parentElement.getBoundingClientRect(), box = root.getBoundingClientRect();
      root.style.left = `${Math.max(8, Math.min(x, bounds.width - box.width - 8))}px`;
      root.style.top = `${Math.max(48, Math.min(y, bounds.height - box.height - 8))}px`;
      root.style.bottom = "auto"; root.style.transform = "none"; moved = true;
    };
    toggle.addEventListener("pointerdown", event => {
      if (event.button !== 0 || drag) return;
      const box = root.getBoundingClientRect(), bounds = root.parentElement.getBoundingClientRect();
      drag = { id: event.pointerId, x: event.clientX, y: event.clientY, left: box.left - bounds.left, top: box.top - bounds.top };
      dragged = false; toggle.setPointerCapture(event.pointerId);
    }, { signal: events.signal });
    toggle.addEventListener("pointermove", event => {
      if (!drag || drag.id !== event.pointerId) return;
      const dx = event.clientX - drag.x, dy = event.clientY - drag.y;
      if (!dragged && Math.hypot(dx, dy) < 4) return;
      dragged = true; place(drag.left + dx, drag.top + dy);
    }, { signal: events.signal });
    const finishDrag = event => {
      if (drag?.id !== event.pointerId) return;
      drag = null;
      if (event.type === "pointercancel") dragged = false;
    };
    toggle.addEventListener("pointerup", finishDrag, { signal: events.signal });
    toggle.addEventListener("pointercancel", finishDrag, { signal: events.signal });
    toggle.addEventListener("lostpointercapture", finishDrag, { signal: events.signal });
    toggle.addEventListener("keydown", event => {
      if (!event.altKey || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
      event.preventDefault();
      const box = root.getBoundingClientRect(), bounds = root.parentElement.getBoundingClientRect();
      place(box.left - bounds.left + (event.key === "ArrowLeft" ? -24 : event.key === "ArrowRight" ? 24 : 0),
        box.top - bounds.top + (event.key === "ArrowUp" ? -24 : event.key === "ArrowDown" ? 24 : 0));
    }, { signal: events.signal });
    const focused = surface.contains(document.activeElement) ? document.activeElement : null;
    root.append(toggle, minimize, surface);
    let attentionAnchor = null;
    if (current.attention && !surface.contains(current.attention)) {
      attentionAnchor = document.createComment("approval position");
      current.attention.before(attentionAnchor);
      current.attention.classList.add("workspace-conversation-attention");
      root.append(current.attention);
    }
    const overlay = !this.expanded && this.workspace;
    const insetTarget = overlay ? this.workspace.element : this.body;
    (overlay ? this.workspace.element : document.getElementById("pane")).append(root);
    current.mount?.(root);
    if (focused) toggle.focus({ preventScroll: true });
    const attention = () => {
      const pending = !!surface.querySelector('.approval:not(.decided), .work-question, [data-workspace-attention="true"]') || !!current.attention?.querySelector(".approval:not(.decided),.work-question");
      const running = !!surface.querySelector('.composer[data-running="true"]');
      root.dataset.attention = String(pending);
      if (pending || running) root.dataset.composer = "expanded";
      minimize.disabled = pending || running;
      toggle.disabled = pending;
      toggle.setAttribute("aria-expanded", String(pending || root.dataset.history === "true"));
      toggle.setAttribute("aria-label", pending ? "Conversation needs approval" : root.dataset.composer === "collapsed" ? "Expand composer" : root.dataset.history === "true" ? "Hide conversation" : "Show conversation");
    };
    const observer = new MutationObserver(attention);
    observer.observe(surface, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "data-running", "data-workspace-attention"] });
    if (current.attention && !surface.contains(current.attention)) observer.observe(current.attention, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "hidden"] });
    attention();
    const updateInset = () => {
      if (moved && !drag) {
        const box = root.getBoundingClientRect(), bounds = root.parentElement.getBoundingClientRect();
        place(box.left - bounds.left, box.top - bounds.top);
      }
      const height = root.getBoundingClientRect().height + 32;
      insetTarget.style.setProperty("--workspace-conversation-height", `${Math.ceil(height)}px`);
      if (!overlay) insetTarget.style.setProperty("--workspace-composer-inset", `${Math.ceil(height)}px`);
    };
    const insetObserver = new ResizeObserver(updateInset);
    insetObserver.observe(root); insetObserver.observe(root.parentElement);
    updateInset();
    this.conversationMount = { surface, anchor, root, label, observer, insetObserver, events, attention: current.attention, attentionAnchor, insetTarget };
  }

  /// Tell the agent what the visible tab shows (or that nothing beside the chat is shown).
  reportFocus() {
    const t = this.isOpen && this.active && !this.workspace ? this.tabs.get(this.active) : null;
    let focus = null;
    try { focus = this.workspace?.focus?.() || t?.ctl?.focus?.() || null; } catch (error) { this.api.toast(`Could not read selected work: ${error.message || String(error)}`); }
    const conversation = this.conversation?.();
    const at = t?.api?.location;
    const recipient = conversation?.focus;
    if (this.focusRecipient && this.focusRecipient !== recipient) this.focusRecipient(null);
    this.focusRecipient = recipient || null;
    if (conversation?.focus && (!at || (conversation.location?.runtime === at.runtime && conversation.location?.project === at.project))) {
      conversation.focus(focus);
    } else (t?.api || this.api).setFocus(focus);
  }

  remove(key, { projectChange = false } = {}) {
    const t = this.tabs.get(key);
    if (!t) return;
    if (t.removing) return t.removing;
    if (typeof t.ctl?.beforeRemove === "function") {
      t.removingForProjectChange = projectChange;
      t.view.inert = true;
      const pending = Promise.resolve().then(() => t.ctl.beforeRemove()).then(allowed => {
        if (allowed === true && this.tabs.get(key) === t) this.removeNow(key, t);
        return allowed === true;
      }).catch(error => {
        console.error("Cannot close panel:", error);
        t.api.toast?.(`Could not close ${t.title}: ${error?.message || String(error)}`);
        return false;
      }).finally(() => {
        if (this.tabs.get(key) === t) {
          t.removing = null;
          t.removingForProjectChange = false;
          if ((t.projectScoped || t.closesWithProject) && t.project === this.project) t.suspended = false;
          t.view.inert = !!t.suspended;
          this.renderTabs();
        }
      });
      t.removing = pending;
      this.renderTabs();
      return pending;
    }
    this.removeNow(key, t);
  }

  removeNow(key, t) {
    if (this.tabs.get(key) !== t) return;
    if (this.active === key) this.setExpanded(false);
    t.onClose?.();
    try { t.ctl?.dispose?.(); } catch (error) { console.error("Cannot dispose panel:", error); }
    t.view.remove();
    this.tabs.delete(key);
    if (this.active === key) this.active = [...this.tabs.values()].filter(tab => !tab.suspended).at(-1)?.key || null;
    if (this.active) this.show(this.active);
    else this.close();
    this.renderTabs();
  }

  close() {
    this.setExpanded(false);
    const t = this.active ? this.tabs.get(this.active) : null;
    this.app.dataset.pane = "closed";
    t?.onClose?.();
    this.reportFocus();
  }

  toggle() {
    if (this.isOpen) this.close();
    else this.open();
  }

  /// A new session does not rebind or discard open resources. Each tab retains the location it
  /// captured when it opened and can keep operating after the window changes chats. A tab bound
  /// to its project (a Terminal, a Browser page) closes when the window moves to another one.
  reset(project) {
    if (this.project !== project) this.setExpanded(false);
    this.project = project;
    for (const [key, tab] of [...this.tabs]) {
      if (!(tab.projectScoped || tab.closesWithProject)) continue;
      tab.suspended = tab.project !== project || !!(tab.removing && tab.removingForProjectChange);
      tab.view.inert = tab.suspended || !!tab.removing;
      if (tab.suspended) tab.view.hidden = true;
    }
    if (this.active && this.tabs.get(this.active)?.suspended) {
      this.active = [...this.tabs.values()].filter(tab => !tab.suspended).at(-1)?.key || null;
      if (this.active) this.show(this.active);
      else this.close();
    }
    this.renderTabs();
    for (const [key, tab] of [...this.tabs]) if (tab.project !== project && (tab.projectScoped || tab.closesWithProject)) this.remove(key, { projectChange: true });
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
