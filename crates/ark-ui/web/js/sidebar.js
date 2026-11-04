// Projects and their chats. A project is a folder; its chats are listed beneath it. The current
// project is unfolded, the others fold away and unfold with their chevron. Everything comes from
// the host (`projects`, `sessions`); nothing is kept here that could drift from the store, except
// which projects the person folded or unfolded.

import { h, icon, clear, ago, clip, touch, $, chatAvatar } from "./dom.js";
import { bindContextMenu } from "./context-menu.js";
import { registry } from "./ext.js";
import { SETTINGS } from "./settings.js";
import { uploadFolder } from "./drop.js";

// Chats listed per project before "Show more".
const SHOWN = 8;
const HIDDEN_PROJECTS = "ark-hidden-projects";

function storedHiddenProjects() {
  try {
    const entries = JSON.parse(localStorage.getItem(HIDDEN_PROJECTS) || "[]");
    return new Map(Array.isArray(entries)
      ? entries.filter((entry) => entry && typeof entry.id === "string").map((entry) => [entry.id, entry.name || "Project"])
      : []);
  } catch {
    return new Map();
  }
}

export class Sidebar {
  constructor(api) {
    this.api = api;
    this.tree = $("#projects");
    this.status = $("#status-ext");
    this.top = $("#sidebar-ext");
    this.settings = $("#open-settings");
    this.gen = 0;
    this.foldRevision = 0;
    this.projectSections = [];
    this.disclosures = new WeakMap();
    this.projectChats = new WeakMap();
    // Project id -> unfolded (true) or folded (false), where the person chose; else the
    // current project is unfolded and the others folded.
    this.folds = {};
    this.current = null;
    this.all = new Set();
    // This is presentation state only. Hiding a project never changes the host's project,
    // folder or chats, and can be undone from the Add project menu.
    this.hidden = storedHiddenProjects();
    // Chats running a turn now, and their projects (`working` from the host).
    this.working = new Set();
    this.workingProjects = new Set();
    this.attention = [];
    this.online = true;
    $("#new-session").addEventListener("click", () => api.newSession());
    $("#add-project").addEventListener("click", (e) => this.addMenu(e.currentTarget));
    this.settings.addEventListener("click", () => api.openSettings());
  }

  /// Mark the chats that are working; a folded project shows it on its row.
  setWorking(list) {
    const ids = new Set(list.map((w) => w.session));
    const same = ids.size === this.working.size && [...ids].every((x) => this.working.has(x));
    this.working = ids;
    this.workingProjects = new Set(list.map((w) => w.project));
    if (!same) this.refresh();
  }

  setAttention(list) {
    this.attention = [...new Map(list.map((item) => [item.id, item])).values()];
    this.refresh();
  }

  setOnline(online) {
    this.online = online;
    for (const mark of this.tree.querySelectorAll(".session-progress, .working-dot")) mark.hidden = !online;
  }

  unfolded(p) {
    return this.folds[p.id] ?? p.current;
  }

  async fold(p, open, animate = true) {
    ++this.foldRevision;
    if (open === p.current) delete this.folds[p.id];
    else this.folds[p.id] = open;
    const group = this.tree.querySelector(`.project[data-id="${CSS.escape(p.id)}"]`);
    if (!group) return;
    const previous = this.disclosures.get(group);
    const height = group.getBoundingClientRect().height;
    previous?.animation?.cancel();
    const state = {};
    this.disclosures.set(group, state);
    const row = group.querySelector(".project-row");
    group.dataset.instant = String(!animate);
    row.setAttribute("aria-expanded", String(open));
    row.querySelector(".project-name")?.setAttribute("aria-expanded", String(open));
    row.querySelector(".project-chev").setAttribute("aria-label", open ? "Fold" : "Unfold");
    row.querySelector(".session-attention, .working-dot")?.remove();
    const badge = this.projectBadge(p, open, this.projectChats.get(group) || []);
    if (badge) row.insertBefore(badge, row.querySelector(".project-more"));
    if (open && !group.querySelector(".chats, .project-section")) {
      let sessions;
      try { sessions = await this.api.call("sessions", { project: p.id }); }
      catch (error) {
        if (group.isConnected && this.disclosures.get(group) === state) {
          await this.fold(p, false, false);
          this.api.toast(error.message || String(error));
        }
        return;
      }
      if (!group.isConnected || this.disclosures.get(group) !== state || !this.unfolded(p)) return;
      this.mountProject(group, p, sessions);
    }
    if (!open && [...group.children].slice(1).some(el => el.contains(document.activeElement))) row.querySelector(".project-name")?.focus({ preventScroll: true });
    for (const child of [...group.children].slice(1)) {
      child.inert = !open;
      child.setAttribute("aria-hidden", String(!open));
    }
    const finish = () => {
      if (this.disclosures.get(group) !== state) return;
      state.animation = null;
      if (!open) {
        this.disposeProject(group);
        [...group.children].slice(1).forEach(el => el.remove());
      }
      group.style.overflow = "";
    };
    const target = open ? group.scrollHeight : row.getBoundingClientRect().height;
    if (!animate || matchMedia("(prefers-reduced-motion: reduce)").matches || height === target) {
      finish();
      return;
    }
    group.style.overflow = "clip";
    state.animation = group.animate([{ height: `${height}px` }, { height: `${target}px` }], {
      duration: open ? 200 : 160, easing: "cubic-bezier(0.23, 1, 0.32, 1)",
    });
    state.animation.onfinish = finish;
  }

  projectBadge(p, open, sessions = []) {
    if (open) return null;
    const waiting = this.attention.filter(item => item.project?.id === p.id || sessions.some(session => session.id === item.session)).length;
    if (waiting) return h("span.session-attention", { title: "Needs you", "aria-label": `${waiting} pending decision${waiting === 1 ? "" : "s"}` }, icon("alert"), String(waiting));
    return this.workingProjects.has(p.id) ? h("span.working-dot", { title: "Working", "aria-label": "Working", hidden: !this.online }) : null;
  }

  disposeProject(group) {
    const owned = this.projectSections.filter(section => section.group === group);
    this.projectSections = this.projectSections.filter(section => section.group !== group);
    for (const { controller } of owned) {
      try { controller.dispose?.(); } catch (error) { console.error(error); }
    }
  }

  mountProject(group, p, sessions) {
    this.projectChats.set(group, sessions);
    const chats = this.chats(p, sessions);
    if (chats) group.append(chats);
    if (!p.current) return;
    for (const section of registry.projectSections.values()) {
      const el = h("div.project-section", { role: "group", "aria-label": section.label, "data-id": section.id });
      group.insertBefore(el, chats);
      try {
        const controller = section.render(el, { project: p }, window.ark);
        if (controller) this.projectSections.push({ group, controller });
      } catch (error) {
        console.error(error);
        el.remove();
      }
    }
  }

  saveHidden() {
    try {
      localStorage.setItem(HIDDEN_PROJECTS, JSON.stringify([...this.hidden].map(([id, name]) => ({ id, name }))));
      return true;
    } catch {
      return false;
    }
  }

  hide(p) {
    this.hidden.set(p.id, p.name);
    if (!this.saveHidden()) {
      this.hidden.delete(p.id);
      this.api.toast("Could not save the sidebar preference");
      return;
    }
    this.refresh();
    this.api.toast(`${p.name} was removed from the sidebar`);
  }

  restore(id) {
    const name = this.hidden.get(id) || "Project";
    this.hidden.delete(id);
    if (!this.saveHidden()) {
      this.hidden.set(id, name);
      this.api.toast("Could not save the sidebar preference");
      return;
    }
    this.refresh();
    this.api.toast(`${name} was restored to the sidebar`);
  }

  async addProject(params) {
    if (!await this.api.addProject(params)) return;
    await this.restoreOpenedProject();
  }

  async restoreOpenedProject() {
    try {
      const opened = (await this.api.call("projects")).find((p) => p.current);
      if (opened && this.hidden.has(opened.id)) return this.restore(opened.id);
    } catch {}
    this.refresh();
  }

  async refresh(currentId) {
    if (currentId !== undefined) this.currentId = currentId;
    const gen = ++this.gen;
    const foldRevision = this.foldRevision;
    let projects;
    try {
      projects = await this.api.call("projects");
    } catch {
      return; // busy while a turn runs; the list refreshes when it ends
    }
    if (gen !== this.gen) return;
    // A project that stops being current stays unfolded. Project expansion is independent, so
    // moving between projects never turns the sidebar into a one-open accordion.
    const current = projects.find((p) => p.current)?.id || null;
    if (current !== this.current) {
      if (this.current && this.folds[this.current] === undefined) this.folds[this.current] = true;
      this.current = current;
    }
    // Keep the saved labels current while a project is hidden. Drop old entries if its host no
    // longer lists the project, so a future project cannot inherit the old presentation state.
    const known = new Set(projects.map((p) => p.id));
    for (const id of this.hidden.keys()) if (!known.has(id)) this.hidden.delete(id);
    projects.forEach((p) => { if (this.hidden.has(p.id)) this.hidden.set(p.id, p.name); });
    this.saveHidden();
    const visible = projects.filter((p) => !this.hidden.has(p.id));
    const locateQuestions = this.attention.some((item) => item.session && !item.project?.id);
    const chats = await Promise.all(visible.map((p) =>
      this.unfolded(p) || locateQuestions ? this.api.call("sessions", { project: p.id }).catch(() => []) : null));
    // A newer refresh started meanwhile; it draws.
    if (gen !== this.gen) return;
    if (foldRevision !== this.foldRevision) return this.refresh();
    const active = document.activeElement;
    const focusedChat = active?.matches(".session") ? active.dataset.id : null;
    const focusedProject = active?.closest(".project")?.dataset.id;
    const focusedSection = active?.closest(".project-section")?.dataset.id;
    const focusedKey = active?.dataset.focusKey;
    const focusedControl = [".project-name", ".project-chev", ".project-more", ".project-new-chat"].find((selector) => active?.matches(selector));
    const sections = this.projectSections;
    this.projectSections = [];
    for (const { controller } of sections) {
      try { controller.dispose?.(); } catch (e) { console.error(e); }
    }
    clear(this.tree);
    visible.forEach((p, i) => {
      const group = this.project(p, chats[i]);
      this.tree.append(group);
      if (this.unfolded(p)) this.mountProject(group, p, chats[i] || []);
    });
    if (focusedChat) this.tree.querySelector(`.session[data-id="${CSS.escape(focusedChat)}"]`)?.focus({ preventScroll: true });
    else if (focusedProject && focusedControl) this.tree.querySelector(`.project[data-id="${CSS.escape(focusedProject)}"] ${focusedControl}`)?.focus({ preventScroll: true });
    else if (focusedProject && focusedSection && focusedKey) this.tree.querySelector(`.project[data-id="${CSS.escape(focusedProject)}"] .project-section[data-id="${CSS.escape(focusedSection)}"] [data-focus-key="${CSS.escape(focusedKey)}"]`)?.focus({ preventScroll: true });
  }

  project(p, sessions) {
    const open = this.unfolded(p);
    const row = h("div.project-row", {
      "aria-expanded": String(open), "aria-current": p.current ? "true" : null,
      title: p.workspace || "No folder yet",
    },
      h("button.project-chev", { type: "button", tabindex: "-1", "aria-label": open ? "Fold" : "Unfold", onclick: (e) => { e.stopPropagation(); this.fold(p, !this.unfolded(p), e.detail !== 0); } }, icon("chevron-right")),
      h("button.project-name", { type: "button", "aria-expanded": String(open), "aria-current": p.current ? "true" : null }, p.name),
      this.projectBadge(p, open, sessions || []),
      h("button.icon-btn.small.project-more", { type: "button", title: "Project", "aria-label": `${p.name}: options`, onclick: (e) => { e.stopPropagation(); this.projectMenu(e.currentTarget, p); } }, icon("more")),
      h("button.icon-btn.small.project-new-chat", { type: "button", title: `New chat in ${p.name}`, "aria-label": `New chat in ${p.name}`, onclick: (e) => { e.stopPropagation(); this.newChat(p); } }, icon("plus")));
    // The current project folds; another one opens where it left off.
    const pick = (animate) => (p.current ? this.fold(p, !this.unfolded(p), animate) : this.openProject(p));
    row.addEventListener("click", (e) => pick(e.detail !== 0));
    row.addEventListener("dblclick", (e) => { e.preventDefault(); this.renameProject(row, p); });
    const group = h("div.project", { role: "group", "data-id": p.id }, row);
    this.projectChats.set(group, sessions || []);
    const projectRow = group.querySelector(".project-row");
    bindContextMenu(projectRow, () => [
      { label: "New chat", run: () => this.newChat(p) },
      { label: "Rename project", run: () => this.renameProject(projectRow, p) },
      { label: "Remove from sidebar", run: () => this.hide(p) },
    ]);
    return group;
  }

  chats(p, sessions) {
    // A chat shows once it has a turn, or while its first one runs.
    const shown = sessions.filter((s) => s.turns > 0 || s.working || this.working.has(s.id) || this.attention.some((item) => item.session === s.id));
    if (!shown.length) return null;
    const box = h("div.chats", { role: "group", "aria-label": "Chats" });
    const all = this.all.has(p.id);
    (all ? shown : shown.slice(0, SHOWN)).forEach((s) => box.append(this.chat(s)));
    if (shown.length > SHOWN) {
      box.append(h("button.chats-more", { type: "button", onclick: () => { all ? this.all.delete(p.id) : this.all.add(p.id); this.refresh(); } },
        all ? "Show less" : `Show ${shown.length - SHOWN} more`));
    }
    return box;
  }

  chat(s) {
    const title = s.title || "Untitled";
    const waiting = this.attention.some((item) => item.session === s.id);
    const row = h("div.session", {
      role: "button", tabindex: "0", "data-id": s.id, "aria-current": s.id === this.currentId ? "true" : null,
    },
      chatAvatar(s.participants),
      h("span.session-title", clip(title, 60)),
      // A chat that runs on another computer: its mark, muted while that computer is away.
      s.place ? h("span.session-place", { title: `Runs on ${s.place.name}`, "data-state": s.place.state }, icon("server")) : null,
      // A chat working in the background: a spinner where its time would be.
      waiting ? h("span.session-attention", { title: "Needs you" }, icon("alert"), "Needs you")
        : s.working || this.working.has(s.id)
        ? h("span.session-progress", { role: "status", hidden: !this.online }, h("span.session-working", { "aria-hidden": "true" }), "Working")
        : h("span.session-time", ago(s.updated_at)));
    if (s.participants?.length) row.setAttribute("aria-description", `Assigned agents: ${s.participants.join(", ")}`);
    row.title = `${title}\n${s.turns} turn${s.turns === 1 ? "" : "s"} · double-click to rename`;
    row.addEventListener("click", () => this.api.openSession(s.id));
    row.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); this.api.openSession(s.id); } });
    row.addEventListener("dblclick", () => this.inline(row, ".session-title", s.title || "", "Chat title", (title) => this.api.call("rename_session", { id: s.id, title })));
    bindContextMenu(row, () => [
      { label: "Open chat", run: () => this.api.openSession(s.id) },
      { label: "Rename chat", run: () => this.inline(row, ".session-title", s.title || "", "Chat title", title => this.api.call("rename_session", { id: s.id, title })) },
    ]);
    return row;
  }

  /// Mark the view the main column shows (none: the conversation).
  markView(id) {
    this.view = id;
    this.tree.dataset.dim = String(!!id);
    this.settings.setAttribute("aria-current", String(id === SETTINGS));
  }

  openProject(p) {
    return this.api.openProject(p.id);
  }

  newChat(p) {
    return this.api.newSession(p.id);
  }

  renameProject(row, p) {
    this.inline(row, ".project-name", p.name, "Project name", (name) => this.api.call("rename_project", { id: p.id, name }));
  }

  /// Edit a row's label in place; `save` gets the new text when it changed.
  inline(row, sel, value, label, save) {
    const el = row.querySelector(sel);
    if (!el) return;
    const input = h("input.session-rename", { value, "aria-label": label });
    input.addEventListener("click", (e) => e.stopPropagation());
    el.replaceWith(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (keep) => {
      if (done) return;
      done = true;
      const text = input.value.trim();
      if (keep && text && text !== value) {
        try {
          await save(text);
        } catch (e) {
          this.api.toast(e.message);
        }
      }
      this.refresh();
    };
    input.addEventListener("blur", () => finish(true));
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") finish(true);
      if (e.key === "Escape") finish(false);
    });
  }

  projectMenu(anchor, p) {
    const item = (label, run) => h("button.menu-item", { type: "button", onclick: () => { menu.close(); run(); } }, h("span", label));
    const menu = this.api.popover(anchor, [
      h("div.menu-sub.pad", { title: p.workspace || "" }, p.workspace ? tail(p.workspace, 44) : "No folder yet"),
      item("New chat", () => this.newChat(p)),
      item("Rename", () => {
        const row = this.tree.querySelector(`.project[data-id="${p.id}"] .project-row`);
        if (row) this.renameProject(row, p);
      }),
      item(p.workspace ? "Change folder…" : "Choose folder…", async () => {
        const path = await this.folder(anchor, "Choose the folder this project works in");
        if (!path) return;
        try {
          await this.api.call("project_folder", { id: p.id, path });
        } catch (e) {
          this.api.toast(e.message);
        }
      }),
      h("div.menu-sep"),
      item("Remove from sidebar", () => this.hide(p)),
      // The app's own actions on a project (`ark.action({ where: "project" })`).
      ...registry.actions
        .filter((a) => {
          if (a.where !== "project") return false;
          try { return !a.when || a.when({ project: p }); } catch { return false; }
        })
        .map((a) => item(a.label, async () => {
          try { await a.run({ project: p }, window.ark); } catch (e) { this.api.toast(e.message || String(e)); }
        })),
    ]);
  }

  addMenu(anchor) {
    const name = h("input.menu-input", { placeholder: "New project…", "aria-label": "New project name" });
    const item = (label, run) => h("button.menu-item", { type: "button", onclick: () => { menu.close(); run(); } }, h("span", label));
    // A folder on the runtime's computer: picked in the window, typed in a browser there. A
    // folder on this device, wherever the runtime runs: sent (a browser cannot name a path).
    const hidden = [...this.hidden];
    const menu = this.api.popover(anchor, [
      (this.api.canPickFolder || this.api.isLocal()) && item("Choose a folder…", async () => {
        const path = await this.folder(anchor, "Open a folder as a project");
        if (path) this.addProject({ path });
      }),
      !this.api.canPickFolder && item("Upload a folder…", () => uploadFolder({
        ...this.api,
        addProject: (params) => this.addProject(params),
        openProject: async (id) => {
          if (await this.api.openProject(id)) await this.restoreOpenedProject();
        },
      })),
      h("div.menu-sep"),
      ...hidden.map(([id, projectName]) => item(`Restore ${projectName}`, () => this.restore(id))),
      hidden.length ? h("div.menu-sep") : null,
      name,
    ].filter(Boolean));
    name.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && name.value.trim()) {
        menu.close();
        this.addProject({ name: name.value.trim() });
      }
    });
    if (!touch()) name.focus();
  }

  /// A folder from the window's picker; where there is none (a browser), a typed path.
  async folder(anchor, title) {
    const picked = await this.api.pickFolder({ title });
    if (picked !== undefined) return picked;
    return new Promise((done) => {
      const input = h("input.menu-input", { placeholder: "/full/path/to/folder", "aria-label": title, spellcheck: "false" });
      const menu = this.api.popover(anchor, [h("div.menu-head", title), input]);
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && input.value.trim()) {
          menu.close();
          done(input.value.trim());
        }
      });
      input.focus();
    });
  }

  /// The app's rows above the projects and status items. The account entry lives in the fixed
  /// rail, and appears once an app has registered a settings section.
  renderExtensions() {
    this.settings.hidden = !registry.settings.size;
    clear(this.top);
    this.top.hidden = !registry.sidebars.length;
    for (const s of registry.sidebars) {
      const el = h("div.sidebar-item", { "data-id": s.id });
      this.top.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
    clear(this.status);
    for (const s of registry.statuses) {
      const el = h("div.status-item", { "data-id": s.id });
      this.status.append(el);
      try { s.render(el, window.ark); } catch (e) { console.error(e); }
    }
  }
}

/// The end of a path, which names the folder: `…/code/project`.
function tail(path, n) {
  return path.length > n ? `…${path.slice(path.length - n + 1)}` : path;
}
