// What agents said to each other (ADR 0119): one quiet line under a turn, and the view it opens.
//
// `exchangeLine(line, open)` is a button: "2 messages with ⬤ CMO". `exchangeView(...)` is the
// messages between the chat's agent and the others, over the chat's own surface: the chat's
// author line and bubble, read only. Data comes from the window calls `exchange_lines` and
// `exchange_talk` (`ark-interact::exchanges`).

import { h, icon, clear, agentFace } from "./dom.js";
import { markdown } from "./md.js";

const GAP_MS = 60 * 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;

function who(person, size) {
  return h("span.xwho", agentFace(person.agent, size, person.look || ""), h("span.xname", { "data-variant": person.look || null }, person.name));
}

/// A line under a turn. `line` is one row of `exchange_lines`; `open(line, button)` runs on a click.
export function exchangeLine(line, open, { fresh = false } = {}) {
  const parts = line.parts?.length ? line.parts : [{ text: line.label }];
  const button = h("button.xline", {
    type: "button",
    "aria-label": line.label,
    "data-turn": line.turn,
    onclick: () => open(line, button),
  }, parts.map((p) => (p.with != null ? who(line.with[p.with], 16) : h("span.xtext", p.text))));
  if (fresh) button.classList.add("xnew");
  return button;
}

/// "Today 14:32", "Yesterday 09:10", "Mon 12 Nov 17:45", in the page's language.
export function stamp(ms, now = Date.now()) {
  const at = new Date(ms);
  const midnight = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((midnight(new Date(now)) - midnight(at)) / DAY_MS);
  const time = at.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (days === 0) return `${new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(0, "day")} ${time}`.replace(/^./, (c) => c.toUpperCase());
  if (days === 1) return `${new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(-1, "day")} ${time}`.replace(/^./, (c) => c.toUpperCase());
  const date = at.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short", ...(days > 300 ? { year: "numeric" } : {}) });
  return `${date} ${time}`;
}

const key = (m) => `${m.author}|${m.at_ms}|${m.work || ""}|${m.peer || ""}|${m.text.length}|${m.failed ? 1 : 0}`;

/// The view over `surface`. `read()` resolves `{ messages, first }` (merged and ordered, and the chat's
/// agent as the runtime names it), `people` names the agents `{ first, others }`. `close()` returns to the chat. Returns `{ el, refresh, close }`.
export function exchangeView({ surface, people, read, onClose, sink = null, agent = people.first.agent, project = "" }) {
  const { first, others } = people;
  const back = h("button.xback.icon-btn", { type: "button", "aria-label": "Back", onclick: () => close() }, icon("arrow-left"));
  const chip = h("div.xchip", who(first, 20), h("span.xarrows", { "aria-hidden": "true" }, "⇄"), others.map((o) => who(o, 20)));
  chip.setAttribute("aria-label", `${first.name} and ${others.map((o) => o.name).join(", ")}`);
  const head = h("header.xhead", back, chip);
  const root = h("div.thread.xthread", { "aria-live": "polite" });
  const scroller = h("section.scroller.xscroll", root);
  const el = h("section.xview", { role: "region", "aria-label": "Messages between agents", "data-state": "loading" }, head, scroller);
  const names = new Map([first, ...others].map((p) => [p.agent, { ...p }]));
  const rows = new Map();
  let first_read = true;
  let closed = false;
  let stick = true;
  let latest = 0;
  scroller.addEventListener("scroll", () => { stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40; });

  const inert = [...surface.children];
  for (const child of inert) child.inert = true;
  surface.append(el);

  const keydown = (event) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    event.preventDefault();
    event.stopPropagation();
    close();
  };
  document.addEventListener("keydown", keydown, true);

  function message(m, fresh) {
    const person = names.get(m.author) || { agent: m.author, name: m.author, look: "" };
    const body = m.failed
      ? h("div.xfail", "Did not arrive")
      : h("div.bubble.xbubble", { html: markdown(m.text) });
    const node = h("div.xmsg", { "data-author": m.author, "data-failed": m.failed ? "true" : null },
      h("div.turn-author", agentFace(person.agent, 20, person.look || ""), h("span.xname", { "data-variant": person.look || null }, person.name)),
      body);
    if (fresh) node.classList.add("xnew");
    return node;
  }

  function typing(author) {
    const person = names.get(author) || { agent: author, name: author, look: "" };
    return h("div.xmsg.xwaiting", { "data-author": author },
      h("div.turn-author", agentFace(person.agent, 20, person.look || ""), h("span.xname", { "data-variant": person.look || null }, person.name)),
      h("div.bubble.xbubble", h("div.greeting-typing", { role: "status", "aria-label": `${person.name} is writing` }, h("span"), h("span"), h("span"))));
  }

  function draw(messages) {
    const want = [];
    const seen = new Map();
    let previous = null;
    for (const m of messages) {
      // Two alike messages in one millisecond stay two.
      const same = key(m);
      const n = seen.get(same) || 0;
      seen.set(same, n + 1);
      const k = n ? `${same}#${n}` : same;
      if (!previous || m.at_ms - previous.at_ms > GAP_MS || new Date(m.at_ms).toDateString() !== new Date(previous.at_ms).toDateString()) {
        const sk = `stamp|${k}`;
        let s = rows.get(sk);
        if (!s) rows.set(sk, s = h("div.xstamp", stamp(m.at_ms)));
        want.push([sk, s]);
      }
      let node = rows.get(k);
      if (!node) rows.set(k, node = message(m, !first_read));
      want.push([k, node]);
      previous = m;
    }
    // A brief whose reply may still come shows who is writing, after the last message.
    const waiting = new Set(messages.filter((m) => m.pending).map((m) => (m.author === first.agent ? m.peer : first.agent)));
    for (const peer of waiting) {
      const tk = `typing|${peer}`;
      let node = rows.get(tk);
      if (!node) rows.set(tk, node = typing(peer));
      want.push([tk, node]);
    }
    const keep = new Set(want.map(([k]) => k));
    for (const [k, node] of rows) if (!keep.has(k)) { node.remove(); rows.delete(k); }
    want.forEach(([, node], i) => { if (root.children[i] !== node) root.insertBefore(node, root.children[i] || null); });
    if (!messages.length) {
      if (!root.querySelector(".xempty")) root.append(h("p.xempty", "No messages yet."));
    } else root.querySelector(".xempty")?.remove();
    el.dataset.state = messages.length ? "ready" : "empty";
    if (stick) scroller.scrollTop = scroller.scrollHeight;
  }

  function fail() {
    clear(root);
    rows.clear();
    el.dataset.state = "error";
    root.append(h("div.xerror", h("p", "Could not load the exchange."), h("button.xretry", { type: "button", onclick: () => { void refresh(); } }, "Retry")));
  }

  async function refresh() {
    let messages;
    const mine = ++latest;
    try {
      const got = await read();
      // An older read that lands after a newer one is dropped.
      if (mine !== latest) return;
      messages = got.messages;
      const known = names.get(first.agent);
      if (got.first && (got.first.name !== known.name || got.first.look !== known.look)) {
        Object.assign(known, got.first);
        chip.firstChild.replaceWith(who(known, 20));
      }
    } catch (error) {
      if (mine !== latest) return;
      // Messages already shown stay; the error replaces only an empty view.
      if (!closed && el.dataset.state !== "ready") fail();
      sink?.(error);
      return;
    }
    if (closed) return;
    root.querySelector(".xerror")?.remove();
    draw(messages);
    first_read = false;
  }

  function close() {
    if (closed) return;
    closed = true;
    document.removeEventListener("keydown", keydown, true);
    el.remove();
    for (const child of inert) child.inert = false;
    onClose?.();
  }

  back.focus({ preventScroll: true });
  void refresh();
  return { el, refresh, close, agent, project, get open() { return !closed; } };
}
