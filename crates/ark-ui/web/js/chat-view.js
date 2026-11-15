// A conversation an app draws from its own records, with the chat's own view: the same turns,
// work blocks, prose and composer as a chat, so the two cannot look different (ADR 0076).
//
// `turns` take the shape of `chat_read`'s transcript, plus what a run that is not this chat's
// own may need: `key` (stable per turn, so only a changed turn redraws), `note` (a line under
// the person's message), `working: { label, since }` (still going, since a time in ms),
// `status` (a muted line for a run that waits), `after` (an app's node under the reply) and
// `author` (`{ name, avatar(size) }`: who wrote the reply, when several write in one view).
// Two fields change in place, without drawing the turn again: `streaming` (the text being
// written now) and `approvals` (questions the run waits on, as `ark:approval` brought them).
// `call(key, id)` loads a tool call's detail so its row opens; `stop()` is the chat's Stop.
// `exchanges` (ADR 0119) is `{ project, agent, session?, name?, look? }`, or a function returning
// it, or null, read at each use: a view that switches agent and chat gives the new one and calls
// `refreshExchanges()`. The turns whose key is `task:<id>` get a quiet line when that task reached
// other agents, and the line opens what they said over this surface. The lines come from the
// window call `exchange_lines`. Null: no lines and no calls.

import { Thread } from "./conversation.js";
import { Composer } from "./composer.js";
import { createChatSurface } from "./chat-surface.js";
import { exchangeLine, exchangeView } from "./exchange.js";

const NEAR_END = 80;

export function chatView(el, base, { submit, stop, call, placeholder = "", model = null, files = null, location = null, exchanges = null } = {}) {
  if (typeof submit !== "function") throw new Error("ark.chatView needs `submit`");
  const api = Object.create(base);
  if (location) Object.defineProperty(api, "location", {get: () => {
    const next = location(), origin = base.location || base.summary?.()?.location;
    if (!next || next.runtime !== origin?.runtime || next.project !== origin?.project) throw Error("The conversation belongs to another project or computer");
    return {...next};
  }});
  let submitDisabled = false;
  let unavailable = false;
  // `/new` and `/history [words]` mean the same in every conversation: the window acts on them.
  const shared = (text) => {
    const line = String(text || "").trim();
    if (line === "/new" && base.newSession) { void base.newSession(); return true; }
    const history = /^\/history(?:\s+([\s\S]*))?$/.exec(line);
    if (history && base.history) { void base.history((history[1] || "").trim()); return true; }
    return false;
  };
  api.submit = (text) => shared(text) ? undefined : submitDisabled ? composer.set(text) : submit(text);
  api.send = api.submit;
  api.steer = api.submit;
  api.cancel = () => {};
  let running = false;

  const elements = createChatSurface({ kind: "chat-view", capabilities: { model: !!model, files: !!files } });
  const { surface, scroller, threadEl: root, input, send, form, queue, actions } = elements;
  input.placeholder = placeholder;
  el.append(surface);

  const inspect = typeof call === "function" ? (turn, row) => call(turn.key, row.dataset.call, Number(row.dataset.n)) : false;
  const thread = new Thread(root, scroller, api, { inspect });
  const composer = new Composer(api, { ...elements, modelOpen: model?.open, captureOrigin: files?.captureOrigin });
  composer.placeholder = placeholder;
  let views = new Map();
  let drawn = false;
  /// Questions this view has drawn, and those answered here (they stay answered when drawn again).
  const seen = new Set();
  const decided = new Set();

  // The chat's Stop: the send button while the run goes on and nothing is typed. Typed text
  // still goes to `submit`; there is no queue.
  const label = () => {
    const stopping = running && !input.value.trim();
    send.setAttribute("aria-label", stopping ? "Stop" : "Send");
    send.disabled = unavailable || (!stopping && submitDisabled);
  };
  new MutationObserver(label).observe(form, { attributes: true, attributeFilter: ["data-empty"] });
  send.addEventListener("click", (e) => {
    if (!running || input.value.trim()) return;
    e.preventDefault();
    stop();
  });

  function signature(turn) {
    const { after, streaming, approvals, ...rest } = turn;
    return JSON.stringify(rest) + (after ? after.outerHTML : "");
  }

  function decide(id, decision) {
    decided.add(String(id));
    Promise.resolve(base.call("approve", { id, decision })).catch((e) => {
      // Not answered: the card takes an answer again.
      decided.delete(String(id));
      for (const card of surface.querySelectorAll(`.approval[data-id="${CSS.escape(String(id))}"]`)) {
        card.classList.remove("decided");
        for (const b of card.querySelectorAll("button")) b.disabled = false;
      }
      base.toast(e.message);
    });
  }

  /// The text being written grows where it is; the questions the run waits on come and go.
  function sync(entry, turn) {
    const text = turn.streaming || "";
    if (text.length > entry.streamed.length) entry.view.stream(text.slice(entry.streamed.length));
    entry.streamed = text;
    const want = new Map((turn.approvals || []).map((m) => [String(m.id), m]));
    for (const card of entry.view.agent.querySelectorAll(":scope > .approval")) {
      if (!want.has(card.dataset.id)) card.remove();
    }
    for (const [id, msg] of want) {
      if (entry.view.agent.querySelector(`:scope > .approval[data-id="${CSS.escape(id)}"]`)) continue;
      // A new question takes the focus only from inside this view, as the chat's own does, and
      // not from a message being typed, so a key meant for it cannot answer.
      const focus = !seen.has(id) && surface.contains(document.activeElement) && !input.value.trim();
      seen.add(id);
      const card = entry.view.approval(msg, decide, { focus });
      if (decided.has(id)) {
        card.classList.add("decided");
        for (const b of card.querySelectorAll("button")) b.disabled = true;
      }
    }
  }

  /// A turn drawn again keeps the tool rows the person opened.
  function carry(from, to) {
    for (const row of from.el.querySelectorAll('.act[data-expanded="true"]')) {
      const more = row.querySelector(":scope > .act-more");
      const same = [...to.el.querySelectorAll(".act[data-call]")].find((r) => r.dataset.call && r.dataset.call === row.dataset.call);
      if (!more || !same || same.querySelector(".act-more")) continue;
      same.append(more);
      same.dataset.expanded = "true";
      const block = same.closest(".work");
      if (block) block.dataset.open = "true";
    }
  }

  function drop(entry) {
    entry.view.stop();
    entry.view.el.remove();
  }

  function render(turns, { greet = "" } = {}) {
    const stick = thread.stick;
    // The turn in view, by key: it may be drawn again below, and must keep its place.
    const anchor = [...views].find(([, entry]) => entry.view.el.offsetTop + entry.view.el.offsetHeight >= scroller.scrollTop);
    const anchorOffset = anchor ? anchor[1].view.el.offsetTop - scroller.scrollTop : 0;
    if (!turns.length) {
      for (const entry of views.values()) drop(entry);
      views = new Map();
      if (root.querySelector(".greeting")?.dataset.key !== String(greet)) thread.greet(greet);
      drawn = true;
      return;
    }
    thread.dropWelcome();
    const next = new Map();
    let before = null;
    for (const [i, turn] of turns.entries()) {
      const key = turn.key ?? String(i);
      const sig = signature(turn);
      let entry = views.get(key);
      // Text that is not the drawn text continued (taken back, say) draws the turn again.
      if (!entry || entry.sig !== sig || !(turn.streaming || "").startsWith(entry.streamed)) {
        const view = thread.storedTurnView(turn);
        view.key = key;
        // A turn that arrives after the first draw comes in the way a chat's new turn does.
        if (drawn && !entry) view.el.classList.remove("stored");
        if (entry) {
          carry(entry.view, view);
          entry.view.stop();
          entry.view.el.replaceWith(view.el);
        }
        entry = { sig, view, streamed: "" };
      }
      sync(entry, turn);
      const node = entry.view.el;
      const at = before ? before.nextSibling : root.firstChild;
      if (at !== node) root.insertBefore(node, at);
      before = node;
      next.set(key, entry);
    }
    for (const [key, entry] of views) if (!next.has(key)) drop(entry);
    views = next;
    drawn = true;
    decorate();
    const kept = anchor && views.get(anchor[0])?.view.el;
    if (stick) thread.scroll(true);
    else if (kept) scroller.scrollTop = kept.offsetTop - anchorOffset;
  }

  /// Another conversation in the same view: nothing carries over, and its turns do not rise in.
  function reset() {
    for (const entry of views.values()) drop(entry);
    views = new Map();
    thread.clear();
    thread.stick = true;
    drawn = false;
  }

  // ---- what agents said to each other ------------------------------------------------------
  const LINES_EVERY = 1000;
  let lines = new Map();
  let drawnLines = new Map();
  let reads = 0;
  let open = null;
  let timer = 0;
  let disposed = false;

  const target = () => (typeof exchanges === "function" ? exchanges() : exchanges) || null;

  function decorate() {
    if (!exchanges) return;
    for (const [turnKey, entry] of views) {
      const turn = String(turnKey).startsWith("task:") ? String(turnKey).slice(5) : null;
      const line = turn && lines.get(turn);
      const there = entry.view.el.querySelector(":scope > .xline");
      const sig = line ? JSON.stringify([line.label, line.count, line.failed]) : "";
      if (!line) { there?.remove(); continue; }
      if (there && there.dataset.sig === sig) continue;
      there?.remove();
      // The first read draws without motion; a line that comes with a later read fades in.
      const button = exchangeLine(line, (row, opener) => show(row, opener), { fresh: reads > 1 && !drawnLines.has(turn) });
      button.dataset.sig = sig;
      entry.view.el.append(button);
      drawnLines.set(turn, sig);
      if (thread.stick) thread.scroll();
    }
  }

  async function loadLines() {
    const t = target();
    if (!exchanges || disposed || !t) return;
    let rows;
    try {
      rows = await base.call("exchange_lines", { project: t.project, agent: t.agent, session: t.session || "" });
    } catch (error) {
      console.warn("ark: could not read what agents said to each other", error);
      return;
    }
    // The app switched to another chat while this was read: that chat reads for itself.
    const now = target();
    if (disposed || !now || now.project !== t.project || now.agent !== t.agent || (now.session || "") !== (t.session || "")) return;
    reads++;
    lines = new Map(rows.map((row) => [row.turn, row]));
    decorate();
  }

  function scheduleLines() {
    if (!exchanges || timer || document.hidden) return;
    timer = setTimeout(() => {
      timer = 0;
      const t = target();
      if (open?.open && (!t || t.agent !== open.agent || t.project !== open.project)) open.close();
      void loadLines();
      open?.refresh();
    }, LINES_EVERY);
  }
  const stopListening = exchanges ? base.on?.("ark:orch", scheduleLines) : null;

  /// The app switched agent or chat (or has none now): the lines of the one before go, the new
  /// one's are read, and an open view of the old one closes.
  function refreshExchanges() {
    open?.close();
    lines = new Map();
    drawnLines = new Map();
    reads = 0;
    decorate();
    return loadLines();
  }

  function show(line, opener) {
    const d = target();
    if (open?.open || !d) return;
    const self = { agent: d.agent, name: d.name || d.agent, look: d.look || "" };
    const others = line.with;
    const people = { first: self, others };
    const read = async () => {
      const t = target();
      if (!t || t.agent !== d.agent || t.project !== d.project) return { first: people.first, messages: [] };
      const talks = await Promise.all(others.map((o) => base.call("exchange_talk", { project: t.project, a: t.agent, b: o.agent })));
      return { first: talks[0].with[0], messages: talks.flatMap((t, i) => t.messages.map((m) => ({ ...m, peer: others[i].agent }))).sort((a, b) => a.at_ms - b.at_ms) };
    };
    open = exchangeView({
      surface,
      people,
      agent: d.agent,
      project: d.project,
      read,
      sink: (error) => console.warn("ark: could not read an exchange", error),
      onClose: () => {
        open = null;
        const again = surface.querySelector(`.xline[data-turn="${CSS.escape(line.turn)}"]`) || opener;
        if (again?.isConnected) again.focus({ preventScroll: true });
      },
    });
  }

  if (exchanges) void loadLines();

  return {
    render,
    reset,
    input,
    actions,
    uploads: elements.uploads,
    addLine: (text, options) => {
      composer.addLine(text, options);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    },
    refreshExchanges,
    focus: () => composer.focus(),
    set: (text) => composer.set(text),
    /// What the composer says while empty, whether it takes a message now, and whether the
    /// run goes on (with `stop`, the send button is Stop while nothing is typed).
    setComposer({ placeholder: words, disabled, running: on, modelLabel, modelDisabled, modelMissing, submitDisabled: blocked } = {}) {
      if (elements.model) {
        if (modelLabel != null) elements.model.textContent = modelLabel;
        if (modelDisabled != null) elements.model.disabled = !!modelDisabled;
        if (modelMissing != null) elements.model.dataset.missing = String(!!modelMissing);
      }
      if (on != null) {
        running = !!on && typeof stop === "function";
        form.dataset.running = String(running);
        label();
      }
      if (words != null) {
        composer.placeholder = words;
        input.placeholder = words;
      }
      if (blocked != null) submitDisabled = !!blocked;
      if (disabled != null) unavailable = !!disabled;
      label();
      if (disabled != null) {
        input.disabled = !!disabled;
        if (elements.attach) elements.attach.disabled = !!disabled;
        form.dataset.unavailable = String(!!disabled);
      }
    },
    dispose() {
      disposed = true;
      clearTimeout(timer);
      stopListening?.();
      open?.close();
      for (const entry of views.values()) entry.view.stop();
      views = new Map();
      thread.dispose();
      composer.dispose();
      surface.remove();
    },
  };
}
