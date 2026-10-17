// Settings › WhatsApp: connect a WhatsApp Business number (`whatsapp`, `whatsapp_connect`,
// `capability_disconnect`). The host checks the five values with Meta before it keeps them and
// answers with the path Meta sends messages to. Shown when the app can connect accounts.

import { h, icon, clear } from "./dom.js";

/// In the order Meta's dashboard shows them.
const FIELDS = [
  ["waba_id", "Business account ID", false, /business account/i],
  ["phone_id", "Phone number ID", false, /phone number id/i],
  ["token", "Access token", true, /access token/i],
  ["app_secret", "App secret", true, /app secret/i],
  ["verify_token", "Verify token", true, /verify token/i],
];
const META = "https://developers.facebook.com/docs/whatsapp/cloud-api/get-started";

function section(el, ark) {
  el.classList.add("whatsapp");
  const list = h("div.plugin-list");
  list.hidden = true;
  const why = h("p.plugin-why", { role: "status" });
  const inputs = Object.fromEntries(FIELDS.map(([key, label, secret]) => [key, h("input.plugin-input.wa-input", {
    type: secret ? "password" : "text",
    name: key,
    autocomplete: secret ? "new-password" : "off",
    spellcheck: "false",
  })]));
  const connect = h("button.btn.primary", { type: "submit", disabled: true }, "Connect");
  const find = h("a.plugin-hint", { href: META, target: "_blank", rel: "noopener noreferrer" }, "Find these in Meta");
  const form = h("form.plugin-add.wa-form", { novalidate: true }, ...FIELDS.map(([key, label]) => h("label.wa-field", h("span.plugin-label", label), inputs[key])), h("div.plugin-bar", find, connect), why);
  let checking = false;
  let known = new Set();
  const timers = new Set();

  const ready = () => FIELDS.every(([key]) => inputs[key].value.trim());
  const sync = () => {
    connect.disabled = checking || !ready();
    for (const i of Object.values(inputs)) i.disabled = checking;
  };
  const mark = (text) => {
    for (const [key, , , rx] of FIELDS) inputs[key].removeAttribute("aria-invalid");
    why.textContent = text;
    const bad = text && FIELDS.find(([, , , rx]) => rx.test(text));
    if (bad) {
      inputs[bad[0]].setAttribute("aria-invalid", "true");
      inputs[bad[0]].focus();
    }
  };
  const later = (fn, ms) => {
    const t = setTimeout(() => { timers.delete(t); fn(); }, ms);
    timers.add(t);
  };

  const draw = (numbers) => {
    known = new Set(numbers.map((n) => n.id));
    clear(list);
    list.hidden = !numbers.length;
    for (const n of numbers) {
      const copy = h("button.icon-btn.small", { type: "button", title: "Copy", "aria-label": `Copy callback path of ${n.name}` }, icon("copy"));
      copy.onclick = async () => {
        try {
          await navigator.clipboard.writeText(n.path);
          clear(copy).append(icon("check"));
          later(() => clear(copy).append(icon("copy")), 1500);
        } catch {
          ark.toast("Copy was refused. Select the path and copy it.");
        }
      };
      const remove = h("button.btn.ghost.small.plugin-remove", { type: "button", "aria-live": "polite" }, "Disconnect");
      remove.onclick = async () => {
        if (!remove.classList.contains("sure")) {
          remove.classList.add("sure");
          remove.textContent = "Disconnect?";
          later(() => {
            remove.classList.remove("sure");
            remove.textContent = "Disconnect";
          }, 3000);
          return;
        }
        remove.disabled = true;
        why.textContent = "";
        try {
          await ark.call("capability_disconnect", { id: n.id });
        } catch (e) {
          why.textContent = e.message || String(e);
        }
        await load();
      };
      list.append(h("div.plugin-row", { "data-id": n.id },
        h("span.plugin-dot", { title: "Connected", "aria-label": "Connected" }),
        h("div.plugin-main", h("div.plugin-name", { title: n.name }, n.name), h("div.plugin-cmd", { title: n.path }, n.path)),
        h("div.plugin-actions", copy, remove)));
    }
  };

  const load = async () => {
    try {
      draw((await ark.call("whatsapp"))?.numbers || []);
    } catch (e) {
      why.textContent = e.message || String(e);
    }
  };

  form.addEventListener("input", () => { sync(); if (why.textContent) mark(""); });
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (checking || !ready()) return;
    checking = true;
    connect.textContent = "Checking…";
    mark("");
    sync();
    let failed = "";
    try {
      const params = Object.fromEntries(FIELDS.map(([key]) => [key, inputs[key].value.trim()]));
      const r = await ark.call("whatsapp_connect", params);
      const mine = (r.numbers || []).find((n) => n.id.endsWith(`:${params.waba_id}/${params.phone_id}`));
      const again = mine && known.has(mine.id);
      for (const i of Object.values(inputs)) i.value = "";
      draw(r.numbers || []);
      ark.toast(`${mine?.name || "The number"} ${again ? "updated" : "connected"}`);
    } catch (err) {
      failed = err.message || String(err);
    }
    checking = false;
    connect.textContent = "Connect";
    sync();
    if (failed) mark(failed);
  });

  el.append(list, form);
  load();
  return {
    refresh: load,
    focus: () => inputs.waba_id,
    dispose() {
      for (const t of timers) clearTimeout(t);
      for (const i of Object.values(inputs)) i.value = "";
    },
  };
}

/// Offer the section when the app can connect accounts.
export async function registerWhatsapp(ark) {
  const r = await ark.call("whatsapp").catch(() => null);
  if (!r?.can_connect) return;
  ark.setting({ id: "whatsapp", title: "WhatsApp", icon: "phone", builtin: true, render: (el) => section(el, ark) });
}
