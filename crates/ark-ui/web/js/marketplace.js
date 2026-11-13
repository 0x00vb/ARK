// Plugins are a global destination. Reviewed provider offers and the runtime's connectable
// capabilities are available by default. Connecting an account, or turning on a capability the
// app offers with a switch (the Computer, ADR 0045), is the only enabling action and never grants
// an agent silent access.

import { h, clear } from "./dom.js";
import { registry } from "./ext.js";
import { pluginArtwork } from "./plugin-art.js";
import { skillsMarketplace } from "./marketplace-skills.js";
import { openOutside } from "./links.js";

function entryIcon(entry) {
  return pluginArtwork(entry);
}

function marketplace(el, ark) {
  el.classList.add("marketplace");
  let offers = [];
  let alive = true;
  let generation = 0;
  let detailControl = null;
  let mountedAppId = null;
  let selected = null;
  let query = "";
  let loading = true;
  let account = null;
  let catalogError = null;
  let connectionsError = null;
  let capabilityError = null;
  const connecting = new Set();
  const body = h("div.market-body");
  const notice = h("div.market-notice", { role: "status" });
  const skillBody = h("div", { hidden: true, id: "market-skills", role: "tabpanel", "aria-labelledby": "market-skills-tab" });
  body.id = "market-plugins";
  body.setAttribute("role", "tabpanel"); body.setAttribute("aria-labelledby", "market-plugins-tab");
  let skillControl = null;
  const tabs = ["Plugins", "Skills"].map((title, index) => h("button", {
    type: "button", role: "tab", id: index === 0 ? "market-plugins-tab" : "market-skills-tab", "aria-selected": String(index === 0), tabindex: index === 0 ? "0" : "-1",
    "aria-controls": index === 0 ? "market-plugins" : "market-skills",
    onclick: () => choose(index),
  }, title));
  function choose(index) {
    tabs.forEach((tab, at) => { tab.setAttribute("aria-selected", String(at === index)); tab.tabIndex = at === index ? 0 : -1; });
    body.hidden = index !== 0; skillBody.hidden = index !== 1;
    if (index === 1 && !skillControl) skillControl = skillsMarketplace(skillBody, ark);
  }
  const kinds = h("div.market-types", { role: "tablist", "aria-label": "Catalog type" }, tabs);
  kinds.addEventListener("keydown", event => {
    const at = tabs.indexOf(document.activeElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? 1 : ["ArrowLeft", "ArrowRight"].includes(event.key) ? 1 - at : null;
    if (at < 0 || next === null) return;
    event.preventDefault(); choose(next); tabs[next].focus();
  });


  const result = (promise) => promise.then((value) => ({ value }), (error) => ({ error }));
  const load = async () => {
    const current = ++generation;
    loading = true;
    draw();
    const [accountResult, catalogResult, linkedResult, capabilityResult] = await Promise.all([
      ark.ext ? result(ark.ext("desktop.account")) : Promise.resolve({ value: null }),
      ark.ext ? result(ark.ext("desktop.plugins_catalog")) : Promise.resolve({ value: null }),
      ark.ext ? result(ark.ext("desktop.plugins_connections")) : Promise.resolve({ value: null }),
      result(ark.call("capabilities")),
    ]);
    if (!alive || current !== generation) return;
    account = accountResult.value;
    catalogError = catalogResult.error || null;
    connectionsError = linkedResult.error || null;
    capabilityError = capabilityResult.error || null;
    const connections = new Map((linkedResult.value?.connections || []).map((connection) => [connection.offer, connection]));
    const managed = Array.isArray(catalogResult.value?.offers) ? catalogResult.value.offers.map((offer) => ({
      id: offer.id,
      source: "managed",
      name: offer.name,
      mark: offer.name?.slice(0, 1).toUpperCase() || "•",
      summary: offer.summary,
      category: offer.categories?.[0] || "Plugins",
      operations: offer.operations,
      disclosure: offer.disclosure,
      connection: connections.get(offer.id),
    })) : [];
    const local = Array.isArray(capabilityResult.value) ? capabilityResult.value
      .filter((capability) => !capability.added && (capability.switch || capability.account || capability.state === "connect" || (capability.state === "unavailable" && capability.provider)))
      .map((capability) => ({
        id: `capability:${capability.id}`,
        capabilityId: capability.id,
        source: "local",
        name: capability.label,
        mark: capability.label?.slice(0, 1).toUpperCase() || "•",
        summary: capability.says.charAt(0).toUpperCase() + capability.says.slice(1),
        category: "On this computer",
        switch: !!capability.switch,
        on: capability.state === "ready",
        connection: !capability.switch && capability.state === "ready" ? { id: capability.id, state: "connected", accountLabel: capability.account } : null,
        unavailable: capability.state === "unavailable",
        unavailableReason: capability.why,
      })) : [];
    const appPlugins = [...registry.plugins.values()].map(entry => ({ ...entry, id: `app:${entry.id}`, source: "app" }));
    offers = [...appPlugins, ...managed, ...local];
    loading = false;
    draw();
  };

  const connect = async (entry) => {
    if (connecting.has(entry.id)) return;
    connecting.add(entry.id);
    notice.textContent = "";
    draw();
    try {
      if (entry.source === "local") {
        const { url } = await ark.call("capability_connect", { id: entry.capabilityId });
        openOutside(url);
      } else {
        await ark.ext("desktop.plugins_connect", { offer: entry.id });
      }
      notice.textContent = "Finish connecting in your browser. This page will update when it is ready.";
      const until = Date.now() + 300000;
      while (alive && Date.now() < until) {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        if (!alive) return;
        await load();
        const updated = offers.find((offer) => offer.id === entry.id);
        if (updated?.connection?.state === "connected") {
          ark.toast(`${entry.name} connected`);
          return;
        }
      }
    } catch (error) { notice.textContent = error.message || String(error); }
    finally {
      connecting.delete(entry.id);
      draw();
    }
  };

  const disconnect = async (entry) => {
    if (!entry.connection) return;
    notice.textContent = "";
    try {
      if (entry.source === "local") await ark.call("capability_disconnect", { id: entry.capabilityId });
      else await ark.ext("desktop.plugins_disconnect", { id: entry.connection.id });
      await load();
    } catch (error) { notice.textContent = error.message || String(error); }
  };

  const turn = async (entry) => {
    if (connecting.has(entry.id)) return;
    connecting.add(entry.id);
    notice.textContent = "";
    draw();
    try { await ark.call("capability_turn", { id: entry.capabilityId, on: !entry.on }); }
    catch (error) { notice.textContent = error.message || String(error); }
    finally {
      connecting.delete(entry.id);
      await load();
    }
  };

  const switchButton = (entry, size = ".small") => {
    const label = entry.on ? "Turn off" : "Turn on";
    return h(`button.btn${entry.on ? ".ghost" : ".primary"}${size}.market-switch`, {
      type: "button", "aria-label": `${label} ${entry.name}`, disabled: connecting.has(entry.id), onclick: () => turn(entry),
    }, label);
  };

  const action = (entry) => {
    if (entry.source === "app") return h("button.btn.ghost.small", { type: "button", "aria-label": `Open ${entry.name}`, onclick: () => { selected = entry; draw(); } }, "Open");
    if (entry.switch) return switchButton(entry);
    if (entry.connection?.state === "connected") return h("button.btn.ghost.small", { type: "button", disabled: true }, "Connected");
    const pending = connecting.has(entry.id);
    const button = h("button.btn.primary.small.market-add", { type: "button", "aria-label": `Connect ${entry.name}` }, pending ? "Connecting…" : "Connect");
    button.disabled = pending || entry.unavailable || (entry.source === "managed" && !!connectionsError);
    button.onclick = () => connect(entry);
    return button;
  };

  const row = (entry) => h("div.market-row", { "data-id": entry.id },
    h("button.market-open", { type: "button", onclick: () => { selected = entry; draw(); }, "aria-label": `View ${entry.name}` },
      entryIcon(entry), h("span.market-copy", h("strong", entry.name), h("small", entry.summary))), action(entry));

  const emptyState = () => {
    if (loading) return h("p.settings-none", { role: "status" }, "Loading plugins…");
    if (query) return h("p.settings-none", { role: "status" }, "No plugins match that search.");
    if (catalogError && account?.state === "signed_out") {
      return h("div.settings-none", { role: "status" },
        h("p", "Sign in to your ARK Account to see managed plugins."),
        h("button.btn.primary.small", { type: "button", onclick: () => ark.openSettings("account") }, "Open Account"));
    }
    if (catalogError) return h("div.settings-none", { role: "status" },
      h("p", "Managed plugins could not be loaded."),
      h("button.btn.ghost.small", { type: "button", onclick: load }, "Retry"));
    return h("p.settings-none", { role: "status" }, "No plugins are available yet.");
  };

  const catalogStatus = () => {
    const statuses = [];
    if (catalogError && account?.state === "signed_out") statuses.push(h("div.market-notice", { role: "status" },
      "Sign in to your ARK Account to see managed plugins. ",
      h("button.btn.ghost.small", { type: "button", onclick: () => ark.openSettings("account") }, "Open Account")));
    else if (catalogError) statuses.push(h("div.market-notice", { role: "status" }, "Managed plugins could not be loaded. ",
      h("button.btn.ghost.small", { type: "button", onclick: load }, "Retry")));
    if (connectionsError && offers.some((entry) => entry.source === "managed")) statuses.push(h("div.market-notice", { role: "status" },
      "Connection status could not be loaded. ", h("button.btn.ghost.small", { type: "button", onclick: load }, "Retry")));
    if (capabilityError) statuses.push(h("div.market-notice", { role: "status" }, "Plugins on this computer could not be loaded. ",
      h("button.btn.ghost.small", { type: "button", onclick: load }, "Retry")));
    return statuses;
  };

  const drawCatalog = () => {
    const focused = el.contains(document.activeElement) && document.activeElement.classList.contains("market-search");
    const start = focused ? document.activeElement.selectionStart : null;
    const end = focused ? document.activeElement.selectionEnd : null;
    const filtered = offers.filter((entry) => `${entry.name} ${entry.summary} ${entry.category}`.toLowerCase().includes(query.toLowerCase()));
    const catalog = h("div.market-catalog");
    for (const category of [...new Set(filtered.map((entry) => entry.category))]) {
      const categoryEntries = filtered.filter((entry) => entry.category === category);
      catalog.append(h("h2.market-heading", category), h("div.market-grid", categoryEntries.map(row)));
    }
    if (!filtered.length) catalog.append(emptyState());
    const search = h("input.market-search", { type: "search", placeholder: "Search plugins", "aria-label": "Search plugins", value: query });
    search.addEventListener("input", () => {
      query = search.value;
      drawCatalog();
    });
    clear(body).append(
      h("div.market-top", search),
      catalog,
      ...catalogStatus(),
      notice,
    );
    if (focused) {
      search.focus();
      search.setSelectionRange(start, end);
    }
  };

  const drawDetail = (entry) => {
    const connection = entry.connection;
    const pending = connecting.has(entry.id);
    const back = h("button.btn.ghost.small", { type: "button", onclick: () => { selected = null; draw(); } }, "Back");
    if (entry.source === "app") {
      const host = h("div.market-plugin-settings");
      clear(body).append(h("div.market-detail-head", back),
        h("section.market-detail", entryIcon(entry), h("div", h("h2", entry.name), h("p", entry.summary))),
        entry.skills?.length ? h("section", h("h3.market-heading", "Included skills"), h("ul.market-includes", entry.skills.map(skill => h("li", skill.name)))) : null,
        host);
      mountedAppId = entry.id;
      try { detailControl = entry.render(host, ark) || null; }
      catch (error) { host.append(h("p.market-notice", { role: "alert" }, error.message || String(error))); }
      return;
    }
    if (entry.switch) {
      clear(body).append(
        h("div.market-detail-head", back),
        h("section.market-detail", entryIcon(entry), h("div", h("h2", entry.name), h("p", entry.summary))),
        h("div.market-detail-actions", switchButton(entry, ""), h("p.market-state", "ARK asks before each step.")),
        ...catalogStatus(),
        notice,
      );
      return;
    }
    const primary = connection?.state === "connected"
      ? h("button.btn.primary", { type: "button", disabled: true }, "Connected")
      : h("button.btn.primary", { type: "button", disabled: pending || entry.unavailable || (entry.source === "managed" && !!connectionsError), onclick: () => connect(entry) }, pending ? "Connecting…" : "Connect");
    const data = entry.disclosure?.payloadProcessors?.map((processor) => processor.name).join(", ") || "The connection provider processes requested actions";
    clear(body).append(
      h("div.market-detail-head", back),
      h("section.market-detail", entryIcon(entry), h("div", h("h2", entry.name), h("p", entry.summary))),
      h("div.market-detail-actions", primary,
        h("p.market-state", entry.unavailable ? entry.unavailableReason : connection?.state === "connected" ? "Connected" : "Continue in your browser")),
      h("dl.market-facts", h("div", h("dt", "Connection"), h("dd", entry.source === "managed" ? "Requires an ARK Account" : "Stored on this computer")), h("div", h("dt", "Data"), h("dd", data))),
      connection?.state === "connected" ? h("button.btn.ghost.small.market-remove-detail", { type: "button", onclick: () => disconnect(entry) }, "Disconnect") : null,
      ...catalogStatus(),
      notice,
    );
  };

  const draw = () => {
    if (!alive || (selected?.source === "app" && mountedAppId === selected.id)) return;
    detailControl?.dispose?.();
    detailControl = null;
    mountedAppId = null;
    if (selected) drawDetail(offers.find(entry => entry.id === selected.id) || selected);
    else drawCatalog();
  };
  const visibility = new MutationObserver(() => {
    if (!el.closest("[hidden]")) return;
    detailControl?.dispose?.();
    detailControl = null;
    selected = null;
    mountedAppId = null;
    clear(body);
  });
  visibility.observe(el, { attributes: true, attributeFilter: ["hidden"] });
  if (el.parentElement) visibility.observe(el.parentElement, { attributes: true, attributeFilter: ["hidden"] });
  el.append(kinds, body, skillBody);
  draw();
  load();
  return { refresh: () => Promise.all([load(), skillControl?.refresh()]), dispose() { alive = false; skillControl?.dispose(); generation += 1; visibility.disconnect(); detailControl?.dispose?.(); detailControl = null; } };
}

export function registerMarketplace(ark) {
  ark.view({ id: "marketplace", title: "Plugins", icon: "plug", nav: "Plugins", render: (el) => marketplace(el, ark) });
}
