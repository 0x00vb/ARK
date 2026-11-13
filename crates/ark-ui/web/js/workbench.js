// The panels every window has, in the side pane: Terminal, Browser and Files, opened from the
// top bar's + menu, a key, or the agent's `present`. They work in the project's folder. An app
// replaces one by registering a panel with the same id.
//
//   terminal  the project's shell (`term_*`); the agent may fill the prompt, never press Enter
//   browser   a page in a frame, with an address bar
//   files     the project's folder; a file opens in a tab of its own (file.js)
//   processes what agents started, with output and Stop (processes.js), when the app offers it

import { h, icon, clear } from "./dom.js";
import { KIND_ICON } from "./file.js";
import { processes, watchProcesses } from "./processes.js";
import { mod } from "./keys.js";
import { openOutside } from "./links.js";

/// What the agent may put at the prompt: one line, no control characters, so it cannot run
/// a command itself (that would skip approval). The person presses Enter.
export const typedCommand = (cmd) => String(cmd ?? "").replace(/[\x00-\x1f\x7f-\x9f]+/g, " ");

export const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/// Load a script or stylesheet once.
const loaded = new Map();
export function load(src) {
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

export async function xterm() {
  await Promise.all([load("vendor/xterm/xterm.css"), load("vendor/xterm/xterm.js")]);
  await load("vendor/xterm/xterm-addon-fit.js");
  return window.Terminal;
}

// ---- terminal ----------------------------------------------------------------------------------

function terminal(el, params, ark) {
  el.classList.add("wb-terminal");
  const project = params?.project;
  const place = params?.place || "";
  const s = {
    session: null, ref: null, generation: null, cursor: 0, sequence: 1,
    term: null, fit: null, pending: params?.locator || "", pendingWrite: "",
    writing: false, offs: [], ended: false, connected: false, alive: true,
  };
  const gap = h("div.term-gap", { role: "status", hidden: true }, "Earlier output unavailable");
  const reopen = h("button.btn.small.term-reopen", { type: "button", hidden: true }, "Reopen");
  el.append(gap, reopen);
  const call = (kind, data = {}) => ark.resource(s.ref, kind, data);
  const pumpWrite = async () => {
    if (s.writing || !s.pendingWrite || !s.ref || s.ended || !s.connected) return;
    const data = s.pendingWrite;
    s.pendingWrite = "";
    s.writing = true;
    const sequence = s.sequence++;
    try { await call("terminal_write", { data, sequence }); }
    catch {
      s.connected = false;
      s.pendingWrite = "";
      if (s.term) s.term.options.disableStdin = true;
    }
    finally {
      s.writing = false;
      pumpWrite();
    }
  };
  const write = (data) => {
    if (!s.ref || s.ended || !s.connected) return;
    s.pendingWrite += data;
    pumpWrite();
  };
  const ended = () => {
    if (s.ended) return;
    s.ended = true;
    s.connected = false;
    s.pendingWrite = "";
    if (s.term) s.term.options.disableStdin = true;
    s.term?.write("\r\n\x1b[2m[shell exited]\x1b[0m\r\n");
    reopen.hidden = false;
  };
  const adopt = (info) => {
    s.session = info.ref?.id || info.session;
    s.ref = info.ref;
    s.generation = info.generation;
    s.cursor = info.cursor || 0;
    s.sequence = 1;
    s.ended = false;
    s.connected = true;
    reopen.hidden = true;
    if (s.term) s.term.options.disableStdin = false;
    if (info.ended) ended();
  };
  const openProjectTerminal = async () => {
    if (!project?.id) throw new Error("Choose a project first");
    const places = await ark.windowCall("places");
    const input = {
      project: project.id,
      place,
      here: places.here || "This computer",
      cols: s.term?.cols || 80,
      rows: s.term?.rows || 24,
      big: false,
    };
    let info = await ark.windowCall("project_terminal_open", input);
    if (info.ask) {
      const bytes = Number(info.ask.bytes || 0).toLocaleString();
      if (!confirm(`Copy ${bytes} bytes to ${place}?`)) throw new Error("Copy cancelled");
      info = await ark.windowCall("project_terminal_open", { ...input, big: true });
    }
    if (info.busy) throw new Error(`Work is running in this project on ${place}`);
    return info;
  };
  reopen.onclick = async () => {
    reopen.disabled = true;
    try {
      const info = await openProjectTerminal();
      adopt(info);
      if (info.history) s.term?.write(info.history);
      s.term?.focus();
    } catch (e) {
      if (!s.alive) return;
      s.term?.write(`\r\n\x1b[31m${e.message}\x1b[0m\r\n`);
    } finally {
      reopen.disabled = false;
    }
  };
  let fitFrame = 0;
  const fit = () => {
    if (!s.alive) return;
    cancelAnimationFrame(fitFrame);
    fitFrame = requestAnimationFrame(() => { if (s.alive) { try { s.fit?.fit(); } catch {} } });
  };
  const theme = () => ({
    background: css("--term-bg") || css("--pane") || "#181818",
    foreground: css("--term-fg") || css("--text") || "#ececeb",
    cursor: css("--term-cursor") || css("--accent") || "#888",
    selectionBackground: css("--term-selection") || "rgba(127,127,127,0.3)",
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
    term.onResize(({ cols, rows }) => s.ref && call("terminal_resize", { cols, rows }).catch(() => {}));
    // Output can arrive before `term_open` answers with the session id; hold it until then.
    const early = [];
    const output = (p) => {
      if (!s.alive) return;
      if (p.generation && s.generation && p.generation !== s.generation) return;
      if (p.exit) return ended();
      term.write(p.data);
      if (p.cursor) s.cursor = p.cursor;
      // Type a presented command once the shell has drawn its prompt.
      if (s.pending) {
        const cmd = s.pending;
        s.pending = "";
        setTimeout(() => write(typedCommand(cmd)), 150);
      }
    };
    s.offs.push(ark.on("ark:term", (p) => {
      if (!s.session) early.push(p);
      else if (p.ref ? JSON.stringify(p.ref) === JSON.stringify(s.ref) : p.session === s.session) output(p);
    }));
    try {
      const info = await openProjectTerminal();
      if (!s.alive) {
        if (info.ref) ark.resource(info.ref, "unsubscribe").catch(() => {});
        return;
      }
      adopt(info);
      if (info.reattached && info.history) term.write(info.history);
      // A re-attached shell's history already holds what arrived early.
      for (const p of early.splice(0)) if (!info.reattached && (p.ref ? JSON.stringify(p.ref) === JSON.stringify(s.ref) : p.session === s.session)) output(p);
      if (info.reattached && s.pending) {
        write(typedCommand(s.pending));
        s.pending = "";
      }
      fit();
      term.focus();
    } catch (e) {
      if (!s.alive) return;
      term.write(`\r\n\x1b[31mCould not start a shell: ${e.message}\x1b[0m\r\n`);
    }
  })();

  const ro = new ResizeObserver(() => fit());
  ro.observe(el);
  const retheme = () => s.term && (s.term.options.theme = theme());
  addEventListener("ark:theme", retheme);
  const replay = async () => {
    if (!s.session || !s.generation) return;
    try {
      const r = await call("terminal_read", { generation: s.generation, after: s.cursor });
      if (!s.alive) return;
      if (r.gap) gap.hidden = false;
      if (r.data) s.term?.write(r.data);
      s.cursor = r.cursor;
      if (r.ended) ended();
      else {
        s.connected = true;
        if (s.term) s.term.options.disableStdin = false;
      }
    } catch (e) {
      if (!s.alive) return;
      s.term?.write(`\r\n\x1b[2m[${e.message}]\x1b[0m\r\n`);
    }
  };
  s.offs.push(ark.on("ark:connected", replay));
  s.offs.push(ark.on("ark:disconnected", () => {
    s.connected = false;
    s.pendingWrite = "";
    if (s.term) s.term.options.disableStdin = true;
  }));
  return {
    focus: () => ({ surface: { kind_tag: "terminal", title: "Terminal" } }),
    update: (p) => p?.locator && write(typedCommand(p.locator)),
    refresh: () => setTimeout(() => { fit(); s.term?.focus(); }, 0),
    reconnect: replay,
    // Closing the tab keeps the shell; reopening re-attaches with its scrollback.
    dispose: () => {
      s.alive = false;
      s.connected = false;
      s.pendingWrite = "";
      cancelAnimationFrame(fitFrame);
      ro.disconnect();
      removeEventListener("ark:theme", retheme);
      for (const off of s.offs) off();
      if (s.ref) call("unsubscribe").catch(() => {});
      // WebKit may deliver a viewport resize to xterm after this controller is detached. Let
      // that queued event finish before xterm tears down its renderer.
      const term = s.term;
      s.term = null;
      setTimeout(() => { try { term?.dispose(); } catch {} }, 100);
    },
  };
}

// ---- browser -----------------------------------------------------------------------------------

const hostOf = (u) => {
  try { return new URL(u).hostname; } catch { return u; }
};
// With the port: for a dev server the port is the point (`localhost:3000`).
const hostPortOf = (u) => {
  try { return new URL(u).host || u; } catch { return u; }
};

// What ARK draws over the pane: menus, the palette, dialogs, toasts, the narrow window's sidebar,
// and anything an app marks `data-ark-overlay`. The window's Browser view sits above the page,
// so it steps aside while one of these covers it.
const OVERLAYS = ".popover, .overlay, .toast[data-show='true'], dialog[open], [data-ark-overlay], .app[data-compact='true'] .sidebar";

// An address on this machine typed without a scheme (`localhost:3000`, `127.0.0.1:8080/x`):
// plain http, as a dev server serves it.
const LOCAL = /^(localhost|[\w-]+\.localhost|127(\.\d{1,3}){3}|0\.0\.0\.0|\[::1\])(:\d+)?([/?#]\S*)?$/i;

const intersects = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

/// Where the Browser view may show: the host's rectangle inside the open pane, or null when the
/// tab is hidden, the pane closed, the window's page hidden, or something of ARK's covers it.
function placeOf(host) {
  if (!host.isConnected || document.hidden) return null;
  const pane = host.closest(".pane");
  if (pane && document.querySelector(".app")?.dataset.pane === "closed") return null;
  const r = host.getBoundingClientRect();
  const p = pane ? pane.getBoundingClientRect() : r;
  // The pane's resize grip stays reachable along its edge.
  const grip = pane?.querySelector(".pane-grip")?.offsetWidth || 0;
  const box = { left: Math.max(r.left, p.left + grip), top: Math.max(r.top, p.top), right: Math.min(r.right, p.right), bottom: Math.min(r.bottom, p.bottom) };
  if (box.right - box.left < 2 || box.bottom - box.top < 2) return null;
  for (const o of document.querySelectorAll(OVERLAYS)) {
    const or = o.getBoundingClientRect();
    if (or.width && or.height && intersects(or, box) && getComputedStyle(o).visibility !== "hidden") return null;
  }
  return { x: Math.round(box.left), y: Math.round(box.top), width: Math.round(box.right - box.left), height: Math.round(box.bottom - box.top) };
}

function browser(el, params, ark) {
  el.classList.add("wb-col");
  const view = ark.browserView;
  const s = { url: "", shown: "" };
  const address = h("input.wb-input", { placeholder: "Search or enter an address", spellcheck: "false", "aria-label": "Address" });
  const host = h("div.wb-frame-host");
  const blank = h("div.wb-empty", "Search or enter an address");
  const out = () => s.url && openOutside(s.url);
  const back = h("button.icon-btn", { type: "button", title: "Back", "aria-label": "Back", onclick: () => nav.back() }, icon("arrow-left"));
  const reload = h("button.icon-btn", { type: "button", title: "Reload", "aria-label": "Reload", onclick: () => s.url && nav.reload() }, icon("reload"));
  const outBtn = h("button.icon-btn", { type: "button", title: "Open in your browser", "aria-label": "Open in your browser", onclick: out }, icon("external"));
  const bar = h("div.wb-bar", back, reload, address, outBtn);
  el.append(bar, host);

  const shown = (u) => {
    s.url = u;
    if (document.activeElement !== address) address.value = u;
    ark.setPanelTitle("browser", hostOf(u));
    ark.reportFocus();
  };
  // The bar goes back to the page shown.
  const restore = () => {
    address.value = s.url = s.shown;
    blank.hidden = !!s.shown;
    ark.setPanelTitle("browser", s.shown ? hostOf(s.shown) : "Browser");
    ark.reportFocus();
  };
  // An address the view refuses (the app's own pages, files): says why.
  const failed = (u, e) => {
    ark.toast(e?.message || String(e));
    if (s.url === u) restore();
  };
  // Nothing answers at the address (nothing runs on that port, a name nobody knows). The bar
  // keeps what was typed.
  const downText = h("p");
  const down = h("div.wb-empty.wb-refused", { hidden: true }, downText, h("button.btn", { type: "button", onclick: () => (nav.retry || nav.reload)() }, icon("reload"), "Try again"));
  const ui = {
    shown: (u) => shown((s.shown = u)),
    failed,
    restore,
    down: (u, message) => {
      if (u) downText.textContent = message || `${hostPortOf(u)} can't be reached`;
      else if (down.contains(document.activeElement)) address.focus();
      down.hidden = !u;
    },
    loading: (on) => bar.toggleAttribute("data-loading", on),
    // A page arrived: what shows through it is white, as a page without a background expects.
    page: () => host.setAttribute("data-page", ""),
  };
  const nav = ark.destinationBrowser || ark.remoteResource
    ? remoteBrowser(host, ark, ui)
    : view ? nativeBrowser(host, view, ui) : framedBrowser(host, ark, out);
  host.append(blank);
  if (view || ark.destinationBrowser || ark.remoteResource) host.append(down);

  const go = (u) => {
    u = String(u || "").trim();
    if (!u) return;
    if (!/^[a-z]+:\/\//i.test(u)) u = LOCAL.test(u) ? `http://${u}` : /\s/.test(u) || !/\./.test(u) ? `https://duckduckgo.com/?q=${encodeURIComponent(u)}` : `https://${u}`;
    blank.hidden = true;
    address.value = u;
    shown(u);
    nav.load(u);
  };
  address.addEventListener("keydown", (e) => { if (e.key === "Enter") go(address.value); });
  if (params?.locator) go(params.locator);
  else requestAnimationFrame(() => address.focus());
  return {
    focus: () => ({ surface: { kind_tag: "browser", title: s.url ? hostOf(s.url) : "Browser", locator: s.url || undefined } }),
    update: (p) => p?.locator && go(p.locator),
    dispose: () => nav.dispose(),
  };
}

/// A chat-owned Chromium page rendered by the runtime that owns the resource. Only one frame
/// request is in flight; the next poll replaces the image, so a slow link cannot queue frames.
function remoteBrowser(host, ark, ui) {
  const ref = ark.resourceRef("browser", "main", ark.location);
  const status = h("span.sr-only", { role: "status", "aria-live": "polite" });
  const image = h("img.remote-browser", {
    alt: "",
    role: "application",
    tabindex: "0",
    "aria-label": "Browser page on the chat's computer",
    "aria-description": "Press Control Option Escape to return keyboard control to ARK",
  });
  host.append(image, status);
  const s = { alive: true, loading: false, timer: 0, width: 1280, height: 900, captured: false, url: "", sequence: 0, navigation: "ready" };
  const call = (kind, input = {}) => ark.resource(ref, kind, input);
  const size = () => {
    const r = host.getBoundingClientRect();
    return { width: Math.max(320, Math.min(1920, Math.round(r.width || 1280))), height: Math.max(240, Math.min(1200, Math.round(r.height || 900))) };
  };
  const point = (e) => {
    const r = image.getBoundingClientRect();
    return { x: (e.clientX - r.left) * s.width / Math.max(1, r.width), y: (e.clientY - r.top) * s.height / Math.max(1, r.height) };
  };
  const poll = async () => {
    if (!s.alive || s.loading || s.navigation !== "ready") return;
    if (!ark.online()) {
      if (s.alive) s.timer = setTimeout(poll, 1000);
      return;
    }
    s.loading = true;
    const sequence = s.sequence;
    const dims = size();
    try {
      if (dims.width !== s.width || dims.height !== s.height) {
        s.width = dims.width; s.height = dims.height;
        await call("browser_resize", { ...dims, scale: Math.min(devicePixelRatio || 1, 2) });
        if (!s.alive || sequence !== s.sequence || s.navigation !== "ready") return;
      }
      const frame = await call("browser_snapshot", dims);
      if (!s.alive || sequence !== s.sequence || s.navigation !== "ready") return;
      if (frame.image) image.src = `data:${frame.media_type};base64,${frame.image}`;
      s.width = frame.width; s.height = frame.height;
      ui.loading(frame.loading);
      if (frame.url) ui.shown(frame.url);
      s.url = frame.url || s.url;
      ui.down("");
      status.textContent = frame.loading ? `Loading ${frame.url || "page"}` : frame.title || frame.url || "Browser page";
      ui.page();
    } catch (e) {
      if (!s.alive || sequence !== s.sequence || s.navigation !== "ready") return;
      status.textContent = e.message;
      ui.loading(false);
      ui.down(s.url || "remote://browser", e.message);
    } finally {
      s.loading = false;
      if (s.alive) s.timer = setTimeout(poll, 250);
    }
  };
  const navigate = async (operation, input = {}) => {
    const url = input.url || s.url;
    const sequence = ++s.sequence;
    s.navigation = "loading";
    s.url = url;
    ui.loading(true);
    status.textContent = `Loading ${url}`;
    try {
      await call(operation, input);
      if (!s.alive || sequence !== s.sequence) return;
      s.navigation = "ready";
      ui.down("");
      await poll();
      if (s.alive && sequence === s.sequence) image.focus();
    } catch (e) {
      if (!s.alive || sequence !== s.sequence) return;
      s.navigation = "failed";
      ui.loading(false);
      status.textContent = e.message;
      ui.down(url, e.message);
    }
  };
  const load = (url) => navigate("browser_navigate", { url });
  const pointer = (event) => (e) => {
    const p = point(e);
    call("browser_pointer", { event, ...p, button: e.button === 2 ? "right" : e.button === 1 ? "middle" : "left", clicks: e.detail || 1 }).catch(() => {});
  };
  image.addEventListener("pointermove", pointer("mouseMoved"));
  image.addEventListener("pointerdown", (e) => { image.setPointerCapture(e.pointerId); image.focus(); pointer("mousePressed")(e); });
  image.addEventListener("pointerup", pointer("mouseReleased"));
  image.addEventListener("wheel", (e) => {
    e.preventDefault();
    const p = point(e);
    call("browser_wheel", { ...p, dx: e.deltaX, dy: e.deltaY }).catch(() => {});
  }, { passive: false });
  image.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && e.ctrlKey && e.altKey) {
      e.preventDefault();
      s.captured = false;
      image.blur();
      host.closest(".pane")?.querySelector(".pane-tab[aria-selected=true]")?.focus();
      return;
    }
    s.captured = true;
    e.preventDefault();
    const modifiers = (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
    if (e.key.length === 1 && !e.metaKey && !e.ctrlKey) call("browser_text", { text: e.key }).catch(() => {});
    else call("browser_key", { event: "keyDown", key: e.key, code: e.code, modifiers })
      .then(() => call("browser_key", { event: "keyUp", key: e.key, code: e.code, modifiers })).catch(() => {});
  });
  poll();
  return {
    load,
    retry: () => s.url && load(s.url),
    back: () => navigate("browser_back"),
    reload: () => navigate("browser_reload"),
    reconnect: poll,
    dispose: () => {
      s.alive = false;
      clearTimeout(s.timer);
      call("browser_close").catch(() => {});
    },
  };
}

/// In the window: the page in a webview of its own, kept over `host`. Its place is sent once it
/// holds still for a frame (not every frame of the pane opening), one call at a time; while the
/// tab is out of sight the place is looked at a few times a second only.
function nativeBrowser(host, view, ui) {
  const s = { alive: true, busy: false, created: false, visible: false, wait: false, last: "", prev: "", frame: 0, pending: "", seq: 0, parked: false, checking: false, asked: "", since: 0, cap: 0, off: 0 };
  // The bar shows a load from Enter until the page finishes, at least long enough to be seen (a
  // refusal on this computer comes back at once), at most 30 s: a load that fails after the
  // address answered is not reported by the view.
  const loading = (on) => {
    clearTimeout(s.cap);
    clearTimeout(s.off);
    if (on) {
      s.since ||= performance.now();
      ui.loading(true);
      s.cap = setTimeout(() => loading(false), 30000);
      return;
    }
    s.off = setTimeout(() => {
      s.since = 0;
      ui.loading(false);
    }, Math.max(0, 300 - (performance.now() - s.since)));
  };
  view.listen((ev) => {
    // While an address is checked, or the pane says it does not answer, the view's late news
    // (the page before finishing) is not shown.
    if (!s.alive || s.parked || s.checking) return;
    if (ev.loading) {
      ui.page();
      loading(true);
      return;
    }
    loading(false);
    if (ev.url && /^https?:/i.test(ev.url)) ui.shown(ev.url);
  }).catch(() => {});
  const open = (u) => {
    s.parked = false;
    ui.down(null);
    s.pending = u;
  };
  // A failed load shows nothing in the view, so the window is asked first whether anything
  // answers there. Only the latest address counts.
  const load = (u) => {
    const n = ++s.seq;
    s.asked = u;
    loading(true);
    s.checking = false;
    if (!/^https?:/i.test(u)) return open(u);
    s.checking = true;
    view.reach(u).then((r) => r?.reached !== false, () => true).then((ok) => {
      if (n !== s.seq || !s.alive) return;
      s.checking = false;
      if (ok) return open(u);
      s.parked = true;
      s.pending = "";
      ui.down(u);
      loading(false);
    });
  };
  // The view is made only while the person is in the window (making it brings the app to the
  // front); the window coming to the front asks again.
  const onFocus = () => (s.wait = false);
  addEventListener("focus", onFocus);
  const tick = () => {
    if (!s.alive) return;
    requestAnimationFrame(tick);
    if (s.busy || (!s.visible && !s.pending && s.frame++ % 8)) return;
    const place = s.parked ? null : placeOf(host);
    const key = place ? `${place.x},${place.y},${place.width},${place.height}` : "";
    const still = key === s.prev;
    s.prev = key;
    if (key === s.last && !s.pending) return;
    if (!place) {
      s.last = key;
      if (s.visible) {
        s.visible = false;
        view.act("hide").catch(() => {});
      }
      return;
    }
    if (!still) return;
    if (!s.created && (!s.pending || s.wait || !document.hasFocus())) return;
    const url = s.pending;
    s.busy = true;
    view.show(place, url).then((r) => {
      s.busy = false;
      if (!s.alive) return view.act("close").catch(() => {});
      if (s.pending === url) s.pending = "";
      if (!r?.shown) {
        s.pending ||= url;
        s.wait = true;
        return;
      }
      s.created = s.visible = true;
      s.last = key;
    }, (e) => {
      s.busy = false;
      if (s.pending === url) s.pending = "";
      if (url) {
        loading(false);
        ui.failed(url, e);
      }
    });
  };
  requestAnimationFrame(tick);
  return {
    load,
    // From "can't be reached", Back returns to the page before it; Reload tries again.
    back: () => {
      if (!s.parked) return view.act("back").catch(() => {});
      s.seq++;
      s.parked = s.checking = false;
      ui.down(null);
      ui.restore();
    },
    reload: () => (s.parked ? load(s.asked) : view.act("reload").catch(() => {})),
    dispose: () => {
      s.alive = false;
      clearTimeout(s.cap);
      clearTimeout(s.off);
      removeEventListener("focus", onFocus);
      view.act("close").catch(() => {});
    },
  };
}

/// In a browser: an iframe. A page that refuses frames would stay white, so the runtime is asked
/// whether it refuses (`frame_check`) and the panel says so.
function framedBrowser(host, ark, out) {
  const frame = h("iframe.wb-frame", { sandbox: "allow-scripts allow-same-origin allow-forms allow-popups", referrerpolicy: "no-referrer", title: "Page" });
  const refused = h("div.wb-empty.wb-refused", { hidden: true });
  host.append(frame, refused);
  let current = "";
  return {
    load(u) {
      current = u;
      refused.hidden = true;
      frame.src = u;
      ark.call("frame_check", { url: u }).then((r) => {
        if (r?.framable !== false || current !== u) return;
        refused.replaceChildren(
          h("p", `${hostOf(u)} can't be shown here`),
          h("button.btn", { type: "button", onclick: out }, icon("external"), "Open in your browser"),
        );
        refused.hidden = false;
      }).catch(() => {});
    },
    back: () => { try { frame.contentWindow.history.back(); } catch {} },
    reload: () => current && (frame.src = current),
    dispose: () => {},
  };
}

// ---- files -------------------------------------------------------------------------------------

/// The project's folder. A file opens in a tab of its own (a spreadsheet as a grid, a document
/// as a page, text in an editor), the same file the agent works on.
function files(el, params, ark) {
  el.classList.add("wb-col");
  const s = { dir: "" };
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

  async function list(path) {
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
          h("button.wb-entry", { type: "button", "data-kind": e.kind || null, onclick: () => (e.dir ? list(e.path) : ark.openFile(e.path)) },
            icon(e.dir ? "folder" : KIND_ICON[e.kind] || "file"), h("span.wb-name", e.name), e.dir ? null : h("span.wb-size", size(e.size)))))
        : h("div.wb-empty", "This folder is empty."),
      // `append` prints null as text: an empty string adds nothing.
      r.more ? h("div.wb-note", `${r.more} more not shown`) : "",
    );
    ark.reportFocus();
  }

  const show = (p) => {
    if (p?.path && p.view !== "folder") {
      list(p.path.includes("/") ? p.path.slice(0, p.path.lastIndexOf("/")) : "");
      return ark.openFile(p.path, p);
    }
    list(p?.path || s.dir);
  };
  show(params);
  // Files a call wrote, or that changed without one (a restore, files added): list again.
  const again = () => (el.isConnected ? list(s.dir) : offs.forEach((off) => off()));
  const offs = [
    ark.on("ark:files_changed", again),
    ark.on("ark:event", (m) => m.event?.type === "tool_result" && (m.event.files?.length || m.event.before?.length) && again()),
  ];
  return {
    focus: () => ({ surface: { kind_tag: "folder", title: s.dir || "Project folder" } }),
    update: (p) => show(p),
    refresh: () => list(s.dir),
    dispose: () => {
      for (const off of offs) off();
      ark.resource(ark.resourceRef("file", "", ark.location), "unsubscribe").catch(() => {});
    },
  };
}

function size(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

// ---- registration ------------------------------------------------------------------------------

/// The built-in panels with their keys, in + menu order. `key` matches a keydown.
export const BUILTINS = [
  { id: "terminal", title: "Terminal", icon: "terminal", presents: ["terminal"], projectScoped: true, wide: true, fill: true, keys: "⌃`", key: (e) => e.ctrlKey && !e.metaKey && e.key === "`", render: terminal },
  { id: "browser", title: "Browser", icon: "globe", presents: ["browser"], closesWithProject: true, wide: true, fill: true, keys: "⌘T", key: (e) => mod(e) && !e.shiftKey && e.key.toLowerCase() === "t", render: browser },
  { id: "files", title: "Files", icon: "folder", presents: ["file", "folder"], closesWithProject: true, wide: true, fill: true, keys: "⌘P", key: (e) => (e.metaKey || e.ctrlKey) && !e.shiftKey && e.key.toLowerCase() === "p", render: files },
];

export function registerBuiltins(ark) {
  for (const p of BUILTINS) ark.panel({
    ...p,
    nav: true,
    builtin: true,
    render: (el, params, scoped) => p.render(el, params, p.projectScoped ? ark : scoped || ark),
  });
  // Processes, only where the app lets agents start them (`procs` fails otherwise).
  let added = false;
  const add = async () => {
    if (added) return;
    if (!ark.summary?.()?.location) {
      setTimeout(add, 0);
      return;
    }
    try { await ark.call("procs"); } catch { return; }
    added = true;
    ark.panel({ id: "processes", title: "Processes", icon: "play", presents: ["processes"], nav: true, builtin: true, wide: true, fill: true, render: (el, params, scoped) => processes(el, params, scoped || ark) });
    watchProcesses(ark);
  };
  add();
  ark.on("ark:connected", add);
}
