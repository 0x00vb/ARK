// The panels every window has, in the side pane: Terminal, Browser and Files, opened from the
// top bar's + menu, a key, or the agent's `present`. They work in the project's folder. An app
// replaces one by registering a panel with the same id.
//
//   terminal  the project's shell (`term_*`); the agent may fill the prompt, never press Enter
//   browser   a page in a frame, with an address bar
//   files     the project's folder: browse, read, edit and save a file (`files`, `file_*`)

import { h, icon, clear } from "./dom.js";

/// What the agent may put at the prompt: one line, no control characters, so it cannot run
/// a command itself (that would skip approval). The person presses Enter.
export const typedCommand = (cmd) => String(cmd ?? "").replace(/[\x00-\x1f\x7f-\x9f]+/g, " ");

const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/// Load a script or stylesheet once.
const loaded = new Map();
function load(src) {
  if (!loaded.has(src)) {
    loaded.set(src, new Promise((ok, fail) => {
      const el = src.endsWith(".css")
        ? h("link", { rel: "stylesheet", href: src })
        : h("script", { src });
      el.onload = ok;
      el.onerror = () => fail(new Error(`could not load ${src}`));
      document.head.append(el);
    }));
  }
  return loaded.get(src);
}

async function xterm() {
  await Promise.all([load("vendor/xterm/xterm.css"), load("vendor/xterm/xterm.js")]);
  await load("vendor/xterm/xterm-addon-fit.js");
  return window.Terminal;
}

// ---- terminal ----------------------------------------------------------------------------------

function terminal(el, params, ark) {
  el.classList.add("wb-terminal");
  const s = { session: null, term: null, fit: null, pending: params?.locator || "", offs: [] };
  const write = (data) => s.session && ark.call("term_write", { session: s.session, data }).catch(() => {});
  const fit = () => { try { s.fit?.fit(); } catch {} };
  const theme = () => ({
    background: css("--pane") || "#181818",
    foreground: css("--text") || "#ececeb",
    cursor: css("--accent") || "#888",
    selectionBackground: "rgba(127,127,127,0.3)",
  });

  (async () => {
    let Terminal;
    try {
      Terminal = await xterm();
    } catch (e) {
      return el.append(h("div.wb-empty", e.message));
    }
    const term = (s.term = new Terminal({
      cursorBlink: true,
      fontSize: 12.5,
      fontFamily: css("--mono") || "ui-monospace, Menlo, monospace",
      theme: theme(),
      scrollback: 5000,
    }));
    s.fit = new window.FitAddon.FitAddon();
    term.loadAddon(s.fit);
    term.open(el);
    fit();
    term.onData(write);
    term.onResize(({ cols, rows }) => s.session && ark.call("term_resize", { session: s.session, cols, rows }).catch(() => {}));
    // Output can arrive before `term_open` answers with the session id; hold it until then.
    const early = [];
    const output = (p) => {
      if (p.exit) return term.write("\r\n\x1b[2m[shell exited]\x1b[0m\r\n");
      term.write(p.data);
      // Type a presented command once the shell has drawn its prompt.
      if (s.pending) {
        const cmd = s.pending;
        s.pending = "";
        setTimeout(() => write(typedCommand(cmd)), 150);
      }
    };
    s.offs.push(ark.on("ark:term", (p) => {
      if (!s.session) early.push(p);
      else if (p.session === s.session) output(p);
    }));
    try {
      const info = await ark.call("term_open", { cols: term.cols, rows: term.rows });
      s.session = info.session;
      if (info.reattached && info.history) term.write(info.history);
      // A re-attached shell's history already holds what arrived early.
      for (const p of early.splice(0)) if (!info.reattached && p.session === s.session) output(p);
      if (info.reattached && s.pending) {
        write(typedCommand(s.pending));
        s.pending = "";
      }
      fit();
      term.focus();
    } catch (e) {
      term.write(`\r\n\x1b[31mCould not start a shell: ${e.message}\x1b[0m\r\n`);
    }
  })();

  const ro = new ResizeObserver(() => fit());
  ro.observe(el);
  const scheme = matchMedia("(prefers-color-scheme: dark)");
  const retheme = () => s.term && (s.term.options.theme = theme());
  scheme.addEventListener("change", retheme);
  return {
    focus: () => ({ surface: { kind_tag: "terminal", title: "Terminal" } }),
    update: (p) => p?.locator && write(typedCommand(p.locator)),
    refresh: () => setTimeout(() => { fit(); s.term?.focus(); }, 0),
    // Closing the tab keeps the shell; reopening re-attaches with its scrollback.
    dispose: () => {
      ro.disconnect();
      scheme.removeEventListener("change", retheme);
      for (const off of s.offs) off();
      s.term?.dispose();
    },
  };
}

// ---- browser -----------------------------------------------------------------------------------

const hostOf = (u) => {
  try { return new URL(u).hostname; } catch { return u; }
};

function browser(el, params, ark) {
  el.classList.add("wb-col");
  const s = { url: "" };
  const address = h("input.wb-input", { placeholder: "Search or enter an address", spellcheck: "false", "aria-label": "Address" });
  const frame = h("iframe.wb-frame", { sandbox: "allow-scripts allow-same-origin allow-forms allow-popups", referrerpolicy: "no-referrer", title: "Page" });
  const blank = h("div.wb-empty", "Enter an address. Some sites refuse to be shown inside another app; open those in your browser.");
  const back = h("button.icon-btn", { type: "button", title: "Back", "aria-label": "Back", onclick: () => { try { frame.contentWindow.history.back(); } catch {} } }, icon("arrow-left"));
  const reload = h("button.icon-btn", { type: "button", title: "Reload", "aria-label": "Reload", onclick: () => s.url && (frame.src = s.url) }, icon("reload"));
  const out = h("button.icon-btn", { type: "button", title: "Open in your browser", "aria-label": "Open in your browser", onclick: () => s.url && window.open(s.url, "_blank", "noopener") }, icon("external"));
  el.append(h("div.wb-bar", back, reload, address, out), h("div.wb-frame-host", frame, blank));

  const go = (u) => {
    u = String(u || "").trim();
    if (!u) return;
    if (!/^[a-z]+:\/\//i.test(u)) u = /\s/.test(u) || !/\./.test(u) ? `https://duckduckgo.com/?q=${encodeURIComponent(u)}` : `https://${u}`;
    s.url = address.value = u;
    blank.hidden = true;
    frame.src = u;
    ark.setPanelTitle("browser", hostOf(u));
    ark.reportFocus();
  };
  address.addEventListener("keydown", (e) => { if (e.key === "Enter") go(address.value); });
  if (params?.locator) go(params.locator);
  else requestAnimationFrame(() => address.focus());
  return {
    focus: () => ({ surface: { kind_tag: "browser", title: s.url ? hostOf(s.url) : "Browser", locator: s.url || undefined } }),
    update: (p) => p?.locator && go(p.locator),
  };
}

// ---- files -------------------------------------------------------------------------------------

function files(el, params, ark) {
  el.classList.add("wb-col");
  const s = { dir: "", file: null, text: "", dirty: false, line: 1 };
  const crumbs = h("div.wb-crumbs");
  const body = h("div.wb-files");
  el.append(h("div.wb-bar", crumbs), body);

  const crumb = (label, path) => h("button.wb-crumb", { type: "button", onclick: () => list(path) }, label);
  const drawCrumbs = (path, root) => {
    clear(crumbs);
    const parts = path ? path.split("/") : [];
    crumbs.append(crumb(root.split("/").filter(Boolean).pop() || "Folder", ""));
    parts.forEach((p, i) => crumbs.append(h("span.wb-sep", "/"), crumb(p, parts.slice(0, i + 1).join("/"))));
  };

  const leave = () => !s.dirty || confirm(`Discard your changes to ${s.file}?`);

  async function list(path) {
    if (!leave()) return;
    s.file = null;
    s.dirty = false;
    ark.setPanelTitle("files", "Files");
    let r;
    try {
      r = await ark.call("files", { path });
    } catch (e) {
      return clear(body).append(h("div.wb-empty", e.message));
    }
    s.dir = path;
    drawCrumbs(path, r.root);
    clear(body).append(
      r.entries.length
        ? h("div.wb-list", r.entries.map((e) =>
          h("button.wb-entry", { type: "button", onclick: () => (e.dir ? list(e.path) : open(e.path)) },
            icon(e.dir ? "folder" : "file"), h("span.wb-name", e.name), e.dir ? null : h("span.wb-size", size(e.size)))))
        : h("div.wb-empty", "This folder is empty."),
      // `append` prints null as text: an empty string adds nothing.
      r.more ? h("div.wb-note", `${r.more} more not shown`) : "",
    );
    ark.reportFocus();
  }

  async function open(path) {
    if (s.file !== path && !leave()) return;
    let r;
    try {
      r = await ark.call("file_read", { path });
    } catch (e) {
      return ark.toast(e.message);
    }
    s.file = r.path;
    s.dirty = false;
    const parent = r.path.includes("/") ? r.path.slice(0, r.path.lastIndexOf("/")) : "";
    s.dir = parent;
    const name = r.path.split("/").pop();
    ark.setPanelTitle("files", name);
    clear(crumbs).append(crumb("‹ Back", parent), h("span.wb-file-name", r.path));
    if (r.text == null) {
      clear(body).append(h("div.wb-empty", `${name} is ${r.why} (${size(r.size)}).`));
      return ark.reportFocus();
    }
    s.text = r.text;
    const area = h("textarea.wb-editor", { spellcheck: "false", "aria-label": name });
    area.value = r.text;
    const save = h("button.wb-save", { type: "button", disabled: true, onclick: () => store(area) }, "Save");
    const status = h("span.wb-status");
    const mark = () => {
      s.dirty = area.value !== s.text;
      save.disabled = !s.dirty;
      status.textContent = s.dirty ? "Edited" : "";
    };
    const where = () => {
      s.line = area.value.slice(0, area.selectionStart).split("\n").length;
      ark.reportFocus();
    };
    area.addEventListener("input", mark);
    area.addEventListener("click", where);
    area.addEventListener("keyup", where);
    area.addEventListener("keydown", (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") { e.preventDefault(); store(area); }
    });
    crumbs.append(h("span.spacer"), status, save);
    clear(body).append(area);
    if (params?.locator && /^\d+$/.test(params.locator)) goToLine(area, Number(params.locator));
    ark.reportFocus();

    async function store(area) {
      if (!s.dirty) return;
      try {
        await ark.call("file_write", { path: s.file, text: area.value });
        s.text = area.value;
        mark();
        ark.toast(`Saved ${name}`);
      } catch (e) {
        ark.toast(e.message);
      }
    }
  }

  const show = (p) => (p?.path ? open(p.path) : list(s.dir));
  show(params);
  return {
    focus: () => ({ surface: { kind_tag: s.file ? "file" : "folder", title: s.file || s.dir || "Project folder", locator: s.file ? `${s.file}:${s.line}` : undefined } }),
    update: (p) => p?.path && open(p.path),
    refresh: () => !s.file && list(s.dir),
  };
}

function goToLine(area, n) {
  const lines = area.value.split("\n");
  const at = lines.slice(0, Math.max(0, n - 1)).join("\n").length + (n > 1 ? 1 : 0);
  area.focus();
  area.setSelectionRange(at, at);
}

function size(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

// ---- registration ------------------------------------------------------------------------------

/// The built-in panels with their keys, in + menu order. `key` matches a keydown.
export const BUILTINS = [
  { id: "terminal", title: "Terminal", icon: "terminal", presents: ["terminal"], wide: true, fill: true, keys: "⌃`", key: (e) => e.ctrlKey && !e.metaKey && e.key === "`", render: terminal },
  { id: "browser", title: "Browser", icon: "globe", presents: ["browser"], wide: true, fill: true, keys: "⌘T", key: (e) => e.metaKey && !e.shiftKey && e.key.toLowerCase() === "t", render: browser },
  { id: "files", title: "Files", icon: "folder", presents: ["file", "folder"], wide: true, fill: true, keys: "⌘P", key: (e) => (e.metaKey || e.ctrlKey) && !e.shiftKey && e.key.toLowerCase() === "p", render: files },
];

export function registerBuiltins(ark) {
  for (const p of BUILTINS) ark.panel({ ...p, nav: true, builtin: true, render: (el, params) => p.render(el, params, ark) });
}
