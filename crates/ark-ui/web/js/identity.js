// Who the person is talking to. In the title bar, the chat's face opens the side pane with the
// face, the name, a short description, the model, the chat's title and its folder; the title
// beside it, with a chevron, opens the recent chats with the same agent.
//
// An identity is plain data an app or the chat itself supplies:
//   { name, title?, description?, avatar(size) -> Node,
//     model?: { label, mark?() -> Node, disabled?, open(anchor) },
//     rename?(title) -> Promise, folder?: { name, path?, open?(anchor) },
//     agent?: { name, project, conversation, program } }
// `title` is what the chip says (the chat's title; `name` when it has none).

import { h, icon, clear } from "./dom.js";

export const DETAILS = "chat-details";

export class Identity {
  /// `recent(anchor, { keyboard })` opens the recent chats menu under the title.
  constructor(api, current, recent) {
    this.api = api;
    this.current = current;
    this.face = h("button.identity-face-btn", { type: "button", id: "chat-identity", "aria-expanded": "false", onclick: () => this.toggle() });
    this.titleButton = h("button.identity-title-btn", {
      type: "button", "aria-haspopup": "menu", "aria-expanded": "false",
      onclick: (event) => this.openRecent(event.detail === 0),
      onkeydown: (event) => {
        if (event.key !== "ArrowDown") return;
        event.preventDefault();
        this.openRecent(true);
      },
    });
    this.recent = recent;
    this.chip = h("div.identity-chip", this.face, this.titleButton);
    // The chip's own double-click is not a rename: the title it replaces renames on double-click.
    this.chip.addEventListener("dblclick", (event) => event.stopPropagation());
    this.redraws = new Set();
  }

  /// The chip for `identity`, to sit where the title text was; null when the view has none.
  draw(identity) {
    if (!identity) {
      if (this.api.panelOpen?.(DETAILS)) this.api.closePanel(DETAILS);
      this.chip.remove();
      for (const redraw of this.redraws) redraw(null);
      return null;
    }
    const label = identity.title || identity.name;
    clear(this.face).append(identity.avatar(22));
    this.face.setAttribute("aria-label", `${identity.name}: details`);
    this.face.setAttribute("aria-expanded", String(!!this.api.panelOpen?.(DETAILS)));
    this.face.title = identity.name;
    clear(this.titleButton).append(h("span.identity-name", label), icon("chevron-down"));
    this.titleButton.setAttribute("aria-label", `${label}: recent chats`);
    for (const redraw of this.redraws) redraw(identity);
    return this.chip;
  }

  openRecent(keyboard) {
    this.titleButton.setAttribute("aria-expanded", "true");
    const closed = () => this.titleButton.setAttribute("aria-expanded", "false");
    if (!this.recent?.(this.titleButton, { keyboard, onClose: closed })) closed();
  }

  toggle() {
    const open = this.api.panelOpen?.(DETAILS);
    if (open) this.api.closePanel(DETAILS);
    else this.api.openPanel(DETAILS);
    queueMicrotask(() => this.face.setAttribute("aria-expanded", String(!!this.api.panelOpen?.(DETAILS))));
  }

  /// The pane's content: face, name, description, then the model, the title and the folder as rows.
  panel(el) {
    el.classList.add("identity-panel");
    let drawn = "";
    let latest = null;
    const draw = (identity) => {
      latest = identity;
      // Same data, same pane: a redraw would drop the focus on a row and detach its open menu.
      const model = identity?.model;
      const folder = identity?.folder;
      const sig = identity ? JSON.stringify([identity.name, identity.description, model?.label, model?.disabled, model?.disabledReason, !!model?.mark,
        identity.title, !!identity.rename, folder?.name, folder?.path, !!folder?.open]) : "";
      if (sig === drawn && el.firstChild) return;
      drawn = sig;
      const focused = el.contains(document.activeElement) ? document.activeElement.dataset.row : null;
      clear(el);
      if (!identity) return;
      const title = identity.title || identity.name;
      el.append(...[
        h("div.identity-face", identity.avatar(72)),
        h("h2.identity-title", identity.name),
        identity.description ? h("p.identity-description", identity.description) : null,
        model ? h("button.identity-row", {
          type: "button", "aria-haspopup": "menu", disabled: !!model.disabled, title: model.disabled ? model.disabledReason : null, "data-row": "model",
          onclick: (event) => latest?.model?.open(event.currentTarget),
        },
          h("span.identity-row-label", "Model"),
          h("span.identity-row-value", model.mark?.() || null, h("span", model.label), icon("chevron-down"))) : null,
        h("button.identity-row", {
          type: "button", disabled: !identity.rename, "data-row": "title", "aria-label": `Title: ${title}${identity.rename ? ", rename" : ""}`,
          onclick: (event) => this.rename(event.currentTarget, latest),
        }, h("span.identity-row-label", "Title"), h("span.identity-row-value", h("span", title))),
        folder ? h("button.identity-row", {
          type: "button", "aria-haspopup": folder.open ? "menu" : null, disabled: !folder.open, title: folder.path || null, "data-row": "folder",
          onclick: (event) => latest?.folder?.open?.(event.currentTarget),
        },
          h("span.identity-row-label", "Folder"),
          h("span.identity-row-value", h("span", folder.name), folder.open ? icon("chevron-down") : null)) : null,
      ].filter(Boolean));
      if (focused) el.querySelector(`[data-row="${focused}"]`)?.focus({ preventScroll: true });
    };
    this.redraws.add(draw);
    draw(this.current());
    return { dispose: () => this.redraws.delete(draw) };
  }

  /// The title row turns into a field: Enter or leaving it saves, Escape keeps the old title.
  rename(row, identity) {
    if (!identity?.rename) return;
    const before = identity.title || identity.name;
    const input = h("input.identity-input", { value: before, "aria-label": "Title", maxlength: "120" });
    const editing = h("div.identity-row.editing", h("span.identity-row-label", "Title"), input);
    row.replaceWith(editing);
    input.focus();
    input.select();
    let done = false;
    const finish = async (save) => {
      if (done) return;
      done = true;
      const text = input.value.trim();
      const value = row.querySelector(".identity-row-value > span");
      const changed = save && text && text !== before;
      if (changed) value.textContent = text;
      editing.replaceWith(row);
      row.focus({ preventScroll: true });
      if (!changed) return;
      try {
        await identity.rename(text);
      } catch (error) {
        value.textContent = before;
        this.api.toast(error.message || String(error));
      }
    };
    input.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "Enter") { event.preventDefault(); void finish(true); }
      if (event.key === "Escape") void finish(false);
    });
    input.addEventListener("blur", () => void finish(true));
  }
}
