// Settings › Computers (ADR 0052): the person's own computers a chat can run on. Adding one
// sets it up over the person's own `ssh` (the hosts of ~/.ssh/config, or user@host typed); each
// step shows as it runs, and a step that needs the person (ssh asking about a new host, sudo)
// opens a terminal on this computer for it. Removing one takes ARK off it.

import { h, icon, clear } from "./dom.js";
import { settingRow } from "./settings.js";
import { terminalSheet } from "./places.js";

const STEPS = [
  ["connect", "Connect"],
  ["look", "Check"],
  ["install", "Install"],
  ["start", "Start"],
  ["pair", "Link"],
  ["tunnel", "Connect the chat"],
];

const STATE = {
  ok: "Ready",
  starting: "Connecting",
  down: "Not answering",
  refused: "Signed this computer out",
  too_old: "Runs an older version",
};

function section(el, ark) {
  el.classList.add("plugins");
  const list = h("div.plugin-list");
  const add = h("div.plugin-list.computer-add");
  const why = h("p.plugin-why", { role: "status" });
  // The setup running, if any: `{target, step, failure}`.
  let setup = null;

  const drawList = async () => {
    const v = await ark.call("computers").catch(() => ({ computers: [] }));
    clear(list);
    if (!v.computers.length) list.append(h("p.settings-none", "None yet."));
    for (const c of v.computers) {
      const detail = [STATE[c.state] || "Not set up"];
      const controls = [];
      if (!c.linger) {
        detail.push(`Stops when you log out of ${c.name}`);
        controls.push(h("button.btn.small", { type: "button", onclick: () => answer(c.target, () => drawList()) }, "Keep running"));
      }
      const remove = h("button.btn.ghost.small.plugin-remove", { type: "button", "aria-label": `Remove ${c.name}` }, "Remove");
      remove.onclick = () => confirmRemove(c, row);
      controls.push(remove);
      const row = settingRow(c.name, detail.join(" · "), ...controls);
      list.append(row);
    }
  };

  // Removing asks once, in place: ARK's data there stays unless the person says otherwise.
  const confirmRemove = (c, row) => {
    const go = async (deleteStore, hereOnly = false) => {
      for (const b of shown.querySelectorAll("button")) b.disabled = true;
      try {
        await ark.call("computer_remove", { name: c.name, delete_store: deleteStore, here_only: hereOnly });
        ark.toast(`${c.name} removed`);
        drawList();
      } catch (e) {
        // It could not be done there (gone, rebuilt, refused): it can be forgotten here alone.
        const words = String(e.message || e);
        const failed = settingRow(`Could not remove ${c.name}`,
          /here only/.test(words) ? words : `${words}. Remove here only forgets it on this computer; ARK stays on ${c.name}.`,
          h("button.btn.small", { type: "button", onclick: () => go(false, true) }, "Remove here only"),
          h("button.btn.ghost.small", { type: "button", onclick: () => drawList() }, "Cancel"));
        shown.replaceWith(failed);
        shown = failed;
      }
    };
    let shown = settingRow(`Remove ${c.name}?`,
      c.address ? `Its chats leave this computer's list. This computer stays signed in there until you sign it out under Settings › Devices on ${c.name}.` : "Its chats leave this computer's list.",
      h("button.btn.small", { type: "button", onclick: () => go(false) }, "Remove"),
      h("button.btn.small", { type: "button", onclick: () => go(true) }, "Remove and delete its data there"),
      h("button.btn.ghost.small", { type: "button", onclick: () => drawList() }, "Cancel"));
    row.replaceWith(shown);
  };

  // A step that needs the person: its command in a terminal here, then the setup again.
  const answer = (target, then) =>
    terminalSheet(ark, "Answer the question", `for ${target}`, (cols, rows) => ark.call("computer_terminal", { target, cols, rows }), then);

  const start = async (target) => {
    target = target.trim();
    if (!target) return;
    why.textContent = "";
    setup = { target, step: "connect", failure: null };
    drawAdd();
    try {
      await ark.call("computer_add", { target });
    } catch (e) {
      setup = { target, step: "connect", failure: { words: e.message || String(e) } };
      drawAdd();
    }
  };

  const drawAdd = async () => {
    clear(add);
    if (setup) {
      const at = STEPS.findIndex(([k]) => k === setup.step);
      const steps = h("ol.computer-steps", ...STEPS.map(([k, label], i) =>
        h("li", { "data-state": setup.failure && i === at ? "failed" : i < at ? "done" : i === at ? "now" : "next" }, label)));
      add.append(h("div.computer-setup", h("div.computer-setup-head", icon("server"), h("span", setup.target)), steps));
      if (setup.failure) {
        const f = setup.failure;
        const again = h("button.btn.small.primary", { type: "button", onclick: () => start(setup.target) }, "Try again");
        const fix = f.terminal
          ? h("button.btn.small", { type: "button", onclick: () => answer(setup.target, () => start(setup.target)) }, "Answer in a terminal")
          : null;
        const cancel = h("button.btn.ghost.small", { type: "button", onclick: () => { setup = null; drawAdd(); } }, "Cancel");
        add.append(h("div.computer-failure", h("p", f.words), h("div.computer-actions", fix, again, cancel)));
      }
      return;
    }
    // `user@host` sets it up over ssh; an address (https://…) needs a code made there instead.
    const code = h("input.menu-input.computer-code", {
      type: "text", placeholder: "Code", "aria-label": "Code", autocomplete: "one-time-code", autocapitalize: "characters", spellcheck: "false", maxlength: "12", hidden: true,
      onkeydown: (e) => e.key === "Enter" && submit(),
    });
    const field = h("input.menu-input.computer-target", {
      type: "text", placeholder: "user@host", "aria-label": "Computer", autocomplete: "off", autocapitalize: "off", spellcheck: "false",
      oninput: () => (code.hidden = !/^https?:/i.test(field.value.trim())),
      onkeydown: (e) => e.key === "Enter" && submit(),
    });
    const submit = async () => {
      if (code.hidden) return start(field.value);
      go.disabled = true;
      why.textContent = "";
      try {
        const r = await ark.call("computer_add", { target: field.value.trim(), code: code.value });
        ark.toast(`${r.name} is ready`);
        drawList();
        drawAdd();
      } catch (e) {
        why.textContent = e.message || String(e);
        go.disabled = false;
      }
    };
    const go = h("button.btn.small.primary", { type: "button", onclick: () => submit() }, "Add");
    const hosts = await ark.call("computer_hosts").catch(() => []);
    const known = hosts.length
      ? h("div.computer-hosts", ...hosts.map((k) =>
        h("button.btn.small.computer-host", { type: "button", title: `${k.user ? `${k.user}@` : ""}${k.hostname}`, onclick: () => start(k.name) }, icon("server"), h("span", k.name))))
      : null;
    add.append(settingRow("Add a computer", null, field, code, go), known);
  };

  const off = ark.on("ark:computer_setup", (m) => {
    if (!setup || m.target !== setup.target) return;
    if (m.done) {
      if (!setup) return;
      setup = null;
      ark.toast(`${m.name} is ready`);
      drawList();
    } else if (m.failure) setup.failure = m.failure;
    else if (m.step) setup.step = m.step;
    drawAdd();
  });

  el.append(list, add, why);
  drawList();
  drawAdd();
  return { refresh: () => drawList(), dispose: () => off() };
}

/// Offer the section when the app lets the person add computers.
export async function registerComputers(ark) {
  const v = await ark.call("computers").catch(() => null);
  if (!v?.can_add) return;
  ark.setting({ id: "computers", title: "Computers", icon: "server", builtin: true, render: (el) => section(el, ark) });
}
