import { ICONS } from "./icons.js";

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

/// A touch screen without a keyboard (a phone, a tablet): no shortcuts to mention, and no
/// field focused on its own (that opens the on-screen keyboard over the page).
export function touch() {
  return matchMedia("(hover: none) and (pointer: coarse)").matches;
}

export function icon(name, cls = "") {
  const span = document.createElement("span");
  span.className = "ico" + (cls ? " " + cls : "");
  span.setAttribute("aria-hidden", "true");
  span.innerHTML = `<svg viewBox="0 0 256 256" fill="currentColor" stroke="none">${Object.hasOwn(ICONS, name) ? ICONS[name] : ICONS.dot}</svg>`;
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


let agentIdentity = null;

export function setAgentIdentity(resolve) {
  if (typeof resolve !== "function") throw new Error("ark.agentIdentity needs a function");
  agentIdentity = resolve;
}

const graphemeSegmenter = typeof Intl.Segmenter === "function"
  ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
  : null;

export function chatAvatar(participants = []) {
  const names = Array.isArray(participants) ? [...new Set(participants.filter(name => typeof name === "string" && name))] : [];
  const mark = h("span.chat-avatar", { "aria-hidden": "true", "data-count": names.length });
  if (!names.length) return h("span.chat-avatar", { "aria-hidden": "true", "data-count": "0" }, icon("chat"));
  for (const name of names.slice(0, 3)) {
    const first = graphemeSegmenter?.segment(name)[Symbol.iterator]().next().value?.segment ?? Array.from(name)[0];
    mark.append(h("span.chat-face", { "data-variant": agentIdentity?.(name) }, first.toUpperCase()));
  }
  if (names.length > 3) mark.append(h("span.chat-overflow", `+${names.length - 3}`));
  return mark;
}
