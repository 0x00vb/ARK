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

import { Thread } from "./conversation.js";
import { Composer } from "./composer.js";
import { createChatSurface } from "./chat-surface.js";

const NEAR_END = 80;

export function chatView(el, base, { submit, stop, call, placeholder = "", model = null, files = null, location = null } = {}) {
  if (typeof submit !== "function") throw new Error("ark.chatView needs `submit`");
  const api = Object.create(base);
  if (location) Object.defineProperty(api, "location", {get: () => {
    const next = location(), origin = base.location || base.summary?.()?.location;
    if (!next || next.runtime !== origin?.runtime || next.project !== origin?.project) throw Error("The conversation belongs to another project or computer");
    return {...next};
  }});
  let submitDisabled = false;
  let unavailable = false;
  api.submit = (text) => submitDisabled ? composer.set(text) : submit(text);
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
    uploads: elements.uploads,
    addLine: (text, options) => {
      composer.addLine(text, options);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    },
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
      for (const entry of views.values()) entry.view.stop();
      views = new Map();
      composer.dispose();
      surface.remove();
    },
  };
}
