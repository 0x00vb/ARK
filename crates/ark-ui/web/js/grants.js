// Settings › Permissions: what agents may do in this project without asking, because the
// person answered "Always in this project" (`grants`, `grant_remove`; the same as `/grants` in
// a terminal). Removing one makes agents ask again from their next call.

import { h, clear } from "./dom.js";

function section(el, ark) {
  el.classList.add("plugins");
  const list = h("div.plugin-list");
  const why = h("p.plugin-why", { role: "status" });

  const draw = (all) => {
    clear(list);
    if (!all.length) {
      list.append(h("p.settings-none", "Agents ask before they change anything."));
      return;
    }
    for (const g of all) {
      const remove = h("button.btn.ghost.small.plugin-remove", { type: "button" }, "Remove");
      remove.onclick = async () => {
        remove.disabled = true;
        why.textContent = "";
        try {
          await ark.call("grant_remove", { class: g.class });
        } catch (e) {
          why.textContent = e.message || String(e);
        }
        await load();
      };
      list.append(h("div.plugin-row.grant-row", { "data-class": g.class },
        h("div.plugin-main",
          h("div.plugin-name", g.class),
          h("div.plugin-detail", { title: g.preview }, `First allowed for ${g.preview}${g.granted_at ? " on " + g.granted_at.slice(0, 10) : ""}`)),
        h("div.plugin-actions", remove)));
    }
  };

  const load = async () => draw((await ark.call("grants").catch(() => [])) || []);
  el.append(
    h("p.settings-lead", "What agents may do in this project without asking."),
    list,
    why);
  load();
  return { refresh: load };
}

/// Offer the section when the host keeps grants.
export async function registerGrants(ark) {
  const has = await ark.call("grants").then(Array.isArray, () => false);
  if (has) ark.setting({ id: "permissions", title: "Permissions", icon: "shield", builtin: true, render: (el) => section(el, ark) });
}
