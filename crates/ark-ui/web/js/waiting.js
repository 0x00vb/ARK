// Work that waits for the person, in any project (ADR 0043): a failure or a result posted into
// a chat nobody was in. They sit in the tray with the questions, one row each. Opening a row
// goes where the work lives and clears it; the x clears it in place. The list is the host's
// (`attention_all`, the terminal's `/waiting`); nothing here is kept that could drift from it.
// This runtime's questions are the tray's cards (`approval` events). What waits on a linked
// runtime (step 4: the person's own server) comes in the same list, named: its questions are
// cards drawn here and answered there, its rows open there.

import { h, icon, clip, $ } from "./dom.js";
import { approvalCard } from "./conversation.js";

// Rows shown before "N more".
const SHOWN = 3;

/// An item's key: its id is its runtime's.
const key = (it) => (it.runtime ? `${it.runtime}:${it.id}` : it.id);

export class Waiting {
  /// `chat` says which chat the window shows.
  constructor(api, transport, chat) {
    this.api = api;
    this.transport = transport;
    this.box = h("div.waiting", { role: "list" });
    this.cards = h("div.waiting-cards");
    this.items = [];
    this.questions = [];
    this.notes = [];
    // Linked runtimes not answering: their items show as last read, not to be acted on.
    this.away = new Set();
    this.all = false;
    // Rows opened here that have no chat: their text shows under the row until closed.
    this.open = new Map();
    this.gen = 0;
    this.chat = chat;
    document.addEventListener("visibilitychange", () => {
      const chat = this.chat();
      if (chat && this.items.some((i) => i.chat === chat && !i.runtime)) this.seen(chat);
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
    let all;
    try {
      all = await this.transport.call("attention_all");
    } catch {
      return; // keep what is drawn; the next change reads again
    }
    if (gen !== this.gen) return;
    const items = all?.items || [];
    this.items = items.filter((i) => i.kind !== "question");
    this.questions = items.filter((i) => i.kind === "question" && i.runtime);
    this.notes = all?.notes || [];
    this.away = new Set(all?.away || []);
    this.draw();
  }

  draw() {
    const tray = $("#asks");
    const focused = document.activeElement?.closest?.(".wait-row")?.dataset.id;
    this.box.replaceChildren();
    const rows = [...this.items];
    // An opened row stays until the person closes it, even once the host has cleared it.
    for (const [k, o] of this.open) if (!rows.some((r) => key(r) === k)) rows.unshift(o);
    const shown = this.all ? rows : rows.slice(0, SHOWN);
    for (const n of this.notes) this.box.append(h("div.wait-note", { role: "listitem" }, n));
    for (const it of shown) this.box.append(this.row(it));
    if (rows.length > SHOWN) {
      this.box.append(h("button.wait-more", { type: "button", onclick: () => { this.all = !this.all; this.draw(); } },
        this.all ? "Show less" : `${rows.length - SHOWN} more`));
    }
    if (rows.length || this.notes.length) {
      // Above the questions: the card waiting for an answer stays nearest the composer.
      if (this.box.parentNode !== tray) tray.prepend(this.box);
    } else this.box.remove();
    this.drawCards(tray);
    if (focused) this.box.querySelector(`.wait-row[data-id="${CSS.escape(focused)}"]`)?.focus();
  }

  /// Questions waiting on a linked runtime: cards like this runtime's, saying where, answered
  /// there. A card answered there meanwhile goes with the next read.
  drawCards(tray) {
    const want = new Set(this.questions.map(key));
    for (const c of [...this.cards.children]) if (!want.has(c.dataset.id)) c.remove();
    for (const q of this.questions) {
      const drawn = this.cards.querySelector(`.approval[data-id="${CSS.escape(key(q))}"]`);
      if (drawn) {
        this.mark(drawn, q);
        continue;
      }
      const card = approvalCard({
        id: key(q),
        title: q.agent ? `${q.agent} wants to:` : null,
        line: q.title,
        choices: q.choices,
        request: { agent: q.agent || q.runtime },
      }, async (_id, decision) => {
        try {
          await this.transport.call("attention_answer", { runtime: q.runtime, id: q.id, decision });
        } catch (e) {
          // Not answered (the other computer stopped answering): the card stays.
          card.classList.remove("decided");
          for (const b of card.querySelectorAll("button")) b.disabled = false;
          this.api.toast(e.message);
        }
      });
      card.prepend(h("div.approval-chat.approval-where"));
      this.mark(card, q);
      this.cards.append(card);
    }
    if (this.questions.length) {
      if (this.cards.parentNode !== tray) tray.append(this.cards);
    } else this.cards.remove();
  }

  /// Where a card from a linked runtime is; while that runtime does not answer, that instead,
  /// and its choices wait.
  mark(card, q) {
    const away = this.away.has(q.runtime);
    card.classList.toggle("away", away);
    card.querySelector(".approval-where").textContent = away
      ? `${q.runtime} is not answering`
      : `In ${q.runtime} · ${q.place.slice(q.runtime.length + 3).split(" · ")[0]}`;
    if (!card.classList.contains("decided")) for (const b of card.querySelectorAll("button")) b.disabled = away;
  }

  row(it) {
    const k = key(it);
    const away = this.away.has(it.runtime);
    const opened = this.open.get(k);
    const row = h("div.wait-row", {
      role: "listitem", tabindex: away ? null : "0", "data-id": k, "data-kind": it.kind,
      "aria-disabled": away ? "true" : null,
      title: `${it.title}\n${it.place}`, "aria-expanded": opened ? "true" : null,
    },
      h("span.wait-mark", icon(it.kind === "failed" ? "alert" : "check")),
      h("span.wait-text",
        h("span.wait-title", clip(it.title, 90)),
        h("span.wait-place", away ? `${it.runtime} is not answering` : it.place)),
      away ? h("span") : h("button.icon-btn.small.wait-clear", {
        type: "button", title: "Clear", "aria-label": `Clear: ${it.title}`,
        onclick: (e) => { e.stopPropagation(); this.clear(it); },
      }, icon("x")),
      opened?.text ? h("div.wait-detail", opened.text) : null);
    if (away) return row;
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
    const card = document.querySelector(`#asks .approval[data-id="${CSS.escape(id)}"]`) || document.querySelector("#asks .approval");
    card?.scrollIntoView({ block: "nearest" });
  }

  /// Go where the work lives: its chat, where the post shows it. Work without a chat shows its
  /// text under the row. Work on another computer opens there in a window that can; else its
  /// words show under the row.
  async go(it) {
    const k = key(it);
    if (this.open.has(k)) {
      this.open.delete(k);
      return this.draw();
    }
    if (it.runtime) {
      try {
        return await this.transport.call("attention_go", { runtime: it.runtime, id: it.id });
      } catch {
        // No window to open it in: its words, here.
      }
    }
    let item;
    try {
      item = await this.transport.call("attention_open", { runtime: it.runtime || "", id: it.id });
    } catch (e) {
      this.api.toast(e.message);
      return this.refresh();
    }
    if (item.chat && !it.runtime) {
      this.items = this.items.filter((x) => key(x) !== k);
      this.draw();
      this.api.openSession(item.chat);
    } else {
      this.open.set(k, { ...it, text: item.text });
      this.draw();
    }
  }

  async clear(it) {
    const k = key(it);
    // A row cleared from the keyboard hands focus to the next one, so Delete clears the next.
    const rows = [...this.box.querySelectorAll(".wait-row")];
    const at = rows.findIndex((r) => r.dataset.id === k);
    const had = at >= 0 && rows[at].contains(document.activeElement);
    this.open.delete(k);
    const listed = this.items.some((x) => key(x) === k);
    this.items = this.items.filter((x) => key(x) !== k);
    this.draw();
    if (had) {
      const left = this.box.querySelectorAll(".wait-row");
      left[Math.min(at, left.length - 1)]?.focus();
    }
    // An opened row was cleared when it opened.
    if (!listed) return;
    try {
      await this.transport.call("attention_clear", { runtime: it.runtime || "", id: it.id });
    } catch {
      this.refresh();
    }
  }
}
