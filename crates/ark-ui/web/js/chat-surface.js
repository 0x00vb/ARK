import { h, icon } from "./dom.js";

export function createChatSurface({ kind = "secondary", session, capabilities = { files: true, model: true, mode: true } } = {}) {
  const primary = kind === "primary";
  const id = (name) => primary ? { id: name } : {};
  const scroller = h("section.scroller", id("scroller"));
  const threadEl = h("div.thread", id("thread"));
  scroller.append(threadEl);
  const asks = h("div.asks", { ...id("asks"), "aria-live": "polite" });
  const uploads = h("div.uploads");
  const queue = h("div.queue", id("queue"));
  const slash = h("div.palette", { ...id("slash"), hidden: true });
  const input = h("textarea", { ...id("input"), rows: "1", placeholder: "Ask anything", spellcheck: "true" });
  const attach = capabilities.files ? h("button.icon-btn.attach", { ...id("attach"), type: "button", title: "Add files", "aria-label": "Add files" }, icon("plus")) : null;
  const mic = h("button.icon-btn.dictate", { ...id("dictate"), type: "button", "aria-label": "Start dictation" });
  const dictationStatus = h("div.dictation-status", { ...id("dictation-status"), role: "status", "aria-live": "polite", hidden: true });
  const model = capabilities.model ? h("button.chip.model-chip", { ...id("model-chip"), type: "button", title: "Model", "aria-haspopup": "menu" }) : null;
  const mode = capabilities.mode ? h("button.chip.mode-chip", { ...id("mode-chip"), type: "button", title: "Execution mode (⇧Tab)", "aria-haspopup": "menu" }, "Manual") : null;
  const controls = h("span.chat-view-controls");
  const actions = h("span.chat-view-actions");
  const place = primary ? h("button.chip.place-chip", { id: "place-chip", type: "button", hidden: true }) : null;
  const context = primary ? h("button.ctx-meter", { id: "ctx-ring", type: "button", title: "Context" }) : null;
  const send = h("button.send", { ...id("send"), type: "submit", "aria-label": "Send" });
  const form = h("form.composer", { ...id("composer"), autocomplete: "off" }, input, dictationStatus,
    h("div.composer-bar", attach, mic, model, mode, place, context, controls, actions, send));
  const placeLine = primary ? h("div.place-line", { id: "place-line", role: "status", hidden: true }) : null;
  const plans = primary ? h("div.plans", { id: "plans" }) : null;
  const dock = h("div.dock", placeLine, plans, uploads, queue, slash, form);
  const surface = h(`section.chat-surface.${kind}`, { "data-session": session, "data-active": String(primary) }, scroller, asks, dock);
  return { surface, scroller, threadEl, asks, uploads, queue, slash, input, attach, mic, dictationStatus, model, mode, send, form, actions, controls };
}
