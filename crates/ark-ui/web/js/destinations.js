// The workspace's stable destinations. Home contains the current project's Chat, Agents and
// Board subviews. Schedules is project-wide. The same controls become a bottom bar when the
// window is narrow, so neither destination is hidden inside the project drawer.

import { $, $$ } from "./dom.js";

export class Destinations {
  constructor(open) {
    this.open = open;
    this.el = $("#destinations");
    this.buttons = $$(".destination", this.el);
    for (const button of this.buttons) {
      button.addEventListener("click", (event) => {
        this.open(button.dataset.destination, { keyboard: event.detail === 0, trigger: button });
      });
    }
  }

  mark(destination) {
    for (const button of this.buttons) {
      if (button.dataset.destination === destination) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
  }
}
