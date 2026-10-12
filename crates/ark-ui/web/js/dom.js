// Small DOM helpers shared by every view and exposed to extensions as `ark.h`.

export function h(tag, attrs, ...children) {
  const [name, ...classes] = tag.split(".");
  const el = document.createElement(name || "div");
  if (classes.length) el.className = classes.join(" ");
  if (attrs && (typeof attrs !== "object" || attrs instanceof Node || Array.isArray(attrs))) {
    children.unshift(attrs);
    attrs = null;
  }
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") el.className += (el.className ? " " : "") + v;
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else if (k === "html") el.innerHTML = v;
    else if (k === "dataset") Object.assign(el.dataset, v);
    else if (k === "style" && typeof v === "object") {
      // Custom properties (`--hue`) need setProperty; Object.assign drops them.
      for (const [p, x] of Object.entries(v)) {
        if (p.startsWith("--")) el.style.setProperty(p, x);
        else el.style[p] = x;
      }
    }
    else el.setAttribute(k, v === true ? "" : v);
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

export const $ = (s, root = document) => root.querySelector(s);
export const $$ = (s, root = document) => [...root.querySelectorAll(s)];

export function clear(el) {
  while (el.firstChild) el.firstChild.remove();
  return el;
}

export const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

// Icons: 16px strokes, drawn to one grid so they sit on the same optical baseline.
const PATHS = {
  home: '<path d="M2.5 7.2L8 2.75l5.5 4.45v5.3a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/><path d="M6 13.5V9h4v4.5"/>',
  compose: '<path d="M11.5 2.5l2 2L7 11H5V9z"/><path d="M13 9v4a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h4"/>',
  sidebar: '<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M6 3v10"/>',
  pane: '<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M10 3v10"/>',
  "chevron-down": '<path d="M4.5 6.5L8 10l3.5-3.5"/>',
  "chevron-right": '<path d="M6.5 4.5L10 8l-3.5 3.5"/>',
  "arrow-up": '<path d="M8 13V3.5M4 7.5l4-4 4 4"/>',
  stop: '<rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="currentColor" stroke="none"/>',
  x: '<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>',
  check: '<path d="M3.5 8.5l3 3 6-7"/>',
  alert: '<path d="M8 2.5l6 11H2z"/><path d="M8 6.5v3M8 11.5v.01"/>',
  dot: '<circle cx="8" cy="8" r="2.2" fill="currentColor" stroke="none"/>',
  search: '<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5"/>',
  layers: '<path d="M8 2.5l5.5 3L8 8.5 2.5 5.5z"/><path d="M2.5 8L8 11l5.5-3"/><path d="M2.5 10.5L8 13.5l5.5-3"/>',
  state: '<circle cx="4" cy="4.5" r="1.5"/><circle cx="12" cy="4.5" r="1.5"/><circle cx="8" cy="11.5" r="1.5"/><path d="M5.3 5.5l1.8 4.5M10.7 5.5l-1.8 4.5"/>',
  trace: '<path d="M3 3v10M3 5h5M3 8.5h8M3 12h4"/>',
  gauge: '<path d="M2.8 11.5a5.5 5.5 0 1 1 10.4 0"/><path d="M8 9l2.5-3"/>',
  file: '<path d="M9 2.5H4.5a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1V6z"/><path d="M9 2.5V6h3.5"/>',
  image: '<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><circle cx="6" cy="6.5" r="1.2"/><path d="M2.5 11l3.5-3 2.5 2 2-1.5 3 2.5"/>',
  sheet: '<rect x="2.5" y="2.5" width="11" height="11" rx="1.5"/><path d="M2.5 6.5h11M2.5 10h11M6.5 2.5v11"/>',
  doc: '<path d="M9 2.5H4.5a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1V6z"/><path d="M9 2.5V6h3.5M5.5 8.5h5M5.5 11h3.5"/>',
  terminal: '<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M5 6.5L7 8l-2 1.5M8.5 10H11"/>',
  box: '<path d="M8 2.5l5 2.5v6L8 13.5 3 11V5z"/><path d="M3 5l5 2.5L13 5M8 7.5v6"/>',
  globe: '<circle cx="8" cy="8" r="5.5"/><path d="M2.5 8h11M8 2.5c1.6 1.8 1.6 9.2 0 11M8 2.5c-1.6 1.8-1.6 9.2 0 11"/>',
  server: '<rect x="2.5" y="2.5" width="11" height="4.5" rx="1.25"/><rect x="2.5" y="9" width="11" height="4.5" rx="1.25"/><path d="M5 4.75h.01M5 11.25h.01"/>',
  laptop: '<rect x="3.5" y="3.5" width="9" height="6.5" rx="1"/><path d="M1.75 12.5h12.5"/>',
  cpu: '<rect x="4" y="4" width="8" height="8" rx="1.5"/><path d="M6.5 2v2M9.5 2v2M6.5 12v2M9.5 12v2M2 6.5h2M2 9.5h2M12 6.5h2M12 9.5h2"/>',
  code: '<path d="M6 4.5L2.5 8 6 11.5M10 4.5L13.5 8 10 11.5"/>',
  copy: '<rect x="5" y="5" width="8" height="8" rx="1.5"/><path d="M3 10.5V4a1 1 0 0 1 1-1h6.5"/>',
  folder: '<path d="M2.5 4.5a1 1 0 0 1 1-1H6l1.5 1.5h5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/>',
  bolt: '<path d="M9 2L4 9h4l-1 5 5-7H8z"/>',
  play: '<path d="M5 3.5v9l7-4.5z"/>',
  clock: '<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3.2l2.1 1.3"/>',
  command: '<path d="M6 6V4.5A1.5 1.5 0 1 0 4.5 6H11.5A1.5 1.5 0 1 0 10 4.5V11.5A1.5 1.5 0 1 0 11.5 10H4.5A1.5 1.5 0 1 0 6 11.5z"/>',
  plus: '<path d="M8 3.5v9M3.5 8h9"/>',
  "arrow-left": '<path d="M13 8H3.5M7.5 4l-4 4 4 4"/>',
  reload: '<path d="M13 8a5 5 0 1 1-1.5-3.5"/><path d="M13 2.5v3h-3"/>',
  undo: '<path d="M5.5 3.5L2.5 6.5l3 3"/><path d="M2.5 6.5h7a3.5 3.5 0 0 1 0 7H7"/>',
  external: '<path d="M9.5 2.5h4v4M13.5 2.5L8 8"/><path d="M12 9.5v3a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3"/>',
  more: '<circle cx="4" cy="8" r=".9" fill="currentColor" stroke="none"/><circle cx="8" cy="8" r=".9" fill="currentColor" stroke="none"/><circle cx="12" cy="8" r=".9" fill="currentColor" stroke="none"/>',
  settings: '<path d="M2.5 4.5h6M11.5 4.5h2M2.5 11.5h2M7.5 11.5h6"/><circle cx="10" cy="4.5" r="1.5"/><circle cx="6" cy="11.5" r="1.5"/>',
  sun: '<circle cx="8" cy="8" r="2.75"/><path d="M8 1.75v1.5M8 12.75v1.5M1.75 8h1.5M12.75 8h1.5M3.6 3.6l1.05 1.05M11.35 11.35l1.05 1.05M3.6 12.4l1.05-1.05M11.35 4.65l1.05-1.05"/>',
  shield: '<path d="M8 1.75l5 2v4.1c0 3-2.15 5.25-5 6.4-2.85-1.15-5-3.4-5-6.4v-4.1z"/>',
  plug: '<path d="M6 1.75v3M10 1.75v3M4.25 4.75h7.5v2.5a3.75 3.75 0 0 1-7.5 0zM8 11v3.25"/>',
  phone: '<rect x="4.5" y="1.75" width="7" height="12.5" rx="1.75"/><path d="M7 11.75h2"/>',
  person: '<circle cx="8" cy="5.25" r="2.75"/><path d="M2.75 14.25c.55-2.6 2.65-4.25 5.25-4.25s4.7 1.65 5.25 4.25"/>',
};

/// A touch screen without a keyboard (a phone, a tablet): no shortcuts to mention, and no
/// field focused on its own (that opens the on-screen keyboard over the page).
export function touch() {
  return matchMedia("(hover: none) and (pointer: coarse)").matches;
}

export function icon(name, cls = "") {
  const span = document.createElement("span");
  span.className = "ico" + (cls ? " " + cls : "");
  span.setAttribute("aria-hidden", "true");
  span.innerHTML = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${PATHS[name] || PATHS.dot}</svg>`;
  return span;
}

export function hydrateIcons(root = document) {
  for (const el of $$("[data-icon]", root)) {
    if (!el.firstChild) el.append(icon(el.dataset.icon));
  }
}

// Formatting that matches ark-interact's `fmt` module.
export function tokens(n) {
  n = Number(n || 0);
  if (n >= 10000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
export function secs(ms) {
  ms = Number(ms || 0);
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60000);
  return `${m}m ${Math.round((ms % 60000) / 1000)}s`;
}
export function ago(iso) {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 604800) return `${Math.floor(s / 86400)}d`;
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
export function clip(s, n) {
  s = String(s ?? "").replace(/\s+/g, " ").trim();
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}
export function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}
