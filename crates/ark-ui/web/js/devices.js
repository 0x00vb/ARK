// Settings › Devices (ADR 0047): notifications on this device, a code to sign in another one,
// and the devices that get notifications. The runtime sends a notification when something has
// waited a minute and nobody is looking (`push_*`); this page subscribes, says when it is in
// view, and opens the item a notification is about (`#item=`).

import { h, clear } from "./dom.js";
import { settingRow } from "./settings.js";

const reg = () => navigator.serviceWorker?.register("sw.js", { scope: "/" });

/// Why notifications cannot work here, or null. The window of the desktop app notifies by
/// itself; a page that is not on a secure address, or an iPhone page not added to the Home
/// Screen, cannot subscribe.
function blocked() {
  if (window.__TAURI__) return "hidden";
  if (!window.isSecureContext) return "This device needs the runtime's https address.";
  const apple = /iPhone|iPad|iPod/.test(navigator.userAgent);
  const standalone = matchMedia("(display-mode: standalone)").matches || navigator.standalone;
  if (apple && !standalone) return "Add this page to the Home Screen first (Share › Add to Home Screen), then open it from there.";
  if (!("serviceWorker" in navigator) || !("PushManager" in window) || !("Notification" in window)) return "hidden";
  if (Notification.permission === "denied") return "Notifications are blocked for this site in the browser's settings.";
  return null;
}

async function subscription() {
  const r = await navigator.serviceWorker?.getRegistration("/");
  return r ? r.pushManager.getSubscription() : null;
}

/// A name for this device the person recognises in the list.
function deviceName() {
  const ua = navigator.userAgent;
  const what = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : /Mac/.test(ua) ? "Mac" : /Windows/.test(ua) ? "Windows" : "Linux";
  const app = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "browser";
  return `${what} ${app}`;
}

const b64 = (s) => {
  const p = "=".repeat((4 - (s.length % 4)) % 4);
  const raw = atob((s + p).replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
};

async function turnOn(ark) {
  if ((await Notification.requestPermission()) !== "granted") throw new Error("Notifications are blocked for this site in the browser's settings.");
  const r = await reg();
  await navigator.serviceWorker.ready;
  const { key } = await ark.call("push_key");
  if (!key) throw new Error("The runtime cannot send notifications.");
  const sub = (await r.pushManager.getSubscription()) || (await r.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64(key) }));
  await ark.call("push_add", { subscription: sub.toJSON(), name: deviceName() });
}

async function turnOff(ark) {
  const sub = await subscription();
  if (!sub) return;
  await ark.call("push_remove", { endpoint: sub.endpoint }).catch(() => {});
  await sub.unsubscribe().catch(() => {});
}

/// Keep what the runtime knows in step with the browser: a subscription the browser renewed
/// (Safari says nothing when it does) is sent again, and the page says when it is in view so
/// this device is not notified of what it shows.
export async function keepInStep(ark) {
  if (blocked() || Notification.permission !== "granted") return;
  const sub = await subscription().catch(() => null);
  if (!sub) return;
  ark.call("push_add", { subscription: sub.toJSON(), name: deviceName() }).catch(() => {});
  const here = () => ark.call("push_here", { endpoint: sub.endpoint, visible: document.visibilityState === "visible" }).catch(() => {});
  document.addEventListener("visibilitychange", here);
  here();
  setInterval(() => document.visibilityState === "visible" && here(), 60_000);
}

/// The item a notification is about: in the address (`#item=`, a tap that opened the page) and
/// from the worker (a tap on a page already open).
export function openItems(open) {
  const take = () => {
    const id = new URLSearchParams(location.hash.slice(1)).get("item");
    if (!id) return;
    history.replaceState(null, "", location.pathname + location.search);
    open(id);
  };
  take();
  addEventListener("hashchange", take);
  navigator.serviceWorker?.addEventListener("message", (e) => e.data?.kind === "open_item" && open(e.data.id));
}

function section(el, ark, pairing) {
  el.classList.add("plugins");
  const why = h("p.plugin-why", { role: "status" });
  const mine = h("div.devices-mine");
  const others = h("div.plugin-list.devices-others");
  let busy = false;

  const drawMine = async () => {
    clear(mine);
    const no = blocked();
    if (no === "hidden") return;
    const on = !no && Notification.permission === "granted" && !!(await subscription().catch(() => null));
    const seg = (label, want) =>
      h("button.seg", {
        type: "button", role: "radio", "aria-checked": String(on === want), "aria-pressed": String(on === want),
        disabled: !!no || busy,
        onclick: async () => {
          if (on === want || busy) return;
          busy = true;
          why.textContent = "";
          drawMine();
          try {
            await (want ? turnOn(ark) : turnOff(ark));
          } catch (e) {
            why.textContent = e.message || String(e);
          }
          busy = false;
          drawMine();
          drawOthers();
        },
      }, label);
    mine.append(settingRow("Notifications on this device", no, h("div.segs", { role: "radiogroup", "aria-label": "Notifications on this device" }, seg("Off", false), seg("On", true))));
  };

  const drawOthers = async () => {
    const all = (await ark.call("push_devices").catch(() => [])) || [];
    clear(others);
    for (const d of all) {
      const remove = h("button.btn.ghost.small.plugin-remove", { type: "button", "aria-label": `Stop notifying ${d.name}` }, "Remove");
      remove.onclick = async () => {
        remove.disabled = true;
        await ark.call("push_remove", { id: d.id }).catch((e) => (why.textContent = e.message));
        drawOthers();
        drawMine();
      };
      others.append(settingRow(d.name, `Notified since ${new Date(d.added_ms).toLocaleDateString()}`, remove));
    }
  };

  const add = h("div.pair-add");
  if (pairing) {
    const show = h("div.pair-code", { "aria-live": "polite" });
    const make = h("button.btn.small", { type: "button" }, "Add a device");
    make.onclick = async () => {
      make.disabled = true;
      try {
        const r = await fetch("/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "" }) });
        if (!r.ok) throw new Error("A code could not be made.");
        const { code, expires_ms } = await r.json();
        clear(show).append(h("code.pair-value", code), h("span.plugin-detail", ` until ${new Date(expires_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`));
      } catch (e) {
        why.textContent = e.message || String(e);
      }
      make.disabled = false;
    };
    add.append(settingRow("Sign in another device", "Open this page's address there and type the code.", make), show);
  }

  // One card: this device, a code for another, then the devices that get notifications.
  el.append(h("div.plugin-list", mine, add), others, why);
  drawMine();
  drawOthers();
  return { refresh: () => (drawMine(), drawOthers()) };
}

/// Offer the section where a device can be notified or another one added.
export async function registerDevices(ark) {
  const s = await fetch("/session", { cache: "no-store" }).then((r) => (r.ok ? r.json() : null), () => null);
  const pairing = !!s?.pairing && !window.__TAURI__;
  if (blocked() === "hidden" && !pairing) return;
  ark.setting({ id: "devices", title: "Devices", icon: "phone", builtin: true, render: (el) => section(el, ark, pairing) });
}
