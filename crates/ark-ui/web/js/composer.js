// The composer. Idle: ↩ sends, ⇧↩ adds a line, `/` completes commands. While the agent works:
// ↩ queues the message for after the turn, ⌘↩ sends it into the running turn (it reaches the
// model at its next step), esc stops. Queued messages wait above the composer and can be sent
// in early or removed.

import { h, icon, clear, clip, touch, $ } from "./dom.js";
import { registry } from "./ext.js";
import { chooseFiles } from "./drop.js";
import { label } from "./keys.js";
import { programSheet } from "./places.js";

export class Composer {
  constructor(api) {
    this.api = api;
    this.input = $("#input");
    this.form = $("#composer");
    this.send = $("#send");
    this.queueEl = $("#queue");
    this.slash = $("#slash");
    this.model = $("#model-chip");
    this.queue = [];
    this.running = false;
    this.commands = [];
    this.slashIndex = 0;

    this.send.append(h("span.send-icon.go", icon("arrow-up")), h("span.send-icon.stop", icon("stop")));
    this.input.addEventListener("input", () => this.changed());
    this.input.addEventListener("keydown", (e) => this.key(e));
    this.form.addEventListener("submit", (e) => {
      e.preventDefault();
      if (this.running && !this.input.value.trim()) api.cancel();
      else this.submit(false);
    });
    this.model.addEventListener("click", () => this.modelMenu());
    $("#attach").addEventListener("click", () => chooseFiles(api, this));
    this.changed();
  }

  focus() {
    this.input.focus();
  }

  set(text) {
    this.input.value = text;
    this.changed();
    this.focus();
    this.input.setSelectionRange(text.length, text.length);
  }

  /// Put `text` under the message being written, on a line of its own, and leave the cursor
  /// at the end of the person's words: what they ask stays first (and names the chat).
  addLine(text) {
    const v = this.input.value.replace(/\s+$/, "");
    this.input.value = `${v}\n${text}`;
    this.changed();
    this.focus();
    this.input.setSelectionRange(v.length, v.length);
  }

  changed() {
    const i = this.input;
    i.style.height = "auto";
    i.style.height = `${Math.min(i.scrollHeight, 240)}px`;
    this.form.dataset.empty = String(!i.value.trim());
    this.updateSlash();
  }

  setRunning(on) {
    this.running = on;
    this.form.dataset.running = String(on);
    this.send.setAttribute("aria-label", on ? "Stop" : "Send");
    this.input.placeholder = on ? (touch() ? "Queue a message" : label("Queue a message · ⌘↩ sends it now")) : this.placeholder || "Ask anything";
  }

  /// Name the model on its button: the app's words when it draws the picker. With nothing
  /// chosen yet the button asks for a choice, and sending opens the menu.
  setModel(desc) {
    let label = desc || "model";
    let missing = false;
    const p = registry.modelPicker;
    if (p) {
      try {
        const l = p.label(desc);
        label = typeof l === "string" ? l : l?.text || "";
        missing = typeof l === "object" && !!l?.missing;
      } catch (e) {
        console.error(e);
      }
    }
    this.model.textContent = label;
    this.model.dataset.missing = String(missing);
    this.missing = missing;
  }

  key(e) {
    if (!this.slash.hidden) {
      const items = [...this.slash.querySelectorAll(".slash-item")];
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        this.slashIndex = (this.slashIndex + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
        this.paintSlash();
        return;
      }
      if ((e.key === "Tab" || e.key === "Enter") && items.length && !e.shiftKey) {
        const pick = items[this.slashIndex]?.dataset.name;
        const typed = this.input.value.trim();
        if (pick && (e.key === "Tab" || typed !== `/${pick}`)) {
          e.preventDefault();
          this.set(`/${pick} `);
          return;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        this.slash.hidden = true;
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      this.submit(e.metaKey || e.ctrlKey);
    }
  }

  submit(now) {
    const text = this.input.value.trim();
    if (!text) return;
    this.input.value = "";
    this.changed();
    if (!this.running) return this.api.submit(text);
    if (text.startsWith("/")) {
      this.enqueue(text);
      return;
    }
    if (now) this.api.steer(text);
    else this.enqueue(text);
  }

  enqueue(text) {
    this.queue.push(text);
    this.paintQueue();
  }

  /// A message sent into the running turn, shown here until the agent reads it.
  steering(text) {
    (this.steers ||= []).push(text);
    this.paintQueue();
  }

  delivered(text) {
    const i = (this.steers || []).indexOf(text);
    if (i >= 0) this.steers.splice(i, 1);
    this.paintQueue();
  }

  clearSteering() {
    this.steers = [];
    this.paintQueue();
  }

  /// Messages to send after the turn: `first` (steering that came too late) goes ahead.
  takeNext(first = []) {
    this.queue = [...first, ...this.queue];
    const next = this.queue.shift();
    this.paintQueue();
    return next;
  }

  paintQueue() {
    clear(this.queueEl);
    for (const [i, q] of (this.steers || []).entries()) {
      this.queueEl.append(h("div.queued.steer", { style: { "--i": i } },
        h("span.queued-k", "Next step"),
        h("span.queued-t", clip(q, 120))));
    }
    this.queue.forEach((q, i) => {
      const item = h("div.queued", { style: { "--i": i } },
        h("span.queued-k", "Queued"),
        h("span.queued-t", clip(q, 120)),
        !q.startsWith("/") && this.running
          ? h("button.link", { title: "Send into the running turn", onclick: () => { this.queue.splice(i, 1); this.paintQueue(); this.api.steer(q); } }, "Send now")
          : null,
        h("button.icon-btn.small", { title: "Remove", "aria-label": "Remove", onclick: () => { this.queue.splice(i, 1); this.paintQueue(); } }, icon("x")));
      this.queueEl.append(item);
    });
  }

  // ---- slash completion ----------------------------------------------------------------------

  updateSlash() {
    const v = this.input.value;
    const m = v.match(/^\/(\S*)$/);
    if (!m) {
      this.slash.hidden = true;
      return;
    }
    const q = m[1].toLowerCase();
    const items = this.commands.filter((c) => c.name.startsWith(q) || c.aliases.some((a) => a.startsWith(q)));
    if (!items.length) {
      this.slash.hidden = true;
      return;
    }
    clear(this.slash);
    for (const c of items.slice(0, 12)) {
      const row = h("div.slash-item", { "data-name": c.name },
        h("span.slash-name", `/${c.name}`, c.usage ? h("span.slash-usage", ` ${c.usage}`) : null),
        h("span.slash-sum", c.summary));
      row.addEventListener("mousedown", (e) => {
        e.preventDefault();
        this.set(`/${c.name} `);
      });
      this.slash.append(row);
    }
    this.slashIndex = Math.min(this.slashIndex, items.length - 1);
    this.slash.hidden = false;
    this.paintSlash();
  }

  paintSlash() {
    [...this.slash.children].forEach((c, i) => c.setAttribute("aria-selected", String(i === this.slashIndex)));
  }

  // ---- model ---------------------------------------------------------------------------------

  /// Models grouped by backend: subscriptions, local Ollama, and APIs that have a key. A backend
  /// that is not ready shows the command that fixes it. The list is fetched on every open, so
  /// signing in elsewhere shows up the next time the menu opens.
  async modelMenu() {
    const open = document.querySelector(".popover.models-pop.open");
    if (open) return open.close();
    const picker = registry.modelPicker;
    if (picker) {
      const el = h("div.model-picker");
      const menu = this.api.popover(this.model, [el], { above: true });
      menu.classList.add("models-pop");
      try {
        // Closing returns to the message, so Enter sends what waited for a model.
        picker.render(el, { close: () => { menu.close(); this.focus(); } });
      } catch (e) {
        console.error(e);
        el.append(h("div.menu-sub.pad", String(e)));
      }
      return;
    }
    const body = h("div.models");
    const input = h("input.menu-input", { placeholder: "Other model, e.g. ollama:qwen3:4b", "aria-label": "Model" });
    const menu = this.api.popover(this.model, [body, h("div.menu-sep"), input], { above: true });
    menu.classList.add("models-pop");
    const pick = async (spec) => {
      menu.close();
      try {
        await this.api.call("model", { spec });
      } catch (err) {
        this.api.toast(err.message);
      }
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && input.value.trim()) pick(input.value.trim());
    });
    // A chat on another computer: its models, keys and programs are that computer's.
    const place = this.api.summary()?.place?.name || null;
    if (this.backendsFor !== place) this.backends = null;
    this.backendsFor = place;
    const paint = (backends) => {
      clear(body);
      const current = this.model.textContent;
      for (const b of backends) {
        if (b.kind === "api" && !b.ready && !this.canSaveKeys && !place) continue;
        const group = h("div.model-group",
          h("div.model-head",
            h("span.model-dot", { "data-ok": String(b.ready) }),
            h("span.model-name", b.label),
            h("span.menu-sub", b.detail)));
        if (b.ready) {
          for (const m of b.models) {
            const here = current === `${b.provider} ${m}`;
            group.append(h("button.menu-item.model-item", { "aria-current": here ? "true" : null, onclick: () => !here && pick(`${b.provider}:${m}`) }, h("span", m)));
          }
        } else if (b.kind === "api") {
          // A key pasted here is checked with the provider and kept in the system's keychain.
          const key = h("input.menu-input.model-key", { type: "password", placeholder: `Paste your ${b.label} key`, "aria-label": `${b.label} key`, autocomplete: "off" });
          const why = h("div.menu-sub.model-key-why");
          key.addEventListener("keydown", async (e) => {
            if (e.key !== "Enter" || !key.value.trim()) return;
            e.stopPropagation();
            key.disabled = true;
            why.textContent = "Checking the key…";
            try {
              if (place) await this.api.call("place_key", { provider: b.provider, key: key.value.trim() });
              else await this.api.call("api_key_save", { provider: b.provider, key: key.value.trim() });
              this.backends = await this.api.call("models");
              paint(this.backends);
            } catch (err) {
              key.disabled = false;
              why.textContent = err.message;
              key.select();
            }
          });
          group.append(h("div.model-fix", key), why);
          // The key this computer keeps goes there only when the person asks.
          if (place && this.savedKeys?.includes(b.provider)) {
            group.append(h("div.model-fix", h("button.btn.small", { type: "button", onclick: async (e) => {
              e.target.disabled = true;
              why.textContent = "Checking the key…";
              try {
                await this.api.call("place_key", { provider: b.provider });
                this.backends = await this.api.call("models");
                paint(this.backends);
              } catch (err) {
                e.target.disabled = false;
                why.textContent = err.message;
              }
            } }, `Use ${this.api.hereName?.() || "this computer"}'s key`)));
          }
        } else if (place && b.step) {
          // The program's own step, run there in a terminal shown here.
          group.append(h("div.model-fix", h("button.btn.small.primary", { type: "button", onclick: () => {
            menu.close();
            programSheet(this.api, b, place, () => { this.backends = null; });
          } }, b.step === "install" ? "Install" : "Sign in")));
        } else if (b.fix) {
          group.append(h("div.model-fix",
            h("code", b.fix),
            h("button.icon-btn.small", { title: "Copy", "aria-label": "Copy command", onclick: () => navigator.clipboard?.writeText(b.fix).then(() => this.api.toast("Copied. Run it in a terminal, then reopen this menu.")) }, icon("copy"))));
        }
        body.append(group);
      }
    };
    if (this.backends) paint(this.backends);
    else body.append(h("div.menu-sub.pad", "Checking models…"));
    try {
      let keys;
      [this.backends, keys] = await Promise.all([
        this.api.call("models"),
        this.api.call("api_keys").catch(() => ({})),
      ]);
      this.canSaveKeys = !!keys.can_save;
      this.savedKeys = keys.saved || [];
      paint(this.backends);
    } catch (err) {
      if (!this.backends) {
        clear(body);
        body.append(h("div.menu-sub.pad", err.message));
      }
    }
    input.focus();
  }
}
