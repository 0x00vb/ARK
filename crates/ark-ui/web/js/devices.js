// Settings › Devices (ADR 0047): notifications on this device, a code to sign in another one,
// and the devices that get notifications. The runtime sends a notification when something has
// waited a minute and nobody is looking (`push_*`); this page subscribes, says when it is in
// view, and opens the item a notification is about (`#item=`).

import { h, clear } from "./dom.js";
import { settingRow } from "./settings.js";

const reg = () => navigator.serviceWorker?.register("sw.js", { scope: "/" });

/// Why notifications cannot work here, or null. The window of the desktop app notifies by
/// itself; a page that is not on a secure address, or an Apple phone's page not added to the Home
/// Screen, cannot subscribe.
function blocked() {
  if (window.__TAURI__) return "hidden";
  if (!window.isSecureContext) return "Open this page at its https address first.";
  // An Apple phone or tablet: Safari there pushes only to a page on the Home Screen.
  const apple = /\biP(hone|ad|od)\b/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
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
  const touch = navigator.maxTouchPoints > 1;
  const what = /Mobile|Android.+Mobile|\biP(hone|od)\b/.test(ua) ? "Phone" : touch && /\biP(ad)\b|Android|Macintosh/.test(ua) ? "Tablet" : /Mac/.test(ua) ? "Mac" : /Windows/.test(ua) ? "Windows" : "Linux";
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
  if (!key) throw new Error("Notifications cannot be sent from here.");
  const sub = (await r.pushManager.getSubscription()) || (await r.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64(key) }));
  await ark.call("push_add", { subscription: sub.toJSON(), name: deviceName() });
}

/// A name for this device, as Settings › Devices lists it.
export { deviceName };

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
  // Removed from another device: it stays removed until turned on here again.
  const r = await ark.call("push_add", { subscription: sub.toJSON(), name: deviceName(), renew: true }).catch(() => null);
  if (r?.removed) return sub.unsubscribe().catch(() => {});
  const here = () => ark.call("push_here", { endpoint: sub.endpoint, visible: document.visibilityState === "visible" }).catch(() => {});
  document.addEventListener("visibilitychange", here);
  addEventListener("pagehide", () => ark.call("push_here", { endpoint: sub.endpoint, visible: false }).catch(() => {}));
  here();
  setInterval(() => document.visibilityState === "visible" && here(), 20_000);
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

  // Devices signed in with a code (Sign out ends their session and notifications), then the
  // others that get notifications (Stop).
  const drawOthers = async () => {
    const [signed, pushed] = await Promise.all([
      ark.call("devices_signed_in").catch(() => []),
      ark.call("push_devices").catch(() => []),
    ]);
    clear(others);
    const day = (ms) => new Date(ms).toLocaleDateString();
    for (const d of signed || []) {
      const out = h("button.btn.ghost.small.plugin-remove", { type: "button", "aria-label": `Sign out ${d.device}` }, "Sign out");
      out.onclick = async () => {
        out.disabled = true;
        await ark.call("device_sign_out", { name: d.name }).catch((e) => (why.textContent = e.message));
        drawOthers();
      };
      const detail = `${d.notified ? "Notified · " : ""}signed in until ${day(d.expires_ms)}`;
      others.append(settingRow(d.device, detail[0].toUpperCase() + detail.slice(1), out));
    }
    for (const d of pushed || []) {
      if ((signed || []).some((s) => s.notified && d.name && s.device === d.name)) continue;
      const stop = h("button.btn.ghost.small.plugin-remove", { type: "button", "aria-label": `Stop notifying ${d.name}` }, "Stop");
      stop.onclick = async () => {
        stop.disabled = true;
        await ark.call("push_remove", { id: d.id }).catch((e) => (why.textContent = e.message));
        drawOthers();
        drawMine();
      };
      others.append(settingRow(d.name, `Notified since ${day(d.added_ms)}`, stop));
    }
  };

  const add = h("div.pair-add");
  if (pairing) {
    const show = h("div.pair-code", { "aria-live": "polite" });
    const make = h("button.btn.small", { type: "button" }, "Add a device");
    make.onclick = async () => {
      make.disabled = true;
      try {
        const { code, expires_ms, qr_svg } = await ark.pairCode();
        clear(show);
        if (qr_svg) {
          const image = h("img.pair-qr", { alt: "Scan to connect ARK for iPhone" });
          image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(qr_svg)}`;
          show.append(image, h("strong.pair-scan", "Scan with ARK for iPhone"));
        }
        show.append(
          h("span.plugin-detail", "Or enter the short-lived code"),
          h("code.pair-value", code),
          h("span.plugin-detail", `Expires at ${new Date(expires_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`),
        );
      } catch (e) {
        why.textContent = e.message || String(e);
      }
      make.disabled = false;
    };
    add.append(settingRow("Add an iPhone", "Scan once in the ARK app.", make), show);
  }

  // One card: this device, a code for another, then the devices that get notifications.
  el.append(h("div.plugin-list", mine, add), others, why);
  drawMine();
  drawOthers();
  return { refresh: () => (drawMine(), drawOthers()) };
}

/// Offer the section where a device can be notified or another one added.
export async function registerDevices(ark) {
  // The native window notifies itself and has no browser session endpoint.
  if (window.__TAURI__) return;
  let pairing = false;
  try {
    const r = await fetch("/session", { cache: "no-store" });
    pairing = r.ok && (await r.json())?.pairing === true;
  } catch {
    // Offline or not JSON: the section still works without offering a pairing code.
    pairing = false;
  }
  if (blocked() === "hidden" && !pairing) return;
  ark.setting({ id: "devices", title: "Devices", icon: "phone", builtin: true, render: (el) => section(el, ark, pairing) });
}
