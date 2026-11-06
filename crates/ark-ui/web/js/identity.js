// Who the person is talking to. A chip in the title bar shows the chat's face and name; it opens
// the side pane with the face, the name, a short description and the model, and nothing else.
//
// An identity is plain data an app or the chat itself supplies:
//   { name, title?, description?, avatar(size) -> Node, model?: { label, mark?() -> Node, disabled?, open(anchor) } }
// `title` is what the chip says (the chat's title; `name` when it has none).

import { h, icon, clear } from "./dom.js";

export const DETAILS = "chat-details";

export class Identity {
  constructor(api, current) {
    this.api = api;
    this.current = current;
    this.chip = h("button.identity-chip", { type: "button", id: "chat-identity", "aria-expanded": "false", onclick: () => this.toggle() });
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
    clear(this.chip);
    this.chip.append(identity.avatar(22), h("span.identity-name", label));
    this.chip.setAttribute("aria-label", `${label}: details`);
    this.chip.setAttribute("aria-expanded", String(!!this.api.panelOpen?.(DETAILS)));
    for (const redraw of this.redraws) redraw(identity);
    return this.chip;
  }

  toggle() {
    const open = this.api.panelOpen?.(DETAILS);
    if (open) this.api.closePanel(DETAILS);
    else this.api.openPanel(DETAILS);
    queueMicrotask(() => this.chip.setAttribute("aria-expanded", String(!!this.api.panelOpen?.(DETAILS))));
  }

  /// The pane's content: face, name, description, and the model as one row.
  panel(el) {
    el.classList.add("identity-panel");
    let drawn = "";
    let latest = null;
    const draw = (identity) => {
      latest = identity;
      // Same data, same pane: a redraw would drop the focus on the model row and detach its open menu.
      const model = identity?.model;
      const sig = identity ? JSON.stringify([identity.name, identity.description, model?.label, model?.disabled, model?.disabledReason, !!model?.mark]) : "";
      if (sig === drawn && el.firstChild) return;
      drawn = sig;
      clear(el);
      if (!identity) return;
      el.append(...[
        h("div.identity-face", identity.avatar(72)),
        h("h2.identity-title", identity.name),
        identity.description ? h("p.identity-description", identity.description) : null,
        model ? h("button.identity-row", {
          type: "button", "aria-haspopup": "menu", disabled: !!model.disabled, title: model.disabled ? model.disabledReason : null,
          onclick: (event) => latest?.model?.open(event.currentTarget),
        },
          h("span.identity-row-label", "Model"),
          h("span.identity-row-value", model.mark?.() || null, h("span", model.label), icon("chevron-down"))) : null,
      ].filter(Boolean));
    };
    this.redraws.add(draw);
    draw(this.current());
    return { dispose: () => this.redraws.delete(draw) };
  }
}
