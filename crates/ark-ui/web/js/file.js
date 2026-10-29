// A file of the project in its own tab: the same file the agent reads and changes. The host
// says what kind it is (ark-files decides, once); this module picks the view for that kind,
// saves the person's changes as the same edits the agent sends, and follows changes made
// elsewhere (the agent, another agent, a program it ran, another app) by the file's version.
// A stored artifact that is a copy of a project file opens here too, as the file.
//
// A view is `render(el, file, ctx)` returning `{ update(file), focus(), dirty(), flush() }`.
// `ctx.save(edits)` saves and answers the file as saved; `ctx.status(state)` shows saving.

import { h, icon, clear } from "./dom.js";
import { markdown } from "./md.js";
import { sheetView } from "./sheet.js";
import { documentView } from "./document.js";
import { highlight, languageOf, MAX_CHARS } from "./highlight.js";
import { registry } from './ext.js';
import { extensionFileView } from './file-viewer.js';
import { calendarView } from './calendar.js';

/// The view for each kind of file (`ark_files::Kind`). A new kind is one more entry; a new
/// format of an existing kind needs none.
const VIEWS = { text: textView, markdown: markdownView, sheet: sheetView, document: documentView, pdf: pdfView, image: imageView };

export const KIND_ICON = { text: "file", markdown: "doc", sheet: "sheet", document: "doc", pdf: "doc", image: "image", map: "file", model: "box" };

/// Extensions and the kind each reads as, and each kind's name, from the host
/// (`hello.file_kinds`, `hello.kind_nouns`).
const KINDS = new Map();
const NOUNS = new Map();
export function setFileKinds(map, nouns) {
  KINDS.clear();
  for (const [ext, kind] of Object.entries(map || {})) KINDS.set(ext, kind);
  NOUNS.clear();
  for (const [kind, noun] of Object.entries(nouns || {})) NOUNS.set(kind, noun);
}

/// What a kind is called, capitalized for a card ("Spreadsheet", "PDF").
export function kindNoun(kind, path) {
  const known = path === undefined || KINDS.has(extensionOf(path));
  const n = (known && NOUNS.get(kind)) || "file";
  return n[0].toUpperCase() + n.slice(1);
}

/// The kind of a file from its name, before it is read.
function extensionOf(path) {
  return String(path).split("/").pop().split(".").slice(1).pop()?.toLowerCase();
}

export function kindOf(path) {
  return KINDS.get(extensionOf(path)) || "text";
}

// A navigation bookmark, never file content or permission. The host still authorizes Retry.
const RECOVERY_KEY = 'ark-file-recovery';
export function fileRecovery(location, path, action) {
  try {
    const raw = sessionStorage.getItem(RECOVERY_KEY);
    const saved = raw && raw.length <= 8192 ? JSON.parse(raw) : null;
    const same = saved?.version === 1 && ['runtime', 'project', 'session'].every(k =>
      typeof location?.[k] === 'string' && saved.location?.[k] === location[k]);
    if (action === 'remember') {
      if (typeof path !== 'string' || path.length > 4096 || !['map', 'model'].includes(kindOf(path)) ||
          !['runtime', 'project', 'session'].every(k => typeof location?.[k] === 'string' && location[k].length > 0 && location[k].length <= 512)) {
        sessionStorage.removeItem(RECOVERY_KEY);
        return;
      }
      const route = Object.fromEntries(['runtime', 'project', 'session'].map(k => [k, location[k]]));
      const bookmark = JSON.stringify({ version: 1, location: route, path });
      if (bookmark.length <= 8192) sessionStorage.setItem(RECOVERY_KEY, bookmark);
      else sessionStorage.removeItem(RECOVERY_KEY);
    } else if (action === 'clear') {
      if (same && saved.path === path) sessionStorage.removeItem(RECOVERY_KEY);
    } else {
      if (same && typeof saved.path === 'string' && saved.path.length <= 4096 && ['map', 'model'].includes(kindOf(saved.path))) return saved.path;
      sessionStorage.removeItem(RECOVERY_KEY);
    }
  } catch { try { sessionStorage.removeItem(RECOVERY_KEY); } catch {} }
  return null;
}

export function fileView(el, path, ark, params) {
  el.classList.add("fv");
  const calendar = /\.calendar\.json$/i.test(path);
  const s = { path, file: null, view: null, version: null, busy: 0, offs: [], alive: true, ctl: null, change: null, source: false, loading: 0, recovering: !!params?.recover };
  const status = h("span.fv-status", { "aria-live": "polite", role: "status" });
  const show = (state) => {
    status.dataset.state = state || "";
    status.textContent = { saving: "Saving", saved: "Saved", error: "Not saved" }[state] || "";
    status.title = status.textContent;
    if (state === "saved") {
      clearTimeout(s.statusTimer);
      s.statusTimer = setTimeout(() => { status.dataset.state = ""; status.textContent = ""; }, 1400);
    }
  };
  const sourceButton = h('button', { type: 'button', hidden: !calendar && kindOf(path) !== 'map', onclick: async (event) => {
      if (s.ctl?.dirty?.()) await s.ctl.flush?.();
      if (s.ctl?.dirty?.()) return;
      s.source = !s.source; event.currentTarget.textContent = s.source ? calendar ? 'Calendar' : kindOf(path) === 'map' ? 'Map' : 'Model' : 'Source'; await load(true);
    } }, 'Source');
  const tools = h("div.fv-tools", status, sourceButton,
    ark.isLocal() && kindOf(path) !== "text"
      ? h("button.icon-btn", { type: "button", title: "Open in its app", "aria-label": "Open in its app", onclick: () => ark.call("file_open", { path: s.path }).catch((e) => ark.toast(e.message)) }, icon("external"))
      : null);
  const body = h("div.fv-body");
  el.append(tools, body);

  const ctx = {
    ark,
    tools,
    accept: async (meta) => (await ark.call('file_probe', { path: s.path })).change === meta.change,
    loaded: (meta) => { s.version = meta.version; s.change = meta.change; queueMicrotask(check); },
    status: show,
    // A view that saves on its own path (text) tells which version it wrote.
    saved: (version) => (s.version = version),
    focusChanged: () => ark.reportFocus(),
    async save(edits, { base = true } = {}) {
      s.busy++;
      show("saving");
      try {
        const file = await ark.call("file_edit", { path: s.path, edits, base: base ? s.version : undefined });
        s.version = file.version;
        s.file = file;
        show("saved");
        return file;
      } catch (e) {
        show("error");
        if (/changed since it was read/.test(e.message)) {
          await load(true);
          ark.toast("Reloaded with the latest changes");
          return null;
        }
        ark.toast(e.message);
        throw e;
      } finally {
        s.busy--;
      }
    },
  };

  async function load(force) {
    s.recovering = false;
    const request = ++s.loading;
    let file;
    try {
      file = await ark.call("file_read", { path: s.path, ...(s.source ? { source: true } : {}) });
    } catch (e) {
      if (!s.alive || request !== s.loading) return;
      if (s.ctl) { status.textContent = e.message; status.dataset.state = 'error'; return; }
      return clear(body).append(h("div.wb-empty", e.message,
        h('button', { type: 'button', onclick: () => load(true) }, 'Retry')));
    }
    if (!s.alive || request !== s.loading) return;
    if (!force && file.version && file.version === s.version) return;
    s.version = file.version;
    s.file = file;
    sourceButton.hidden = !(calendar || s.source || kindOf(path) === 'map' || file.source_available);
    const kind = file.text == null && !file.kind ? null : file.kind || "text";
    if (!kind || (kind === "text" && file.text == null)) {
      s.ctl?.dispose?.();
      s.ctl = null;
      return clear(body).append(h("div.wb-empty", `${file.path.split("/").pop()} is ${file.why || "not shown here"}.`));
    }
    const viewKey = calendar && !s.source && kind === 'text' ? 'calendar' : kind;
    if (s.ctl && s.viewKey === viewKey && s.ctl.update) return s.ctl.update(file);
    s.ctl?.dispose?.();
    s.kind = kind;
    s.viewKey = viewKey;
    el.prepend(tools);
    clear(body);
    const viewer = !s.source && [...registry.fileViewers.values()].find(v => v.kinds.includes(kind));
    const supported = file.snapshot_supported ?? ark.fileSnapshots?.();
    if (viewKey === 'calendar') s.ctl = calendarView(body, file);
    else if (viewer && supported) s.ctl = extensionFileView(body, file, ctx, viewer);
    else if (VIEWS[kind]) s.ctl = VIEWS[kind](body, file, ctx, params) || {};
    else if (kind === 'map' && file.text != null) { s.kind = 'text'; s.ctl = textView(body, file, ctx, params); }
    else {
      clear(body).append(h('div.wb-empty', supported ? 'No viewer is available for this file' : 'This file cannot be shown on this computer'));
      s.ctl = {};
    }
    ark.reportFocus();
  }

  /// Someone else may have changed the file: read it again unless the person has unsaved
  /// changes (their save then meets the new version and reloads).
  async function check() {
    if (!s.alive || s.recovering || s.busy || s.ctl?.dirty?.()) return;
    try {
      if (!s.source && ['map', 'model'].includes(s.kind) && (s.file?.snapshot_supported ?? ark.fileSnapshots?.())) {
        const probe = await ark.call('file_probe', { path: s.path });
        if (s.change && probe.change !== s.change) await load(true);
        return;
      }
      const { version } = await ark.call("file_version", { path: s.path });
      if (version !== s.version) await load(false);
    } catch {}
  }
  const wrote = (files) => (files || []).includes(s.path) && setTimeout(check, 60);
  s.offs.push(ark.on("ark:event", (m) => m.event?.type === "tool_result" && wrote(m.event.files)));
  s.offs.push(ark.on("ark:orch", (m) => wrote(m.activity?.files)));
  s.offs.push(ark.on("ark:files_changed", (m) => wrote(m.paths)));
  const onFocus = () => check();
  addEventListener("focus", onFocus);
  if (s.recovering) body.append(h('div.wb-empty', 'The file view was interrupted',
    h('button', { type: 'button', onclick: () => load(true) }, 'Retry')));
  else load(true);

  return {
    focus: () => {
      const base = { surface: { kind_tag: s.kind || "file", title: s.path.split("/").pop(), path: s.path } };
      try {
        const f = s.ctl?.focus?.();
        if (f?.selection) base.selection = f.selection;
      } catch {}
      return base;
    },
    update: (p) => p?.locator && s.ctl?.reveal?.(p.locator),
    refresh: () => check(),
    dispose() {
      s.alive = false;
      s.loading++;
      s.ctl?.flush?.();
      s.ctl?.dispose?.();
      removeEventListener("focus", onFocus);
      for (const off of s.offs) off();
      ark.resource(ark.resourceRef("file", s.path, ark.location), "unsubscribe").catch(() => {});
    },
  };
}

// ---- text --------------------------------------------------------------------------------------

/// Text, as before: a plain editor, saved with ⌘S or Save.
function textView(el, file, ctx, params) {
  const { ark } = ctx;
  const s = { text: file.text, dirty: false, line: 1, version: file.version, path: file.path };
  const name = file.path.split("/").pop();
  // Code gets colours: a layer behind the textarea, whose own text is transparent, so typing,
  // selection and undo stay the textarea's. Lines do not wrap there, so both lay out alike.
  // Any other file is the plain editor, wrapping as before.
  const code = languageOf(file.path) != null;
  const area = h("textarea.wb-editor", { spellcheck: "false", "aria-label": name, ...(code ? { wrap: "off" } : {}) });
  area.readOnly = !!file.read_only;
  const text = h("code");
  const body = code ? h("div.wb-code", h("pre.wb-hl", { "aria-hidden": "true" }, text), area) : area;
  // The layer moves by the textarea's scroll offsets instead of scrolling itself, so it cannot
  // stop short where the textarea's scroll bars make its own range larger.
  const sync = () => {
    text.style.transform = `translate(${-area.scrollLeft}px, ${-area.scrollTop}px)`;
  };
  // Over MAX_CHARS the layer goes and the textarea shows its own text, so a huge file costs
  // nothing per keystroke.
  const draw = () => {
    const big = area.value.length > MAX_CHARS;
    body.classList.toggle("wb-plain", big);
    text.innerHTML = big ? "" : highlight(area.value, s.path);
    sync();
  };
  // At most one repaint per frame, however fast the typing.
  let frame = 0;
  const paint = () => {
    if (!code || frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      draw();
    });
  };
  const save = h("button.wb-save", { type: "button", disabled: true, onclick: () => store() }, "Save");
  const state = h("span.wb-status");
  const mark = () => {
    s.dirty = area.value !== s.text;
    save.disabled = !s.dirty;
    state.textContent = s.dirty ? "Edited" : "";
  };
  const where = () => {
    s.line = area.value.slice(0, area.selectionStart).split("\n").length;
    ctx.focusChanged();
  };
  async function store() {
    if (file.read_only || !s.dirty) return;
    try {
      const r = await ark.call("file_write", { path: s.path, text: area.value, base: s.version });
      s.version = r.version;
      ctx.saved(r.version);
      s.text = area.value;
      mark();
      ark.toast(`Saved ${name}`);
    } catch (e) {
      ark.toast(e.message);
    }
  }
  area.value = file.text;
  if (code) draw();
  area.addEventListener("input", () => {
    mark();
    paint();
  });
  if (code) area.addEventListener("scroll", sync);
  area.addEventListener("click", where);
  area.addEventListener("keyup", where);
  area.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") { e.preventDefault(); e.stopPropagation(); store(); }
  });
  el.append(h("div.wb-bar", h("span.wb-file-name", file.path), h("span.spacer"), state, file.read_only ? null : save, ctx.tools), body);
  if (params?.locator && /^\d+$/.test(params.locator)) goToLine(area, Number(params.locator));
  return {
    focus: () => ({ selection: { line: s.line } }),
    dirty: () => s.dirty,
    update(f) {
      if (s.dirty) return;
      s.text = area.value = f.text;
      s.version = f.version;
      mark();
      paint();
    },
    reveal: (loc) => /^\d+$/.test(loc) && goToLine(area, Number(loc)),
  };
}

function goToLine(area, n) {
  const lines = area.value.split("\n");
  const at = lines.slice(0, Math.max(0, n - 1)).join("\n").length + (n > 1 ? 1 : 0);
  area.focus();
  area.setSelectionRange(at, at);
}

// ---- Markdown ----------------------------------------------------------------------------------

/// Markdown as a page, with the project's images it links to; Edit shows the text to change.
function markdownView(el, file, ctx, params) {
  let edit = null;
  let page = null;
  let current = file;
  const show = () => {
    revokeAll(el);
    clear(el);
    edit = null;
    const toggle = h("button.btn.small", { type: "button", onclick: () => editing() }, "Edit");
    page = h("div.prose.md-page");
    el.append(h("div.wb-bar", h("span.wb-file-name", current.path), h("span.spacer"), toggle, ctx.tools), page);
    page.innerHTML = markdown(current.text, { files: true });
    for (const img of page.querySelectorAll("img[data-file]")) {
      media(ctx.ark, near(current.path, img.dataset.file))
        .then((m) => {
          keep(el, m.url);
          img.src = m.url;
        })
        .catch(() => img.replaceWith(h("span.md-missing", img.alt || img.dataset.file)));
    }
  };
  const editing = () => {
    clear(el);
    const box = h("div.fv-body");
    el.append(box);
    edit = textView(box, current, ctx, params);
    const bar = box.querySelector(".wb-bar");
    bar?.insertBefore(h("button.btn.small", { type: "button", onclick: () => done() }, "Done"), bar.querySelector(".wb-save"));
  };
  // Back to the page, as the file now is (the person may have saved).
  const done = async () => {
    if (edit?.dirty?.()) return ctx.ark.toast("Save or undo your changes first");
    try {
      current = await ctx.ark.call("file_read", { path: current.path });
    } catch {}
    show();
  };
  show();
  return {
    dirty: () => !!edit?.dirty?.(),
    focus: () => edit?.focus?.() || null,
    update(f) {
      current = f;
      if (edit) edit.update(f);
      else show();
    },
    dispose: () => revokeAll(el),
  };
}

/// `rel` as written in the document at `doc`, as a path from the project folder.
function near(doc, rel) {
  const parts = doc.split("/").slice(0, -1);
  for (const p of rel.split("/")) {
    if (p === "..") parts.pop();
    else if (p && p !== ".") parts.push(p);
  }
  return parts.join("/");
}

// ---- PDFs and images: the file's own bytes -----------------------------------------------------

/// A file's bytes as a URL this page can show (`file_media`). Freed when its view goes.
async function media(ark, path) {
  const m = await ark.call("file_media", { path });
  const bytes = Uint8Array.from(atob(m.data), (c) => c.charCodeAt(0));
  return { url: URL.createObjectURL(new Blob([bytes], { type: m.media_type })), ...m };
}

const URLS = new WeakMap();
function keep(el, url) {
  const list = URLS.get(el) || [];
  list.push(url);
  URLS.set(el, list);
}
function revokeAll(el) {
  for (const u of URLS.get(el) || []) URL.revokeObjectURL(u);
  URLS.delete(el);
}

/// A view that shows the file itself, drawn again when its version changes.
function mediaView(draw) {
  return (el, file, ctx) => {
    let version = null;
    const body = h("div.media-body");
    el.append(h("div.wb-bar", h("span.wb-file-name", file.path), h("span.spacer"), ctx.tools), body);
    const load = async (f) => {
      if (f.version && f.version === version) return;
      version = f.version;
      try {
        const m = await media(ctx.ark, f.path);
        revokeAll(body);
        keep(body, m.url);
        clear(body).append(draw(m, f));
      } catch (e) {
        clear(body).append(h("div.wb-empty", e.message));
      }
    };
    load(file);
    return { update: load, dispose: () => revokeAll(body) };
  };
}

function imageView(el, file, ctx) {
  return mediaView((m, f) => h("img.media-image", { src: m.url, alt: f.name }))(el, file, ctx);
}

/// The PDF's pages, drawn by the browser's own viewer.
function pdfView(el, file, ctx) {
  return mediaView((m, f) => h("iframe.media-pdf", { src: m.url, title: f.name }))(el, file, ctx);
}
