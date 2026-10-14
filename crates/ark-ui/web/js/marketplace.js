// Settings › Plugins. The catalog is intentionally separate from Tool servers: adding a
// marketplace entry records a person's choice, while connecting an account is a later, visible
// step. Nothing on this page gives an agent access by itself.

import { h, clear, icon } from "./dom.js";

function entryIcon(entry) {
  return h("span.market-icon", { "aria-hidden": "true", "data-id": entry.id }, entry.mark);
}

function marketplace(el, ark) {
  el.classList.add("marketplace");
  let installed = new Map();
  let connections = new Map();
  let offers = [];
  let managed = false;
  let selected = null;
  let query = "";
  const body = h("div.market-body");
  const notice = h("p.market-notice", { role: "status" });

  const load = async () => {
    const [records, catalog, linked] = await Promise.all([
      ark.call("marketplace_plugins").catch(() => []),
      ark.ext ? ark.ext("desktop.plugins_catalog").catch(() => null) : Promise.resolve(null),
      ark.ext ? ark.ext("desktop.plugins_connections").catch(() => null) : Promise.resolve(null),
    ]);
    installed = new Map((records || []).map((record) => [record.id, record]));
    offers = Array.isArray(catalog?.offers) ? catalog.offers.map((offer) => ({
      id: offer.id,
      name: offer.name,
      mark: offer.name?.slice(0, 1).toUpperCase() || "•",
      summary: offer.summary,
      category: offer.categories?.[0] || "Plugins",
      disclosure: offer.disclosure,
    })) : [];
    connections = new Map((linked?.connections || []).map((connection) => [connection.offer, connection]));
    managed = !!catalog;
    draw();
  };
  const entries = () => offers;
  const add = async (entry) => {
    notice.textContent = "";
    try {
      await ark.call("marketplace_add", { id: entry.id });
      ark.toast(`${entry.name} added`);
      await load();
    } catch (error) { notice.textContent = error.message || String(error); }
  };
  const remove = async (entry) => {
    notice.textContent = "";
    try { await ark.call("marketplace_remove", { id: entry.id }); await load(); }
    catch (error) { notice.textContent = error.message || String(error); }
  };
  const connect = async (entry) => {
    notice.textContent = "";
    try {
      await ark.ext("desktop.plugins_connect", { offer: entry.id });
      notice.textContent = "Your browser is open. Complete the connection there, then refresh this page.";
    } catch (error) { notice.textContent = error.message || String(error); }
  };
  const action = (entry, compact = false) => {
    if (installed.has(entry.id)) {
      const button = h("button.btn.ghost.small", { type: "button" }, compact ? "Added" : "Remove");
      if (compact) button.disabled = true;
      else button.onclick = () => remove(entry);
      return button;
    }
    const button = h("button.btn.primary.small.market-add", { type: "button", "aria-label": `Add ${entry.name}` }, compact ? icon("plus") : "Add");
    button.onclick = () => add(entry);
    return button;
  };
  const row = (entry) => h("div.market-row", { "data-id": entry.id },
    h("button.market-open", { type: "button", onclick: () => { selected = entry; draw(); }, "aria-label": `View ${entry.name}` },
      entryIcon(entry), h("span.market-copy", h("strong", entry.name), h("small", entry.summary))), action(entry, true));
  const drawCatalog = () => {
    const filtered = entries().filter((entry) => `${entry.name} ${entry.summary} ${entry.category}`.toLowerCase().includes(query.toLowerCase()));
    const catalog = h("div.market-catalog");
    for (const category of [...new Set(filtered.map((entry) => entry.category))]) {
      const entries = filtered.filter((entry) => entry.category === category);
      catalog.append(h("h2.market-heading", category), h("div.market-grid", entries.map(row)));
    }
    if (!filtered.length) catalog.append(h("p.settings-none", "No plugins match that search."));
    const search = h("input.market-search", { type: "search", placeholder: "Search plugins", "aria-label": "Search plugins", value: query });
    search.addEventListener("input", () => { query = search.value; draw(); });
    const installedEntries = [...installed.keys()].map((id) => entries().find((entry) => entry.id === id)).filter(Boolean);
    clear(body).append(
      h("div.market-top", h("div", h("p.settings-lead", "Connect services so ARK can work across your tools.")), search),
      installedEntries.length ? h("section.market-installed", h("h2.market-heading", "Installed"), h("div.market-grid", installedEntries.map(row))) : null,
      catalog,
      notice,
    );
  };
  const drawDetail = (entry) => {
    const added = installed.has(entry.id);
    const connection = connections.get(entry.id);
    const back = h("button.btn.ghost.small", { type: "button", onclick: () => { selected = null; draw(); } }, "Back");
    const primary = !added ? action(entry)
      : connection?.state === "connected" ? h("button.btn.primary", { type: "button", disabled: true }, "Connected")
        : managed ? h("button.btn.primary", { type: "button", onclick: () => connect(entry) }, "Connect")
          : h("button.btn.primary", { type: "button", disabled: true }, "Connect");
    const data = entry.disclosure?.payloadProcessors?.map((processor) => processor.name).join(", ") || "The connection provider processes requested actions";
    clear(body).append(
      h("div.market-detail-head", back),
      h("section.market-detail", entryIcon(entry), h("div", h("h2", entry.name), h("p", entry.summary))),
      h("div.market-detail-actions", primary, added
        ? h("p.market-state", connection?.state === "connected" ? "Connected for your ARK Account." : managed ? "Connect an account in your browser. Adding never gives an agent access." : "Connect requires a signed-in ARK Account and a configured managed provider.")
        : h("p.market-state", "Adding does not connect an account or give an agent access.")),
      h("dl.market-facts", h("div", h("dt", "Connection"), h("dd", "Requires an ARK Account")), h("div", h("dt", "Data"), h("dd", data)), h("div", h("dt", "Access"), h("dd", "Each action still asks for approval when needed"))),
      added ? h("button.btn.ghost.small.market-remove-detail", { type: "button", onclick: () => remove(entry) }, "Remove") : null,
      notice,
    );
  };
  const draw = () => selected ? drawDetail(selected) : drawCatalog();
  el.append(body);
  load();
  return { refresh: load };
}

export async function registerMarketplace(ark) {
  const has = await ark.call("marketplace_plugins").then((value) => value !== null, () => false);
  if (has) ark.setting({ id: "marketplace", title: "Plugins", icon: "plug", builtin: true, render: (el) => marketplace(el, ark) });
}
