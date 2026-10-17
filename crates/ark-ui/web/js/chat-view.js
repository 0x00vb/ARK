// A conversation an app draws from its own records, with the chat's own view: the same turns,
// work blocks, prose and composer as a chat, so the two cannot look different (ADR 0076).
//
// `turns` take the shape of `chat_read`'s transcript, plus what a run that is not this chat's
// own may need: `key` (stable per turn, so only a changed turn redraws), `note` (a line under
// the person's message), `working: { label, since }` (still going, since a time in ms),
// `status` (a muted line for a run that waits) and `after` (an app's node under the reply).

import { h } from "./dom.js";
import { Thread } from "./conversation.js";
import { Composer } from "./composer.js";

const NEAR_END = 80;

export function chatView(el, base, { submit, placeholder = "" } = {}) {
  if (typeof submit !== "function") throw new Error("ark.chatView needs `submit`");
  const api = Object.create(base);
  api.submit = (text) => submit(text);
  api.send = api.submit;
  api.steer = api.submit;
  api.cancel = () => {};

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

  const thread = new Thread(root, scroller, api, { inspect: false });
  const composer = new Composer(api, { input, form, send, queue, slash, model: null, mode: null, attach: null });
  composer.placeholder = placeholder;
  let views = new Map();
  let drawn = false;

  function signature(turn) {
    const { after, ...rest } = turn;
    return JSON.stringify(rest) + (after ? after.outerHTML : "");
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
      if (!entry || entry.sig !== sig) {
        const view = thread.storedTurnView(turn);
        // A turn that arrives after the first draw comes in the way a chat's new turn does.
        if (drawn && !entry) view.el.classList.remove("stored");
        if (entry) {
          entry.view.stop();
          entry.view.el.replaceWith(view.el);
        }
        entry = { sig, view };
      }
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
    /// What the composer says while empty, and whether it takes a message now.
    setComposer({ placeholder: words, disabled } = {}) {
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
