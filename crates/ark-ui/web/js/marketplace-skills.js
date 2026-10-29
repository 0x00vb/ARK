import { h, clear, icon } from "./dom.js";

const key = (location) => location ? JSON.stringify([location.runtime, location.project, location.session]) : "";

export function skillsMarketplace(el, ark) {
  let rows = [], query = "", installedOnly = false, selected = null;
  let loading = true, packageError = "", skillError = "", notice = "";
  let generation = 0, alive = true, targetKey = "", target = ark;
  const busy = new Set();
  el.classList.add("market-skills");

  function location() { return ark.summary?.()?.location; }
  function current() { return key(location()); }
  function select(row) { selected = row.id; draw(); }

  async function load() {
    const wanted = current(), at = ark.at && location() ? ark.at({ ...location() }) : ark;
    const request = ++generation;
    if (targetKey !== wanted) { rows = []; selected = null; notice = ""; }
    targetKey = wanted; target = at; loading = true;
    draw();
    const [packages, skills] = await Promise.allSettled([at.call("skill_packages"), at.call("skills")]);
    if (!alive || request !== generation || wanted !== current()) return;
    packageError = packages.status === "rejected" ? String(packages.reason?.message || packages.reason) : !Array.isArray(packages.value) ? "Invalid skill package catalog" : "";
    skillError = skills.status === "rejected" ? String(skills.reason?.message || skills.reason) : !Array.isArray(skills.value) ? "Invalid installed skill list" : "";
    const previousPackages = rows.filter(row => row.source === "package");
    const previousSkills = rows.filter(row => row.source !== "package");
    const offered = packageError ? previousPackages : packages.value.map(row => ({ ...row, source: "package", category: "Available skills" }));
    const existing = skillError ? previousSkills : skills.value.filter(row => !row.package).map(row => ({
      ...row, id: `${row.scope}:${row.name}`, summary: row.description, installed: true, available: false,
      category: row.scope === "builtin" ? "Included with ARK" : row.scope === "project" ? "This project" : "On this computer",
      source: row.scope === "builtin" ? "builtin" : "local",
    }));
    rows = [...offered, ...existing]; loading = false; draw();
  }

  const pending = row => busy.has(`${targetKey}:${row.id}`);

  async function act(row, method) {
    if (pending(row)) return;
    if (targetKey !== current()) { notice = "The destination changed. Review the current skills."; return load(); }
    const at = target, wanted = targetKey;
    const operationKey = `${wanted}:${row.id}`;
    busy.add(operationKey); notice = ""; draw();
    try {
      await at.call(method, { id: row.id, digest: row.digest });
      if (alive && wanted === current()) notice = method === "skill_package_install" ? `${row.name} installed.` : `${row.name} removed. Local files were kept.`;
    } catch (error) {
      if (alive && wanted === current()) notice = error.message || String(error);
    } finally {
      busy.delete(operationKey);
      if (alive) await load();
    }
  }

  function action(row) {
    if (row.source !== "package") return h("span.market-skill-state", row.source === "builtin" ? "Included" : "Installed");
    if (row.installed && !row.update_available) return h("span.market-skill-state", row.error ? "Needs attention" : "Installed");
    if (!row.available) return null;
    const inProgress = pending(row);
    return h("button.btn.primary.small", { type: "button", "aria-label": `${row.update_available ? "Update" : "Install"} ${row.name}`, disabled: inProgress || !!packageError, onclick: () => act(row, "skill_package_install") }, inProgress ? "Installing…" : row.update_available ? "Update" : "Install");
  }

  function status() {
    const result = [];
    if (packageError || skillError) result.push(h("div.market-notice", { role: "status" },
      h("span", [packageError && "Skill packages are unavailable. Showing the last update when available.", skillError && "Installed skills could not be read."].filter(Boolean).join(" ")),
      h("button.btn.ghost.small", { type: "button", onclick: load }, "Retry")));
    if (notice) result.push(h("p.market-notice", { role: "status" }, notice));
    return result;
  }

  function draw() {
    if (!alive) return;
    const focused = el.contains(document.activeElement) && document.activeElement.classList.contains("market-search");
    const range = focused ? [document.activeElement.selectionStart, document.activeElement.selectionEnd] : null;
    clear(el); el.setAttribute("aria-busy", String(loading));
    const detail = selected && rows.find(row => row.id === selected);
    if (detail) {
      const back = h("button.btn.ghost.small", { type: "button", onclick: () => { selected = null; draw(); el.querySelector(".market-search")?.focus(); } }, "Back");
      el.append(h("div.market-detail-head", back), h("section.market-detail", h("div", h("h2", detail.name), h("p", detail.summary))),
        h("div.market-detail-actions", action(detail)),
        h("dl.market-facts", h("div", h("dt", "Available in"), h("dd", detail.source === "package" ? "All projects on the selected computer" : detail.category)),
          detail.publisher ? h("div", h("dt", "Publisher"), h("dd", detail.publisher)) : null,
          detail.version ? h("div", h("dt", "Version"), h("dd", detail.version)) : null),
        detail.skills?.length ? h("section", h("h3.market-heading", "Includes"), h("ul.market-includes", detail.skills.map(name => h("li", name)))) : null,
        detail.error ? h("p.market-notice", { role: "alert" }, detail.error) : null,
        detail.source === "package" && detail.installed ? h("button.btn.ghost.small", { type: "button", disabled: pending(detail) || !!packageError, onclick: () => act(detail, "skill_package_remove") }, pending(detail) ? "Removing…" : "Remove") : null,
        ...status());
      return;
    }
    const search = h("input.market-search", { type: "search", placeholder: "Search skills", "aria-label": "Search skills", value: query });
    search.addEventListener("input", () => { query = search.value; draw(); });
    const filters = h("div.market-filter", { "aria-label": "Skill filter" }, [false, true].map(installed => h("button.btn.ghost.small", {
      type: "button", "aria-pressed": String(installedOnly === installed), onclick: () => { installedOnly = installed; draw(); },
    }, installed ? "Installed" : "Browse")));
    const place = ark.summary?.()?.place?.name || (ark.isLocal?.() ? "this computer" : "the connected computer");
    el.append(h("div.market-top", search, filters), h("p.market-install-scope", `All projects on ${place}`));
    const filtered = rows.filter(row => (!installedOnly || row.installed) && `${row.name} ${row.summary} ${(row.skills || []).join(" ")}`.toLowerCase().includes(query.toLowerCase()));
    for (const category of [...new Set(filtered.map(row => row.category))]) {
      el.append(h("h2.market-heading", category), h("div.market-grid", filtered.filter(row => row.category === category).map(row => h("div.market-row", { "data-id": row.id },
        h("button.market-open", { type: "button", "aria-label": `View ${row.name}`, onclick: () => select(row) }, h("span.market-skill-mark", icon("layers")), h("span.market-copy", h("strong", row.name), h("small", row.summary))), action(row)))));
    }
    if (!filtered.length) el.append(h("p.settings-none", { role: "status" }, loading ? "Loading skills…" : query ? "No skills match that search." : installedOnly ? "No installed skills." : packageError || skillError ? "" : "No skills are available yet."));
    el.append(...status());
    if (focused) { search.focus(); search.setSelectionRange(...range); }
  }
  const offSummary = ark.on?.("ark:summary", () => { if (targetKey !== current()) load(); });
  const offConnected = ark.on?.("ark:connected", load);
  load();
  return { refresh: load, dispose() { alive = false; generation++; offSummary?.(); offConnected?.(); } };
}
