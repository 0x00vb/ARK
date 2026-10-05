// The conversation: user messages, what the agent is doing, and its answers.
//
// Activity comes from ark-interact already described (`activity.label` etc.); this file only
// decides layout. Consecutive activity forms a work block that stays open while the agent
// works and folds into one line when the turn ends. A tool row opens on click to show the
// call: arguments, result, duration, approval, and what it added to the model's context.

import { h, icon, clear, clip, secs, tokens, esc } from "./dom.js";
import { markdown } from "./md.js";
import { renderReport } from "./report.js";
import { registry } from "./ext.js";
import { kindOf, kindNoun, KIND_ICON } from "./file.js";


const TOOLISH = new Set(["tool", "retrieval", "reuse", "capability", "present", "plan", "state", "note", "steered", "warning"]);

export class Thread {
  constructor(root, scroller, api) {
    this.root = root;
    this.scroller = scroller;
    this.api = api;
    this.live = null;
    this.stick = true;
    scroller.addEventListener("scroll", () => {
      const s = scroller;
      this.stick = s.scrollHeight - s.scrollTop - s.clientHeight < 80;
    });
    root.addEventListener("click", (e) => {
      const a = e.target.closest("a.cite");
      if (a) {
        e.preventDefault();
        api.openInvocation(a.dataset.inv);
      }
    });
  }

  clear() {
    clear(this.root);
    this.live = null;
    this.firstIndex = null;
  }

  scroll(force) {
    if (force || this.stick) {
      requestAnimationFrame(() => (this.scroller.scrollTop = this.scroller.scrollHeight));
    }
  }

  isEmpty() {
    return !this.root.querySelector(".turn, .sysmsg");
  }

  welcome(info, suggestions, onPick) {
    this.clear();
    const w = h("div.welcome",
      info.logo ? h("img.welcome-logo", { src: info.logo, alt: "" }) : null,
      h("h1", info.title || "What should we work on?"),
      info.subtitle ? h("p", info.subtitle) : null,
      suggestions.length
        ? h("div.suggestions", suggestions.map(([label, prompt], i) =>
            h("button.suggestion", { style: { "--i": i }, onclick: () => onPick(prompt) }, label)))
        : null,
    );
    this.root.append(w);
  }

  dropWelcome() {
    this.root.querySelector(".welcome")?.remove();
  }

  // ---- stored turns --------------------------------------------------------------------------

  /// Render stored turns. `prepend` puts them above what is shown (loading earlier pages).
  renderStored(turns, { prepend = false, hasMore = false } = {}) {
    this.dropWelcome();
    const frag = document.createDocumentFragment();
    for (const t of turns) frag.append(this.storedTurn(t));
    this.root.querySelector(".load-earlier")?.remove();
    if (prepend) {
      const before = this.scroller.scrollHeight;
      this.root.prepend(frag);
      this.scroller.scrollTop += this.scroller.scrollHeight - before;
    } else {
      this.root.append(frag);
    }
    if (turns.length) this.firstIndex = Math.min(this.firstIndex ?? Infinity, turns[0].index);
    if (hasMore && this.firstIndex > 0) {
      this.root.prepend(h("button.load-earlier", { onclick: () => this.api.loadEarlier(this.firstIndex) }, "Show earlier turns"));
    }
  }

  storedTurn(t) {
    const turn = new TurnView(this, t.input, t.index);
    turn.el.classList.add("stored");
    // Activity replays in order: narration where it was written, steering where it was read.
    // Turns stored before traces were kept only have the steered list.
    const acts = t.activity || [];
    if (!acts.some((a) => a.kind === "steered")) for (const s of t.steered || []) turn.userLine(s, true);
    for (const a of acts) {
      if (a.kind === "narration") turn.text(a.label, false);
      else if (a.kind === "steered") turn.userLine(a.detail || a.label, true);
      else turn.activity(a, null);
    }
    turn.closeBlocks();
    // A replayed "stopped" activity already said so, as it did live.
    if (t.cancelled && !acts.some((a) => a.kind === "stopped")) turn.notice("Stopped.", "muted");
    else if (t.unfinished) turn.notice(`Did not finish: ${t.unfinished}`, "error");
    else if (t.answer) turn.text(t.answer, true);
    turn.fileCards();
    turn.footer(t.footer, t.index);
    for (const p of t.posts || []) turn.el.append(postView(p));
    return turn.el;
  }

  /// Something work this chat set up posted here (a scheduled run's result), as it arrives.
  post(p) {
    this.dropWelcome();
    this.root.append(postView(p));
    this.scroll(false);
  }

  // ---- the live turn -------------------------------------------------------------------------

  begin(input) {
    this.dropWelcome();
    this.live = new TurnView(this, input, null);
    this.live.live = true;
    this.live.working("Thinking");
    this.root.append(this.live.el);
    this.scroll(true);
    return this.live;
  }

  event(msg) {
    const t = this.live;
    if (!t) return;
    if (msg.kind === "text") {
      t.stream(msg.text);
    } else if (msg.kind === "event") {
      const ev = msg.event;
      if (ev.type === "answer") {
        if (!ev.streamed) t.text(ev.text, true);
        else t.markAnswer();
      } else if (ev.type === "steered") {
        t.userLine(ev.text, true);
      } else if (msg.activity) {
        t.activity(msg.activity, ev);
      }
    }
    this.scroll();
  }

  approval(msg, onDecide) {
    (this.live || this.lastTurn())?.approval(msg, onDecide);
    this.scroll(true);
  }

  approvalClosed(id) {
    const card = document.querySelector(`.approval[data-id="${id}"]`);
    if (!card) return;
    card.classList.add("leaving");
    setTimeout(() => card.remove(), 180);
  }

  lastTurn() {
    return this.live;
  }

  finish(msg) {
    const t = this.live;
    if (!t) return;
    t.index = msg.index ?? null;
    t.done(msg);
    this.live = null;
    this.scroll();
  }

  /// A command and its structured result, shown in the conversation.
  report(line, reply) {
    this.dropWelcome();
    const r = reply.report || {};
    if (!r.title && !(r.blocks || []).length) return;
    const el = h("div.sysmsg", h("div.sysmsg-cmd", line), renderReport(r, { onCommand: (c) => this.api.command(c) }));
    this.root.append(el);
    this.scroll(true);
  }

}

/// A post: who sent it, what about, and what it said.
function postView(p) {
  return h(`div.post${p.ok ? "" : ".failed"}`,
    h("div.post-head", h("span.post-from", p.from), h("span.post-title", p.title),
      p.ok ? null : h("span.post-failed", "did not finish")),
    h("div.prose", { html: markdown(p.text || "") }));
}

class TurnView {
  constructor(thread, input, index) {
    this.thread = thread;
    this.api = thread.api;
    this.index = index;
    this.calls = 0;
    this.block = null;
    this.textEl = null;
    this.textBuf = "";
    this.rows = new Map();
    /// Files of the project the turn wrote, in the order it first wrote them.
    this.files = new Set();
    this.started = performance.now();
    this.el = h("article.turn",
      h("div.user", h("div.bubble", input)),
      (this.agent = h("div.agent")),
    );
  }

  userLine(text, steered, note) {
    const b = h("div.user.steered", h("div.bubble", text), note ? h("div.user-note", note) : null);
    this.agent.append(b);
    this.block = null;
    this.textEl = null;
  }

  working(label) {
    if (!this.live) return;
    if (!this.workEl) {
      this.workEl = h("div.working-line", h("span.shimmer"), h("span.elapsed"));
      this.agent.append(this.workEl);
      this.timer = setInterval(() => {
        const s = Math.floor((performance.now() - this.started) / 1000);
        this.workEl.querySelector(".elapsed").textContent = s >= 1 ? `${s}s · esc to stop` : "";
      }, 500);
    }
    this.workEl.querySelector(".shimmer").textContent = label;
  }

  keepWorkingLast() {
    if (this.workEl) this.agent.append(this.workEl);
  }

  // ---- work blocks -------------------------------------------------------------------------

  currentBlock() {
    if (this.block) return this.block;
    const rows = h("div.work-rows");
    const head = h("button.work-head", { type: "button" }, icon("chevron-right", "chev"), h("span.work-summary"));
    const b = h("div.work", { "data-open": "true" }, head, h("div.disclose", h("div.disclose-inner", rows)));
    head.addEventListener("click", () => (b.dataset.open = b.dataset.open === "true" ? "false" : "true"));
    this.block = { el: b, rows, tools: 0, labels: [] };
    this.agent.append(b);
    this.textEl = null;
    this.keepWorkingLast();
    return this.block;
  }

  closeBlocks() {
    for (const b of this.agent.querySelectorAll(".work")) {
      const n = Number(b.dataset.tools || 0);
      const first = b.dataset.firstTool || b.dataset.first || "";
      const s = b.querySelector(".work-summary");
      // What it did first, in the app's words, and how many steps followed.
      const rest = n > 1 ? ` and ${n - 1} more step${n === 2 ? "" : "s"}` : "";
      s.textContent = first ? clip(first, 60) + rest : n > 0 ? `${n} step${n === 1 ? "" : "s"}` : "";
      if (s.textContent) b.dataset.summary = "";
      else delete b.dataset.summary;
      b.dataset.open = "false";
    }
    this.block = null;
  }

  activity(a, ev) {
    if (!a) return;
    const kind = a.kind;
    if (kind === "result") return this.result(a, ev);
    if (kind === "approval") return this.attach(a, ev, (row) => {
      row.dataset.status = a.label === "denied" ? "denied" : row.dataset.status;
    });
    if (kind === "reuse") return this.attach(a, ev, (row) => (row.dataset.status = "reused"), "reused an earlier result");
    if (kind === "warning" && /needs approval/.test(a.label)) return this.attach(a, ev, (row) => (row.dataset.status = "refused"), "needs approval; not run");
    if (kind === "stopped") {
      this.closeBlocks();
      return this.notice("Stopped.", "muted");
    }
    const blk = this.currentBlock();
    if (!TOOLISH.has(kind)) return;
    // The same activity again: count it on the last row.
    const last = blk.rows.lastElementChild;
    if (kind !== "tool" && last && last.dataset.label === a.label && last.dataset.kind === kind) {
      const n = Number(last.dataset.count || 1) + 1;
      last.dataset.count = n;
      last.querySelector(".act-count").textContent = `×${n}`;
      return;
    }
    const row = h("div.act", { "data-kind": kind, "data-label": a.label, "data-status": kind === "tool" ? "running" : "done" },
      h("div.act-line", h("span.act-glyph"), h("span.act-label", a.label), h("span.act-count"), h("span.act-meta")),
      a.detail && kind !== "retrieval" ? h("div.act-detail", clip(a.detail, 240)) : null,
      a.items?.length ? h("ol.act-items", a.items.map((i) => h("li", i))) : null,
    );
    if (a.tone && a.tone !== "normal") row.classList.add(`tone-${a.tone}`);
    if (!blk.labels.length) blk.el.dataset.first = a.label;
    blk.labels.push(a.label);
    if (kind === "tool") {
      const n = ++this.calls;
      row.dataset.n = n;
      if (!blk.el.dataset.firstTool) blk.el.dataset.firstTool = a.label;
      row.dataset.action = a.action || "";
      blk.el.dataset.tools = Number(blk.el.dataset.tools || 0) + 1;
      this.rows.set(a.call, row);
      row.classList.add("inspectable");
      row.querySelector(".act-line").addEventListener("click", () => this.toggleCall(row));
      this.working("Working");
    }
    blk.rows.append(row);
  }

  /// Put an outcome (approval, reuse, refusal) on the most recent running row for that action.
  attach(a, ev, mark, text) {
    const rows = [...this.agent.querySelectorAll(`.act[data-kind="tool"]`)].reverse();
    const row = rows.find((r) => r.dataset.action === a.action || (ev && ev.action === r.dataset.action)) || rows[0];
    if (!row) return;
    mark(row);
    this.detail(row, text || a.label, a.tone);
  }

  detail(row, text, tone) {
    let d = row.querySelector(".act-detail");
    if (!d) {
      d = h("div.act-detail");
      row.querySelector(".act-line").after(d);
    }
    d.textContent = clip(text, 240);
    d.className = "act-detail" + (tone && tone !== "normal" && tone !== "muted" ? ` tone-${tone}` : "");
  }

  result(a, ev) {
    for (const f of a.files || []) this.files.add(f);
    const row = this.rows.get(a.call);
    if (!row) return;
    const ok = ev ? ev.ok : a.tone !== "error";
    if (row.dataset.status === "running") row.dataset.status = ok ? "ok" : "failed";
    if (a.label) this.detail(row, a.label, ok ? "muted" : "error");
    if (ev?.duration_ms != null) row.querySelector(".act-meta").textContent = secs(ev.duration_ms);
  }

  async toggleCall(row) {
    let more = row.querySelector(".act-more");
    if (more) {
      row.dataset.expanded = row.dataset.expanded === "true" ? "false" : "true";
      return;
    }
    more = h("div.disclose.act-more", h("div.disclose-inner", h("div.call", h("div.call-loading", "Loading…"))));
    row.append(more);
    requestAnimationFrame(() => (row.dataset.expanded = "true"));
    const box = more.querySelector(".call");
    try {
      const r = await this.api.call("call", { index: this.index, n: Number(row.dataset.n) });
      clear(box).append(callDetail(r, this.api, this.index));
    } catch (e) {
      clear(box).append(h("div.call-err", e.message));
    }
  }

  // ---- text ----------------------------------------------------------------------------------

  stream(text) {
    if (!this.textEl) {
      this.block = null;
      this.textEl = h("div.prose.streaming");
      this.agent.append(this.textEl);
      this.textBuf = "";
      this.keepWorkingLast();
    }
    this.textBuf += text;
    this.working("Writing");
    // Draw on the next frame, into the element this text belongs to: a tool row may close
    // the element before the frame, and its last chunk must still land.
    const el = (this.lastStream = this.textEl);
    el._md = this.textBuf;
    if (!el.pending) {
      el.pending = true;
      requestAnimationFrame(() => {
        el.pending = false;
        el.innerHTML = markdown(el._md);
      });
    }
  }

  /// The streamed text was the answer. A row (a failed self-check, say) may have closed the
  /// element since, so mark the last streamed one rather than only an open one.
  markAnswer() {
    const el = this.textEl || this.lastStream;
    if (el) {
      el.innerHTML = markdown(el._md ?? this.textBuf);
      el.classList.remove("streaming");
      el.classList.add("answer");
    }
  }

  text(md, answer) {
    this.block = null;
    const el = h("div.prose" + (answer ? ".answer" : ""), { html: markdown(md) });
    this.agent.append(el);
    this.textEl = null;
    this.keepWorkingLast();
  }

  notice(text, tone) {
    this.agent.append(h(`div.notice.tone-${tone || "muted"}`, text));
    this.keepWorkingLast();
  }

  /// The files the turn wrote, each a card that opens it beside the conversation.
  fileCards() {
    this.agent.querySelector(".turn-files")?.remove();
    if (!this.files.size) return;
    this.agent.append(h("div.turn-files", [...this.files].map((path) => {
      const kind = kindOf(path);
      return h("button.file-card", { type: "button", "data-kind": kind, title: path, onclick: () => this.api.openFile(path) },
        h("span.file-card-ico", icon(KIND_ICON[kind])),
        h("span.file-card-text", h("span.file-card-name", path.split("/").pop()), h("span.file-card-sub", kindNoun(kind))));
    })));
  }

  footer(parts, index) {
    if (!parts || !parts.length) return;
    this.agent.append(h("div.turn-foot", parts.join(" · ")));
  }

  approval(msg, onDecide) {
    const card = approvalCard(msg, onDecide);
    this.block = null;
    this.textEl = null;
    this.agent.append(card);
    this.keepWorkingLast();
    requestAnimationFrame(() => card.querySelector(".btn.primary")?.focus({ preventScroll: true }));
  }

  done(msg) {
    this.live = false;
    clearInterval(this.timer);
    this.workEl?.remove();
    this.workEl = null;
    // Text still streaming is the answer only if the turn finished; a stopped turn has none.
    if (this.textEl && !msg.cancelled && !msg.error) this.markAnswer();
    else this.textEl?.classList.remove("streaming");
    this.closeBlocks();
    if (msg.error) this.notice(msg.error, "error");
    else if (msg.cancelled && !this.agent.querySelector(".notice")) this.notice("Stopped.", "muted");
    if (msg.ok && !msg.cancelled && msg.answer && !this.agent.querySelector(".prose.answer")) this.text(msg.answer, true);
    this.fileCards();
    this.footer(msg.footer, msg.index);
    for (const card of this.agent.querySelectorAll(".approval:not(.decided)")) card.remove();
  }
}

/// The question card for a pending approval. An orchestrated agent's question names the
/// agent and its task; the conversation's own has the keyboard hints.
export function approvalCard(msg, onDecide) {
  const req = msg.request;
  const own = !req.agent;
  const decide = (d) => {
    card.classList.add("decided");
    for (const b of card.querySelectorAll("button")) b.disabled = true;
    onDecide(msg.id, d);
  };
  // The line's first row says what; the rest, if any, is what it would write (a preview).
  const [what, ...more] = String(msg.line || "").split("\n");
  const indent = Math.min(...more.filter((l) => l.trim()).map((l) => l.match(/^ */)[0].length));
  const preview = more.map((l) => l.slice(Number.isFinite(indent) ? indent : 0)).join("\n").trim();
  const card = h("div.approval", { "data-id": msg.id, "data-action": req.action || null, tabindex: "-1" },
    // The headline comes from ark-interact, as the terminal's does.
    h("div.approval-title", msg.title || (own ? "ARK wants to:" : `${req.agent} wants to:`)),
    h("div.approval-line", what),
    preview ? h("pre.approval-preview", preview) : null,
    // What the action does in general, unless the preview already shows what this call does.
    req.summary && !preview ? h("div.approval-summary", req.summary) : null,
    // The answers come from ark-interact, as the terminal's do: the first is the default.
    h("div.approval-actions", ...(msg.choices || [
      { label: "Allow once", decision: "allow_once" },
      { label: "Allow for session", decision: "allow_session" },
      { label: "Deny", decision: "deny" },
    ]).map((c, i) => h(i === 0 ? "button.btn.primary" : c.decision === "deny" ? "button.btn.ghost" : "button.btn",
      { onclick: () => decide(c.decision), "data-decision": c.decision },
      c.label,
      own && i === 0 ? h("kbd", "↩") : null,
      own && c.decision === "deny" ? h("kbd", "esc") : null))),
    req.class && req.class !== req.action && req.action !== "acquire_capability" ? h("div.approval-class", `“Allow for session” covers ${req.class}`) : null,
  );
  card.decide = decide;
  return card;
}

/// Everything known about one tool call.
export function callDetail(r, api, index) {
  const c = r.call;
  const inv = r.invocation;
  const status = {
    running: "running", ok: "ok", failed: "failed", denied: "denied by you",
    refused: "not run: needed approval", reused: "reused an earlier result", not_run: "not run",
  }[c.status] || c.status;
  const fields = h("dl.kv",
    h("dt", "Action"), h("dd.mono", c.action),
    h("dt", "Status"), h(`dd.status-${c.status}`, status),
    c.duration_ms != null ? [h("dt", "Duration"), h("dd", secs(c.duration_ms))] : null,
    c.approval ? [h("dt", "Approval"), h("dd", `${c.approval.decision.replace(/_/g, " ")}${c.approval.remembered ? " (remembered)" : ""} · ${c.approval.class}`)] : null,
    c.context_tokens != null
      ? [h("dt", "Context"), h("dd", `+${tokens(c.context_tokens)} tokens (distilled) · carried by ${c.seen_by_requests} later request${c.seen_by_requests === 1 ? "" : "s"}${c.left_window_at ? ` · left the window at step ${c.left_window_at}` : ""}`)]
      : null,
    inv ? [h("dt", "Evidence"), h("dd.mono", `inv:${inv.id.slice(0, 8)}`)] : null,
  );
  const out = h("div.call-body", fields);
  out.append(h("div.call-label", "Arguments"), h("pre.code-block", JSON.stringify(c.input, null, 2)));
  if (c.summary) out.append(h("div.call-label", "Result the model saw"), h("div.call-summary", c.summary));
  if (c.error) out.append(h("div.call-err", c.error));
  if (inv) {
    const custom = registry.toolRenderers.find((t) => {
      try { return t.match(c.action, c); } catch { return false; }
    });
    if (custom) {
      const host = h("div.call-custom");
      out.append(host);
      try { custom.render(host, { call: c, invocation: inv }, window.ark); } catch (e) { host.textContent = String(e); }
    } else {
      const body = JSON.stringify(inv.output, null, 2) || "";
      const lines = body.split("\n");
      const pre = h("pre.code-block.output", lines.slice(0, 80).join("\n"));
      out.append(h("div.call-label", "Full output (not sent to the model)"), pre);
      if (lines.length > 80) {
        out.append(h("button.link", { onclick: (e) => { pre.textContent = body; e.target.remove(); } }, `Show all ${lines.length} lines`));
      }
    }
  }
  return out;
}

export { esc };
