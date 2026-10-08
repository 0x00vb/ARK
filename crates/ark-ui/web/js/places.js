// Where a chat runs (ADR 0052): this computer, or one of the person's own computers. An empty
// chat offers the choice on a chip beside the model; the first message starts the chat there,
// and from then on the place is fixed. A chat on another computer keeps the chip as its mark,
// and a line above the composer when that computer has files this one does not, or does not
// answer.

import { h, icon, clear, $ } from "./dom.js";
import { xterm } from "./workbench.js";

export class Places {
  constructor(api) {
    this.api = api;
    this.chip = $("#place-chip");
    this.line = $("#place-line");
    this.asks = $("#asks");
    // The place picked for this empty chat; null: this computer.
    this.pending = null;
    this.list = null;
    this.summary = null;
    this.running = false;
    this.chip.addEventListener("click", () => this.menu());
  }

  /// This computer as a sentence names it: "this Mac", "this computer".
  get here() {
    const h = this.list?.here || "This computer";
    return h[0].toLowerCase() + h.slice(1);
  }

  /// The computers there are, read again (a link came or went).
  async load() {
    try {
      this.list = await this.api.call("places");
    } catch {
      this.list = null;
    }
    this.render();
  }

  /// The chat windows show changed, or started or stopped working.
  show(summary, running) {
    const moved = summary?.session?.id !== this.summary?.session?.id;
    this.summary = summary;
    this.running = running;
    if (moved || summary?.place || summary?.session?.turns) this.pending = null;
    this.render();
  }

  /// An empty chat of this computer: its place can still change.
  get open() {
    const s = this.summary;
    return !!s && !s.place && !s.session?.turns && !this.running;
  }

  render() {
    const s = this.summary;
    const chip = this.chip;
    const others = this.list?.places || [];
    if (s?.place) {
      const down = s.place.state !== "ok";
      chip.hidden = false;
      chip.setAttribute("aria-disabled", "true");
      chip.dataset.state = down ? "down" : "ok";
      chip.title = `This chat runs on ${s.place.name}`;
      clear(chip).append(icon("server"), h("span", s.place.name));
    } else if (this.open && (others.length || this.list?.can_add)) {
      chip.hidden = false;
      chip.removeAttribute("aria-disabled");
      delete chip.dataset.state;
      chip.title = "Where this chat runs";
      const name = this.pending || this.list.here;
      clear(chip).append(icon(this.pending ? "server" : "laptop"), h("span", name));
    } else {
      chip.hidden = true;
    }
    this.renderLine();
  }

  menu() {
    if (!this.open || !this.list) return;
    const pick = (name) => {
      this.pending = name;
      el.close();
      this.render();
    };
    const item = (name, here, p) => {
      const down = p && p.state !== "ok";
      const current = (this.pending || null) === (here ? null : name);
      return h("button.menu-item.place-item", {
        type: "button", disabled: down, "aria-current": current ? "true" : null,
        onclick: () => pick(here ? null : name),
      },
        icon(here ? "laptop" : "server"),
        h("span", name),
        down ? h("span.menu-sub", "Not answering") : null);
    };
    const items = [item(this.list.here, true, null)];
    for (const p of this.list.places || []) items.push(item(p.name, false, p));
    if (this.list.can_add) {
      items.push(h("div.menu-sep"),
        h("button.menu-item", { type: "button", onclick: () => { el.close(); this.api.openSettings("computers"); } },
          icon("plus"), h("span", "Add a computer")));
    }
    const el = this.api.popover(this.chip, items, { above: true });
    el.classList.add("places-pop");
  }

  /// Before the first message of an empty chat placed elsewhere: start the chat there, then
  /// send. True when this took the message.
  async beforeSend(text) {
    if (!this.pending || !this.open) return false;
    const place = this.pending;
    this.setBusy(place);
    try {
      let r = await this.api.call("new_session", { place });
      if (r.ask) {
        this.setBusy(null);
        if (!(await this.askSize(place, r.ask))) {
          this.api.compose(text);
          return true;
        }
        this.setBusy(place);
        r = await this.api.call("new_session", { place, big: true });
      }
      // The page rebuilds for the new chat first, so the turn draws once.
      await this.api.shown(r.session?.id);
      const copy = r.copy || {};
      if (copy.busy) this.api.toast(`Work runs in this project on ${place}: ${this.here}'s changes go with the next chat`);
      else if (copy.kept_back?.length) this.api.toast(`${files(copy.kept_back.length)} changed on ${place} too: kept as they are there`);
      await this.api.call("send", { text });
    } catch (e) {
      this.api.toast(e.message || String(e));
      this.api.compose(text);
    } finally {
      this.setBusy(null);
    }
    return true;
  }

  setBusy(place) {
    this.chip.dataset.busy = String(!!place);
    if (place) clear(this.chip).append(icon("server"), h("span", `Getting ${place} ready`));
    else this.render();
  }

  /// The project is bigger than a chat sends without asking.
  askSize(place, ask) {
    return new Promise((resolve) => {
      const done = (yes) => {
        card.remove();
        resolve(yes);
      };
      const card = h("div.approval.place-ask", { role: "alertdialog", "aria-label": `Send to ${place}` },
        h("div.approval-title", `Send ${this.summary?.project?.name || "this project"} to ${place}?`),
        h("div.approval-line", `${size(ask.bytes)} · ${files(ask.files)}`),
        h("div.approval-actions",
          h("button.btn.primary", { type: "button", onclick: () => done(true) }, "Send"),
          h("button.btn", { type: "button", onclick: () => done(false) }, "Cancel")));
      this.asks.prepend(card);
      card.querySelector(".btn.primary").focus();
    });
  }

  // ---- the line above the composer ---------------------------------------------------------

  renderLine(conflicts) {
    const s = this.summary;
    const line = this.line;
    clear(line);
    if (!s?.place) {
      line.hidden = true;
      return;
    }
    const place = s.place.name;
    if (s.place.state !== "ok") {
      line.hidden = false;
      line.dataset.tone = "down";
      line.append(icon("alert"), h("span.place-line-text", `${place} is not answering`),
        h("button.btn.small", { type: "button", onclick: () => this.retry() }, "Try again"));
      return;
    }
    if (conflicts?.length) {
      line.hidden = false;
      line.dataset.tone = "ask";
      line.append(h("div.place-line-text", `Changed here and on ${place}`));
      for (const path of conflicts) {
        line.append(h("div.place-conflict",
          h("span.place-file", path),
          h("button.btn.small", { type: "button", onclick: () => this.bring({ [path]: "mine" }, conflicts) }, `Keep ${this.here}'s`),
          h("button.btn.small", { type: "button", onclick: () => this.bring({ [path]: "theirs" }, conflicts) }, `Take ${place}'s`)));
      }
      return;
    }
    const n = s.changes || 0;
    if (!n || this.running) {
      line.hidden = true;
      return;
    }
    line.hidden = false;
    line.dataset.tone = "changes";
    line.append(icon("server"), h("span.place-line-text", `${files(n)} changed on ${place}`),
      h("button.btn.small.primary", { type: "button", onclick: () => this.bring({}) }, `Bring to ${this.here}`));
  }

  async retry() {
    const id = this.summary?.session?.id;
    if (id) await this.api.call("open_session", { id }).catch((e) => this.api.toast(e.message));
  }

  /// Bring what changed there here; files changed on both sides wait for a choice each.
  async bring(choices, waiting) {
    const btns = this.line.querySelectorAll("button");
    btns.forEach((b) => (b.disabled = true));
    try {
      let r = await this.api.call("place_bring", { choices });
      if (r.ask) {
        if (!(await this.askSize(this.summary.place.name, r.ask))) return this.renderLine(waiting);
        r = await this.api.call("place_bring", { choices, big: true });
      }
      const n = (r.written?.length || 0) + (r.deleted?.length || 0) + (r.kept_mine?.length || 0);
      if (n) this.api.toast(`${files(n)} brought to ${this.here}`);
      if (r.refused?.length) this.api.toast(`${files(r.refused.length)} not taken: ${r.refused[0][1]}`);
      this.renderLine(r.waiting);
    } catch (e) {
      this.api.toast(e.message || String(e));
      this.renderLine(waiting);
    }
  }

  /// A turn on another computer ended there with `count` files that differ from here.
  changed(msg) {
    if (msg.session !== this.summary?.session?.id) return;
    this.summary = { ...this.summary, changes: msg.count };
    this.renderLine();
  }
}

function files(n) {
  return `${n.toLocaleString()} file${n === 1 ? "" : "s"}`;
}

function size(bytes) {
  const mb = bytes / (1024 * 1024);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.max(1, Math.round(mb))} MB`;
}

/// A program's install or sign-in on another computer, in a terminal of its own: the link and
/// the code it prints show here, and what the person types goes there. Nothing of it is kept.
export function programSheet(api, backend, place, done) {
  const verb = backend.step === "install" ? "Install" : "Sign in to";
  return terminalSheet(api, `${verb} ${backend.label}`, `on ${place}`,
    (cols, rows) => api.call("place_setup", { program: backend.provider, step: backend.step, cols, rows }), done);
}

/// A terminal over the page for one program: `open(cols, rows)` starts it and answers its
/// terminal's id. Closing ends it; `done` runs after.
export async function terminalSheet(api, title, sub, open, done) {
  const box = h("div.term-box");
  const close = h("button.icon-btn", { type: "button", "aria-label": "Close", title: "Close" }, icon("x"));
  const sheet = h("div.program-sheet", { role: "dialog", "aria-label": `${title} ${sub}`.trim(), "data-ark-overlay": "" },
    h("div.program-head", h("span", title), h("span.menu-sub", sub), close),
    box);
  const overlay = h("div.program-overlay", sheet);
  document.body.append(overlay);
  const s = { id: null, ref: null, term: null, offs: [] };
  const resource = (kind, input = {}) => {
    if (!s.ref) return Promise.reject(new Error("The terminal has no resource location"));
    return api.resource(s.ref, kind, input);
  };
  const end = () => {
    for (const off of s.offs) off();
    if (s.ref) resource("terminal_close", { session: s.id }).catch(() => {});
    s.term?.dispose();
    overlay.remove();
    done?.();
  };
  close.onclick = end;
  sheet.addEventListener("keydown", (e) => e.key === "Escape" && e.metaKey && end());
  let Terminal;
  try {
    Terminal = await xterm();
  } catch (e) {
    box.append(h("div.wb-empty", e.message));
    return;
  }
  const css = (v) => getComputedStyle(document.documentElement).getPropertyValue(v).trim();
  const term = (s.term = new Terminal({
    cursorBlink: true, fontSize: 12.5, scrollback: 2000,
    fontFamily: css("--mono") || "ui-monospace, Menlo, monospace",
    theme: { background: css("--card") || "#ffffff", foreground: css("--text") || "#1a1a1a", cursor: css("--accent") || "#888", selectionBackground: "rgba(127,127,127,0.3)" },
  }));
  const fit = new window.FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(box);
  try { fit.fit(); } catch {}
  const early = [];
  const output = (p) => (p.exit ? term.write("\r\n\x1b[2m[finished]\x1b[0m\r\n") : term.write(p.data));
  s.offs.push(window.ark.on("ark:term", (p) => {
    if (!s.ref) early.push(p);
    else if (p.ref && JSON.stringify(p.ref) === JSON.stringify(s.ref)) output(p);
  }));
  term.onData((data) => s.ref && resource("terminal_write", { session: s.id, data }).catch(() => {}));
  try {
    const opened = await open(term.cols, term.rows);
    s.id = opened.session;
    s.ref = opened.ref;
    if (!s.ref) throw new Error("The runtime did not return a terminal resource reference");
    for (const p of early.splice(0)) if (p.ref && JSON.stringify(p.ref) === JSON.stringify(s.ref)) output(p);
    term.focus();
  } catch (e) {
    term.write(`\x1b[31m${e.message}\x1b[0m\r\n`);
  }
}
