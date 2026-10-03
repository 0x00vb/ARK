// Settings › Skills: the skills of the project folder (`.agents/skills`) and of ARK's store
// (`skills`), read by the conversation when a task fits, and by each agent given them (`skills`;
// the same as `/skills`). Adding one is the person's: the Terminal opens in the project folder
// with `npx skills add` typed, and Enter is theirs.

import { h, clear } from "./dom.js";

function section(el, ark) {
  const list = h("div.plugin-list.skill-list");
  const pkg = h("input.plugin-input.skill-pkg", {
    spellcheck: "false",
    "aria-label": "A skill package",
    placeholder: "owner/repo, e.g. vercel-labs/agent-skills",
  });
  const open = h("button.btn.primary", { type: "button" }, "Open in Terminal");
  open.onclick = () => {
    const name = pkg.value.trim();
    ark.openPanel("terminal", { locator: `npx skills add ${name}${name ? "" : " "}` });
  };
  pkg.addEventListener("keydown", (e) => e.key === "Enter" && open.click());
  const again = h("button.btn.ghost.small", { type: "button" }, "Refresh");

  const load = async () => {
    const all = (await ark.call("skills").catch(() => [])) || [];
    clear(list);
    if (!all.length) {
      list.append(h("p.settings-none", "No skills yet."));
      return;
    }
    for (const s of all) {
      list.append(h("div.plugin-row.skill-row", { "data-name": s.name },
        h("div.plugin-main",
          h("div.plugin-name", s.name, h("span.skill-scope", s.scope === "project" ? "This project" : "All projects")),
          h("div.skill-desc", { title: s.description }, s.description))));
    }
  };
  again.onclick = load;

  el.append(
    h("p.settings-lead", "Instructions for kinds of work. Only their names reach the model until a task needs one. The conversation reads any of them; each agent reads the ones it is given."),
    list,
    h("div.plugin-add",
      h("label.plugin-label", "Add skills"),
      h("div.skill-add", pkg, open),
      h("div.plugin-bar",
        h("span.plugin-hint", "Installs into this project's .agents/skills after you press Enter there. A folder with a SKILL.md in ARK's store skills folder serves every project."),
        again)));
  load();
  return { refresh: load };
}

/// Offer the section when the host has skills (an ARK host always answers `skills`).
export async function registerSkills(ark) {
  const has = await ark.call("skills").then(Array.isArray, () => false);
  if (has) ark.setting({ id: "skills", title: "Skills", icon: "layers", builtin: true, render: (el) => section(el, ark) });
}
