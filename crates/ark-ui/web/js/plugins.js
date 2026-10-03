// Settings › Tool servers: the MCP servers whose tools agents can use (`plugins`,
// `plugin_add`, `plugin_remove`, `plugin_restart`; the same as `/plugin` in a terminal). Shown
// when the app has plugins. Each agent still chooses which tools it may use; a change applies
// from the next turn.

import { h, icon, clear } from "./dom.js";

function servers(el, ark) {
  el.classList.add("plugins");
  const list = h("div.plugin-list");
  const input = h("textarea.plugin-input", {
    rows: 2,
    spellcheck: "false",
    "aria-label": "A server to add",
    placeholder: "notes python3 notes_server.py\nor paste an mcpServers JSON snippet",
  });
  const add = h("button.btn.primary", { type: "button" }, "Add");
  const why = h("p.plugin-why", { role: "status" });

  const draw = (all) => {
    clear(list);
    if (!all.length) {
      list.append(h("p.settings-none", "No servers yet."));
      return;
    }
    for (const p of all) {
      const restart = h("button.btn.ghost.small", { type: "button" }, "Restart");
      restart.onclick = async () => {
        restart.disabled = true;
        await act(() => ark.call("plugin_restart", { name: p.name }));
      };
      // Remove asks once, in place: the second click within a few seconds removes it.
      const remove = h("button.btn.ghost.small.plugin-remove", { type: "button" }, "Remove");
      remove.onclick = async () => {
        if (!remove.classList.contains("sure")) {
          remove.classList.add("sure");
          remove.textContent = "Remove?";
          setTimeout(() => {
            remove.classList.remove("sure");
            remove.textContent = "Remove";
          }, 3000);
          return;
        }
        remove.disabled = true;
        await act(() => ark.call("plugin_remove", { name: p.name }));
      };
      const detail = p.running
        ? `${p.tools.length} tool${p.tools.length === 1 ? "" : "s"}${p.tools.length ? ": " + p.tools.map((t) => t.slice(p.name.length + 1)).join(", ") : ""}`
        : p.error || "Stopped";
      list.append(h(`div.plugin-row${p.running ? "" : ".down"}`, { "data-name": p.name },
        h("span.plugin-dot", { title: p.running ? "Running" : "Stopped" }),
        h("div.plugin-main",
          h("div.plugin-name", p.name),
          h("div.plugin-detail", { title: detail }, detail),
          h("div.plugin-cmd", [p.command, ...p.args].join(" "))),
        h("div.plugin-actions", restart, remove)));
    }
  };

  const load = async () => draw((await ark.call("plugins").catch(() => [])) || []);
  async function act(fn) {
    why.textContent = "";
    try {
      await fn();
    } catch (e) {
      why.textContent = e.message || String(e);
    }
    await load();
  }

  add.onclick = async () => {
    const text = input.value.trim();
    if (!text) return input.focus();
    add.disabled = true;
    add.textContent = "Starting…";
    why.textContent = "";
    try {
      const done = await ark.call("plugin_add", { text });
      input.value = "";
      ark.toast(`Added ${done.map((p) => p.name).join(", ")}`);
    } catch (e) {
      why.textContent = e.message || String(e);
    }
    add.disabled = false;
    add.textContent = "Add";
    await load();
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) add.click();
  });

  el.append(
    h("p.settings-lead", "Agents can use the tools of these MCP servers. Each agent chooses which; a change applies from its next turn."),
    list,
    h("div.plugin-add",
      h("label.plugin-label", icon("plus"), "Add a server"),
      input,
      h("div.plugin-bar", h("span.plugin-hint", "A name, then the command that starts it. ⌘↩ adds."), add),
      why));
  load();
  return { refresh: load };
}

/// Offer the section when the app has plugins (`plugins` answers null when it has none).
export async function registerPlugins(ark) {
  const has = await ark.call("plugins").then((v) => v !== null, () => false);
  if (has) ark.setting({ id: "plugins", title: "Tool servers", icon: "box", builtin: true, render: (el) => servers(el, ark) });
}
