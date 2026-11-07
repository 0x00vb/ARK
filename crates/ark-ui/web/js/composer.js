// The composer. Idle: ↩ sends, ⇧↩ adds a line, `/` completes commands. While the agent works:
// ↩ sends the message into the running turn (it reaches the model at its next step), ⌘↩ queues
// it for after the turn, esc stops. Queued messages wait above the composer and can be sent
// in early or removed.

import { h, icon, clear, clip, touch, $ } from "./dom.js";
import { registry, viewForCommand } from "./ext.js";
import { chooseFiles } from "./drop.js";
import { label } from "./keys.js";
import { programSheet } from "./places.js";
import { DictationController } from "./dictation.js";

let composerNumber = 0;

const REMOTE_COMMANDS = new Set(["new", "resume", "project", "sessions", "help", "waiting"]);

export class Composer {
  constructor(api, elements = {}) {
    this.api = api;
    this.input = elements.input || $("#input");
    this.form = elements.form || $("#composer");
    this.send = elements.send || $("#send");
    this.queueEl = elements.queue || $("#queue");
    this.slash = elements.slash || $("#slash");
    // `null` leaves a control out (an app's conversation has no model or files).
    this.model = "model" in elements ? elements.model : $("#model-chip");
    this.attach = "attach" in elements ? elements.attach : $("#attach");
    const dictationId = `dictation-status-${++composerNumber}`;
    this.mic = elements.mic || this.form.querySelector(".dictate") || h("button.icon-btn.dictate", { type: "button", "aria-label": "Start dictation" });
    this.dictationStatus = elements.dictationStatus || this.form.querySelector(".dictation-status") || h("div.dictation-status", { role: "status", "aria-live": "polite", hidden: true });
    if (!this.dictationStatus.id) this.dictationStatus.id = dictationId;
    this.mic.setAttribute("aria-describedby", this.dictationStatus.id);
    if (!this.mic.isConnected) this.attach ? this.attach.after(this.mic) : this.form.querySelector(".composer-bar").prepend(this.mic);
    if (!this.dictationStatus.isConnected) this.input.after(this.dictationStatus);
    this.queue = [];
    this.running = false;
    this.commands = [];
    this.slashIndex = 0;
    this.caret = this.input.selectionEnd || 0;

    this.send.append(h("span.send-icon.go", icon("arrow-up")), h("span.send-icon.stop", icon("stop")));
    this.input.addEventListener("input", () => this.changed());
    for (const event of ["input", "select", "keyup", "pointerup", "focus"]) this.input.addEventListener(event, () => { this.caret = this.input.selectionEnd ?? this.input.value.length; });
    this.input.addEventListener("keydown", (e) => this.key(e));
    this.form.addEventListener("submit", (e) => {
      e.preventDefault();
      if (this.dictation.beforeSubmit()) return;
      if (this.running && !this.input.value.trim()) api.cancel();
      else this.submit(false);
    });
    this.model?.addEventListener("click", () => elements.modelOpen ? elements.modelOpen(this.model, () => this.focus()) : this.modelMenu());
    this.attach?.addEventListener("click", () => chooseFiles(api, elements.captureDraft, { captureOrigin: elements.captureOrigin, progressTarget: elements.uploads }));
    this.dictation = new DictationController({
      api,
      button: this.mic,
      status: this.dictationStatus,
      insert: (text) => this.insertDictation(text),
      beginLive: () => this.beginLive(),
      live: (text) => this.showLive(text),
      endLive: () => this.endLive(),
    });
    this.changed();
  }

  /// Words heard so far stand in the draft where the transcript will land. Typing waits until
  /// Stop, so nothing the person types is overwritten by the next preview.
  beginLive() {
    this.live = { at: Math.max(0, Math.min(this.caret ?? this.input.value.length, this.input.value.length)), text: "" };
    this.input.readOnly = true;
  }

  /// Where the interim words are now, or -1 when the draft no longer holds them.
  liveAt() {
    const live = this.live;
    if (!live?.text) return live ? live.at : -1;
    const value = this.input.value;
    if (value.startsWith(live.text, live.at)) return live.at;
    return value.indexOf(live.text);
  }

  /// Replace the interim words, or the empty spot where they will go, with `text`.
  writeLive(text) {
    const live = this.live;
    const at = this.liveAt();
    if (at < 0) { live.text = ""; return false; }
    const value = this.input.value;
    const before = value.slice(0, at);
    const after = value.slice(at + live.text.length);
    const left = text && before && !/\s$/.test(before) ? " " : "";
    const right = text && after && !/^\s/.test(after) ? " " : "";
    const written = text ? `${left}${text}${right}` : "";
    this.input.value = before + written + after;
    live.at = at;
    live.text = written;
    this.caret = at + written.length;
    this.changed();
    return true;
  }

  showLive(text) {
    if (this.live) this.writeLive(String(text).trim());
  }

  endLive() {
    if (!this.live) return;
    this.writeLive("");
    this.live = null;
    this.input.readOnly = false;
  }

  insertDictation(text) {
    const returnToInput = document.activeElement === this.mic || document.activeElement === document.body;
    const words = String(text).trim();
    if (this.live && this.writeLive(words)) {
      this.live = null;
      this.input.readOnly = false;
    } else {
      this.live = null;
      this.input.readOnly = false;
      const at = Math.max(0, Math.min(this.caret ?? this.input.value.length, this.input.value.length));
      const before = this.input.value.slice(0, at);
      const after = this.input.value.slice(at);
      const left = before && !/\s$/.test(before) ? " " : "";
      const right = after && !/^\s/.test(after) ? " " : "";
      const inserted = `${left}${words}${right}`;
      this.input.value = before + inserted + after;
      this.caret = at + inserted.length;
      this.changed();
    }
    if (returnToInput) this.input.focus();
    if (document.activeElement === this.input) this.input.setSelectionRange(this.caret, this.caret);
    this.input.dispatchEvent(new Event("input", { bubbles: true }));
  }

  dispose() {
    this.dictation.dispose();
  }

  focus() {
    this.input.focus();
  }

  set(text) {
    this.dictation.cancel();
    this.input.value = text;
    this.changed();
    this.focus();
    this.input.setSelectionRange(text.length, text.length);
  }

  /// Put `text` under the message being written, on a line of its own, and leave the cursor
  /// at the end of the person's words: what they ask stays first (and names the chat).
  addLine(text, { focus = true } = {}) {
    const selection = [this.input.selectionStart, this.input.selectionEnd, this.input.selectionDirection];
    const v = this.input.value.replace(/\s+$/, "");
    this.input.value = `${v}\n${text}`;
    this.changed();
    if (focus) {
      this.focus();
      this.input.setSelectionRange(v.length, v.length);
    } else this.input.setSelectionRange(...selection);
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
    this.input.placeholder = on ? (touch() ? "Message the agent" : label("Message the agent · ⌘↩ queues it")) : this.placeholder || "Ask anything";
  }

  /// Name the model on its button: the app's words when it draws the picker. With nothing
  /// chosen yet the button asks for a choice, and sending opens the menu.
  setModel(desc) {
    this.modelDescription = desc;
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

  placeChanged(place) {
    this.dictation.routeChanged();
    this.backends = null;
    this.backendsFor = null;
    this.pendingModel = "";
    if (place) {
      this.setModel(`${place} default`);
    } else {
      this.setModel(this.api.summary()?.model);
    }
    this.updateSlash();
  }

  /// The chat's authority as its summary says it. Not drawn: the person changes it in words.
  setExecutionMode(mode, revision = 0) {
    this.executionMode = ["plan", "manual", "auto"].includes(mode) ? mode : "manual";
    this.executionModeRevision = revision;
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
      if (this.dictation.beforeSubmit()) return;
      this.submit(e.metaKey || e.ctrlKey);
    }
  }

  submit(queue) {
    const text = this.input.value.trim();
    if (!text) return;
    this.input.value = "";
    this.changed();
    const view = text.startsWith("/") && viewForCommand(text, this.api.project?.());
    if (view) return this.api.openView(view.id);
    if (!this.running) return this.api.submit(text);
    if (text.startsWith("/")) {
      this.enqueue(text);
      return;
    }
    if (queue) this.enqueue(text);
    else this.api.steer(text);
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
        h("span.queued-k", "Sending"),
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
    const remote = this.api.summary()?.place || this.api.pendingPlace?.();
    const items = this.commands.filter((c) => (!remote || REMOTE_COMMANDS.has(c.name))
      && (c.name.startsWith(q) || c.aliases.some((a) => a.startsWith(q))));
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

  commandAvailable(text) {
    if (!this.api.summary()?.place && !this.api.pendingPlace?.()) return true;
    const word = text.trim().slice(1).split(/\s+/, 1)[0].toLowerCase();
    const command = this.commands.find((c) => c.name === word || c.aliases.includes(word));
    return !!command && REMOTE_COMMANDS.has(command.name);
  }

  paintSlash() {
    [...this.slash.children].forEach((c, i) => c.setAttribute("aria-selected", String(i === this.slashIndex)));
  }

  // ---- model ---------------------------------------------------------------------------------

  /// Models grouped by backend: subscriptions, local Ollama, and APIs that have a key. A backend
  /// that is not ready shows the command that fixes it. The list is fetched on every open, so
  /// signing in elsewhere shows up the next time the menu opens.
  async modelMenu(anchor = this.model) {
    const open = document.querySelector(".popover.models-pop.open");
    if (open) return open.close();
    const picker = registry.modelPicker;
    if (picker) {
      const el = h("div.model-picker");
      const menu = this.api.popover(anchor, [el], { above: true });
      menu.classList.add("models-pop");
      try {
        // Closing returns to the message, so Enter sends what waited for a model.
        picker.render(el, {
          close: () => { menu.close(); this.focus(); },
          current: this.modelDescription,
          choose: this.api.chooseModel,
        });
      } catch (e) {
        console.error(e);
        el.append(h("div.menu-sub.pad", String(e)));
      }
      return;
    }
    const body = h("div.models");
    const input = h("input.menu-input", { placeholder: "Other model, e.g. ollama:qwen3:4b", "aria-label": "Model" });
    const menu = this.api.popover(anchor, [body, h("div.menu-sep"), input], { above: true });
    menu.classList.add("models-pop");
    let effortPage = null;
    let inlineError = null;
    const pick = async (spec) => {
      inlineError = null;
      try {
        if (this.api.pendingPlace?.() && !this.api.summary()?.place) {
          this.pendingModel = spec;
          this.setModel(spec.replace(":", " "));
          menu.close();
          this.focus();
          return;
        }
        await this.api.call("model", { spec });
        menu.close();
        this.focus();
      } catch (err) {
        inlineError = `Could not use ${spec}. ${err.message}`;
        paint(this.backends || []);
      }
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && input.value.trim()) pick(input.value.trim());
    });
    // A chat on another computer: its models, keys and programs are that computer's.
    const place = this.api.summary()?.place?.name || this.api.pendingPlace?.() || null;
    const loadBackends = () => place && !this.api.summary()?.place
      ? this.api.call("place_models", { place })
      : this.api.call("models");
    if (this.backendsFor !== place) this.backends = null;
    this.backendsFor = place;
    const paint = (backends) => {
      clear(body);
      if (effortPage) {
        const { backend: b, model: m } = effortPage;
        const head = h("div.model-head",
          h("button.icon-btn.small", { type: "button", "aria-label": `${b.label} models`, onclick: () => { effortPage = null; inlineError = null; paint(backends); } }, icon("arrow-left")),
          h("span.model-name", m.label || m.id));
        body.append(head);
        const choices = [{ id: null, label: "Default", hint: m.default_effort }, ...(m.efforts || []).map((e) => ({ id: e, label: e.charAt(0).toUpperCase() + e.slice(1) }))];
        for (const effort of choices) {
          const spec = `${b.provider}:${m.id}${effort.id ? `@${effort.id}` : ""}`;
          body.append(h("button.menu-item.model-item", { onclick: () => pick(spec) },
            h("span", effort.label), effort.hint ? h("span.menu-sub", effort.hint) : null));
        }
        if (inlineError) body.append(h("div.menu-sub.pad.model-error", { role: "alert" }, inlineError));
        return;
      }
      const current = this.model.textContent;
      for (const b of backends) {
        if (b.kind === "api" && !b.ready && !this.canSaveKeys && !place) continue;
        const group = h("div.model-group",
          h("div.model-head",
            h("span.model-dot", { "data-ok": String(b.ready) }),
            h("span.model-name", b.label),
            h("span.menu-sub", b.detail)));
        if (b.ready) {
          const rich = (b.model_options || []).filter((m) => !/embed/i.test(m.id));
          const options = rich.length ? rich : (b.models || []).filter((m) => !/embed/i.test(m)).map((id) => ({ id, label: id, efforts: [] }));
          for (const m of options) {
            const here = current === `${b.provider} ${m.id}` || current.startsWith(`${b.provider} ${m.id}@`);
            group.append(h("button.menu-item.model-item", { "aria-current": here ? "true" : null, onclick: () => {
              if (m.efforts?.length) {
                effortPage = { backend: b, model: m };
                inlineError = null;
                paint(backends);
              } else if (!here) pick(`${b.provider}:${m.id}`);
            } }, h("span", m.label || m.id), m.efforts?.length ? icon("chevron-right") : null));
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
              if (place) await this.api.call("place_key", { place, provider: b.provider, key: key.value.trim() });
              else await this.api.call("api_key_save", { provider: b.provider, key: key.value.trim() });
              this.backends = await loadBackends();
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
                await this.api.call("place_key", { place, provider: b.provider });
                this.backends = await loadBackends();
                paint(this.backends);
              } catch (err) {
                e.target.disabled = false;
                why.textContent = err.message;
              }
            } }, `Use ${this.api.hereName?.() || "this computer"}'s key`)));
          }
        } else if (place && b.step && this.api.summary()?.place) {
          // The program's own step, run there in a terminal shown here.
          group.append(h("div.model-fix", h("button.btn.small.primary", { type: "button", onclick: () => {
            menu.close();
            programSheet(this.api, b, place, () => { this.backends = null; });
          } }, b.step === "install" ? "Install" : "Sign in")));
        } else if (place && b.step) {
          group.append(h("div.model-fix", h("span.menu-sub", "Set up after this chat starts")));
        } else if (b.fix) {
          group.append(h("div.model-fix",
            h("code", b.fix),
            h("button.icon-btn.small", { title: "Copy", "aria-label": "Copy command", onclick: () => navigator.clipboard?.writeText(b.fix).then(() => this.api.toast("Copied. Run it in a terminal, then reopen this menu.")) }, icon("copy"))));
        }
        body.append(group);
      }
      if (inlineError) body.append(h("div.menu-sub.pad.model-error", { role: "alert" }, inlineError));
    };
    if (this.backends) paint(this.backends);
    else body.append(h("div.menu-sub.pad", "Checking models…"));
    try {
      let keys;
      [this.backends, keys] = await Promise.all([
        loadBackends(),
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
