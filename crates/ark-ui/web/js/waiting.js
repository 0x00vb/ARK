// Work that waits for the person, in any project (ADR 0043): a failure or a result posted into
// a chat nobody was in. They sit in the tray with the questions, one row each. Opening a row
// goes where the work lives and clears it; the x clears it in place. The list is the host's
// (`attention`, the terminal's `/waiting`); nothing here is kept that could drift from it.
// Questions are not drawn here: they are the tray's cards.

import { h, icon, clip, $ } from "./dom.js";

// Rows shown before "N more".
const SHOWN = 3;

export class Waiting {
  /// `chat` says which chat the window shows.
  constructor(api, transport, chat) {
    this.api = api;
    this.transport = transport;
    this.box = h("div.waiting", { role: "list" });
    this.items = [];
    this.all = false;
    // Rows opened here that have no chat: their text shows under the row until closed.
    this.open = new Map();
    this.gen = 0;
    this.chat = chat;
    document.addEventListener("visibilitychange", () => {
      const chat = this.chat();
      if (chat && this.items.some((i) => i.chat === chat)) this.seen(chat);
    });
  }

  /// The person sees this chat: what was posted there no longer waits. Only while the page is
  /// in view; a hidden tab says so when it comes back (see the constructor).
  seen(chat) {
    if (document.visibilityState !== "visible") return;
    this.transport.call("attention_seen", { session: chat }).catch(() => {});
  }

  async refresh() {
    const gen = ++this.gen;
    let items;
    try {
      items = await this.transport.call("attention");
    } catch {
      return; // keep what is drawn; the next change reads again
    }
    if (gen !== this.gen) return;
    this.items = (items || []).filter((i) => i.kind !== "question");
    this.draw();
  }

  draw() {
    const tray = $("#asks");
    const focused = document.activeElement?.closest?.(".wait-row")?.dataset.id;
    this.box.replaceChildren();
    const rows = [...this.items];
    // An opened row stays until the person closes it, even once the host has cleared it.
    for (const [id, o] of this.open) if (!rows.some((r) => r.id === id)) rows.unshift(o);
    const shown = this.all ? rows : rows.slice(0, SHOWN);
    for (const it of shown) this.box.append(this.row(it));
    if (rows.length > SHOWN) {
      this.box.append(h("button.wait-more", { type: "button", onclick: () => { this.all = !this.all; this.draw(); } },
        this.all ? "Show less" : `${rows.length - SHOWN} more`));
    }
    if (rows.length) {
      // Above the questions: the card waiting for an answer stays nearest the composer.
      if (this.box.parentNode !== tray) tray.prepend(this.box);
    } else this.box.remove();
    if (focused) this.box.querySelector(`.wait-row[data-id="${CSS.escape(focused)}"]`)?.focus();
  }

  row(it) {
    const opened = this.open.get(it.id);
    const row = h("div.wait-row", {
      role: "listitem", tabindex: "0", "data-id": it.id, "data-kind": it.kind,
      title: `${it.title}\n${it.place}`, "aria-expanded": opened ? "true" : null,
    },
      h("span.wait-mark", icon(it.kind === "failed" ? "alert" : "check")),
      h("span.wait-text",
        h("span.wait-title", clip(it.title, 90)),
        h("span.wait-place", it.place)),
      h("button.icon-btn.small.wait-clear", {
        type: "button", title: "Clear", "aria-label": `Clear: ${it.title}`,
        onclick: (e) => { e.stopPropagation(); this.clear(it); },
      }, icon("x")),
      opened?.text ? h("div.wait-detail", opened.text) : null);
    row.addEventListener("click", () => this.go(it));
    row.addEventListener("keydown", (e) => {
      if (e.target !== row) return;
      if (e.key === "Enter") { e.preventDefault(); this.go(it); }
      else if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); this.clear(it); }
    });
    return row;
  }

  /// The item a notification was about: a question shows as its card (its project opens when
  /// it is another one); a result or a failure opens as a row does. One answered or cleared
  /// meanwhile says so.
  async openById(id) {
    const items = (await this.transport.call("attention").catch(() => null)) || [];
    const it = items.find((x) => x.id === id);
    if (!it) return this.api.toast("That was answered or cleared already.");
    if (it.kind !== "question") return this.go(it);
    if (it.project !== this.api.project()?.id) await this.api.openProject(it.project);
    document.querySelector("#asks .approval")?.scrollIntoView({ block: "nearest" });
  }

  /// Go where the work lives: its chat, where the post shows it. Work without a chat shows its
  /// text under the row.
  async go(it) {
    if (this.open.has(it.id)) {
      this.open.delete(it.id);
      return this.draw();
    }
    let item;
    try {
      item = await this.transport.call("attention_open", { id: it.id });
    } catch (e) {
      this.api.toast(e.message);
      return this.refresh();
    }
    if (item.chat) {
      this.items = this.items.filter((x) => x.id !== it.id);
      this.draw();
      this.api.openSession(item.chat);
    } else {
      this.open.set(it.id, item);
      this.draw();
    }
  }

  async clear(it) {
    // A row cleared from the keyboard hands focus to the next one, so Delete clears the next.
    const rows = [...this.box.querySelectorAll(".wait-row")];
    const at = rows.findIndex((r) => r.dataset.id === it.id);
    const had = at >= 0 && rows[at].contains(document.activeElement);
    this.open.delete(it.id);
    const listed = this.items.some((x) => x.id === it.id);
    this.items = this.items.filter((x) => x.id !== it.id);
    this.draw();
    if (had) {
      const left = this.box.querySelectorAll(".wait-row");
      left[Math.min(at, left.length - 1)]?.focus();
    }
    // An opened row was cleared when it opened.
    if (!listed) return;
    try {
      await this.transport.call("attention_clear", { id: it.id });
    } catch {
      this.refresh();
    }
  }
}
