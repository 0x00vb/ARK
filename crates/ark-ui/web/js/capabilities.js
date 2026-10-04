// Settings › Capabilities: what ARK can use (`capabilities`, `capability_connect`,
// `capability_disconnect`; the same list as `/connect` in a terminal). Connect opens the
// provider's sign-in in the browser; the row turns ready once the person allows it there.

import { h, clear } from "./dom.js";

function section(el, ark) {
  el.classList.add("plugins");
  const list = h("div.plugin-list");
  const why = h("p.plugin-why", { role: "status" });
  let waiting = null;

  const draw = (all) => {
    clear(list);
    if (!all.length) {
      list.append(h("p.settings-none", "This app gives its agents no capabilities yet."));
      return;
    }
    for (const c of all) {
      const actions = h("div.plugin-actions");
      const says = c.says.charAt(0).toUpperCase() + c.says.slice(1);
      let detail = says;
      if (c.state === "ready" && c.account) detail = `${says} · ${c.account}`;
      if (c.added) detail = `${says} · managed in Tool servers`;
      // Why it cannot be used is for the curious: on hover.
      if (c.state === "unavailable") actions.append(h("span.plugin-hint", { title: c.why }, "Not available yet"));
      if (c.state === "connect") {
        const go = h("button.btn.primary.small", { type: "button" }, waiting === c.id ? "Waiting for sign-in…" : `Connect ${c.provider || ""}`.trim());
        go.disabled = waiting === c.id;
        go.onclick = () => connect(c);
        actions.append(go);
      } else if (c.state === "ready" && c.account) {
        // Disconnect asks once, in place: the second click within a few seconds disconnects.
        const off = h("button.btn.ghost.small.plugin-remove", { type: "button" }, "Disconnect");
        off.onclick = async () => {
          if (!off.classList.contains("sure")) {
            off.classList.add("sure");
            off.textContent = `Disconnect ${c.provider}?`;
            setTimeout(() => { off.classList.remove("sure"); off.textContent = "Disconnect"; }, 3000);
            return;
          }
          off.disabled = true;
          await act(() => ark.call("capability_disconnect", { id: c.id }));
        };
        actions.append(off);
      }
      const state = c.state === "ready" ? "" : ".off";
      list.append(h(`div.plugin-row.capability-row${state}`, { "data-id": c.id },
        h("span.plugin-dot", { title: c.state === "ready" ? "Ready" : c.state === "connect" ? "Not connected" : "Unavailable" }),
        h("div.plugin-main",
          h("div.plugin-name", c.label),
          h("div.plugin-detail", { title: detail }, detail)),
        actions));
    }
  };

  const load = async () => {
    const all = (await ark.call("capabilities").catch(() => [])) || [];
    draw(all);
    return all;
  };

  async function act(fn) {
    why.textContent = "";
    try {
      await fn();
    } catch (e) {
      why.textContent = e.message || String(e);
    }
    await load();
  }

  // Open the sign-in, then look again every two seconds until the row is ready (five minutes).
  async function connect(c) {
    why.textContent = "";
    let url;
    try {
      ({ url } = await ark.call("capability_connect", { id: c.id }));
    } catch (e) {
      why.textContent = e.message || String(e);
      return;
    }
    window.open(url, "_blank", "noopener");
    waiting = c.id;
    await load();
    const until = Date.now() + 300000;
    while (waiting === c.id && Date.now() < until) {
      await new Promise((r) => setTimeout(r, 2000));
      const all = await load();
      if (all.find((x) => x.id === c.id)?.state === "ready") {
        ark.toast(`${c.label} is ready`);
        break;
      }
    }
    waiting = null;
    await load();
  }

  el.append(list, why);
  load();
  return { refresh: load, dispose: () => { waiting = null; } };
}

/// Offer the section when the host lists capabilities.
export async function registerCapabilities(ark) {
  const has = await ark.call("capabilities").then(Array.isArray, () => false);
  if (has) ark.setting({ id: "capabilities", title: "Capabilities", icon: "plug", builtin: true, render: (el) => section(el, ark) });
}
