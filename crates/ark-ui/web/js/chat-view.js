// A conversation an app draws from its own records, with the chat's own view: the same turns,
// work blocks, prose and composer as a chat, so the two cannot look different (ADR 0076).
//
// `turns` take the shape of `chat_read`'s transcript, plus what a run that is not this chat's
// own may need: `key` (stable per turn, so only a changed turn redraws), `note` (a line under
// the person's message), `working: { label, since }` (still going, since a time in ms),
// `status` (a muted line for a run that waits) and `after` (an app's node under the reply).
// Two fields change in place, without drawing the turn again: `streaming` (the text being
// written now) and `approvals` (questions the run waits on, as `ark:approval` brought them).
// `call(key, id)` loads a tool call's detail so its row opens; `stop()` is the chat's Stop.

import { h } from "./dom.js";
import { Thread } from "./conversation.js";
import { Composer } from "./composer.js";

const NEAR_END = 80;

export function chatView(el, base, { submit, stop, call, placeholder = "" } = {}) {
  if (typeof submit !== "function") throw new Error("ark.chatView needs `submit`");
  const api = Object.create(base);
  api.submit = (text) => submit(text);
  api.send = api.submit;
  api.steer = api.submit;
  api.cancel = () => {};
  let running = false;

  const scroller = h("section.scroller");
  const root = h("div.thread");
  scroller.append(root);
  const input = h("textarea", { rows: "1", placeholder, spellcheck: "true" });
  const send = h("button.send", { type: "submit", "aria-label": "Send" });
  const actions = h("span.chat-view-actions");
  const form = h("form.composer", { autocomplete: "off" }, input, h("div.composer-bar", actions, send));
  const queue = h("div.queue");
  const slash = h("div.palette", { hidden: true });
  const surface = h("section.chat-surface.chat-view", scroller, h("div.dock", queue, slash, form));
  el.append(surface);

  const inspect = typeof call === "function" ? (turn, row) => call(turn.key, row.dataset.call) : false;
  const thread = new Thread(root, scroller, api, { inspect });
  const composer = new Composer(api, { input, form, send, queue, slash, model: null, mode: null, attach: null });
  composer.placeholder = placeholder;
  let views = new Map();
  let drawn = false;
  /// Questions this view has drawn, and those answered here (they stay answered when drawn again).
  const seen = new Set();
  const decided = new Set();

  // The chat's Stop: the send button while the run goes on and nothing is typed. Typed text
  // still goes to `submit`; there is no queue.
  const label = () => send.setAttribute("aria-label", running && !input.value.trim() ? "Stop" : "Send");
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
    Promise.resolve(base.call("approve", { id, decision })).catch((e) => base.toast(e.message));
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
      // A new question takes the focus only from inside this view, as the chat's own does.
      const focus = !seen.has(id) && surface.contains(document.activeElement);
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

  function render(turns, { empty = "" } = {}) {
    const stick = thread.stick;
    // The turn in view, by key: it may be drawn again below, and must keep its place.
    const anchor = [...views].find(([, entry]) => entry.view.el.offsetTop + entry.view.el.offsetHeight >= scroller.scrollTop);
    const anchorOffset = anchor ? anchor[1].view.el.offsetTop - scroller.scrollTop : 0;
    if (!turns.length) {
      for (const entry of views.values()) drop(entry);
      views = new Map();
      if (root.querySelector(".welcome h1")?.textContent !== (empty || "What should we work on?")) thread.welcome({ title: empty }, [], () => {});
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

  return {
    render,
    reset,
    input,
    actions,
    focus: () => composer.focus(),
    set: (text) => composer.set(text),
    /// What the composer says while empty, whether it takes a message now, and whether the
    /// run goes on (with `stop`, the send button is Stop while nothing is typed).
    setComposer({ placeholder: words, disabled, running: on } = {}) {
      if (on != null) {
        running = !!on && typeof stop === "function";
        form.dataset.running = String(running);
        label();
      }
      if (words != null) {
        composer.placeholder = words;
        input.placeholder = words;
      }
      if (disabled != null) {
        input.disabled = !!disabled;
        send.disabled = !!disabled;
        form.dataset.unavailable = String(!!disabled);
      }
    },
    dispose() {
      for (const entry of views.values()) entry.view.stop();
      views = new Map();
      composer.dispose();
      surface.remove();
    },
  };
}
