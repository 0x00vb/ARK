// One protocol, two carriers: the desktop window (Tauri IPC) or a browser (HTTP + SSE).
// Views only see `call(method, params)` and `subscribe(fn)`.

// The window's own page comes from the app's asset protocol (`tauri:` on macOS and Linux,
// `http://tauri.localhost` on Windows). A window opened on a runtime elsewhere (ADR 0016) loads
// that runtime's page over HTTP: Tauri still defines `__TAURI__` there, but the runtime is not
// in this process, so the page uses HTTP like any browser.
function ownPage() {
  return location.protocol === "tauri:" || location.hostname === "tauri.localhost";
}

// The window that opened a runtime elsewhere in place of its own page (`ark_ui::remote::visit`)
// goes back to it when this address is loaded: the window catches it and never fetches it.
const OWN_PAGE = "https://ark-local.invalid/";

export function connect() {
  const tauri = window.__TAURI__;
  if (tauri && tauri.core && ownPage()) {
    return {
      kind: "desktop",
      shell: "desktop",
      local: true,
      call: (method, params = {}) =>
        tauri.core.invoke("ark_call", { method, params }).catch((e) => {
          throw new Error(typeof e === "string" ? e : e?.message || String(e));
        }),
      subscribe(fn) {
        const ch = new tauri.core.Channel();
        ch.onmessage = fn;
        tauri.core.invoke("ark_subscribe", { channel: ch });
      },
      pickFolder: (title) => tauri.core.invoke("ark_pick_folder", { title: title || null }),
      // The packaged app, not the runtime: signed updates from its embedded endpoint. No URL,
      // header, version or downgrade input crosses this bridge.
      updates: {
        status: () => tauri.core.invoke("ark_update_status"),
        check: () => tauri.core.invoke("ark_update_check"),
        listen(fn) {
          const ch = new tauri.core.Channel();
          ch.onmessage = fn;
          return tauri.core.invoke("ark_update_events", { channel: ch });
        },
      },
      pageUpdates: {
        status: () => tauri.core.invoke("ark_page_update_status"),
        check: () => tauri.core.invoke("ark_page_update_check"),
        apply: () => tauri.core.invoke("ark_page_update_apply"),
        ready: (revision) => tauri.core.invoke("ark_page_update_ready", { revision }),
        listen(fn) {
          const ch = new tauri.core.Channel();
          ch.onmessage = fn;
          return tauri.core.invoke("ark_page_update_events", { channel: ch });
        },
      },
      // The Browser panel's page in a webview of the window's own, placed over the panel
      // (ADR 0044): pages that refuse to be framed show too.
      browserView: {
        show: (r, url) => tauri.core.invoke("ark_browser_show", { url: url || null, x: r.x, y: r.y, width: r.width, height: r.height }),
        act: (action) => tauri.core.invoke("ark_browser", { action }),
        reach: (url) => tauri.core.invoke("ark_browser_reach", { url }),
        listen(fn) {
          const ch = new tauri.core.Channel();
          ch.onmessage = fn;
          return tauri.core.invoke("ark_browser_events", { channel: ch });
        },
      },
    };
  }
  const url = new URL(location.href);
  let token = url.searchParams.get("token");
  try {
    if (token) sessionStorage.setItem("ark-token", token);
    else token = sessionStorage.getItem("ark-token");
  } catch {}
  // A one-time link from the runtime's entry (ADR 0040): `#code=…`. It leaves the address at
  // once and is traded for a session the browser keeps in an HttpOnly cookie; the page never
  // holds a secret.
  const code = new URLSearchParams(url.hash.slice(1)).get("code");
  if (url.searchParams.has("token") || code) {
    url.searchParams.delete("token");
    // A code leaves the address; an item to open (`#item=`, a notification, another computer's
    // window) stays for the page to read.
    history.replaceState(null, "", url.pathname + url.search + (code ? "" : url.hash));
  }
  const ready = code
    ? fetch("/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code }) }).catch(() => {})
    : Promise.resolve();
  const headers = { "content-type": "application/json", ...(token ? { "x-ark-token": token } : {}) };
  // Where the entry is, learnt while the runtime answers: a runtime that went down cannot say.
  let entry = null;
  const session = async () => {
    try {
      const r = await fetch("/session", { cache: "no-store" });
      const s = r.ok ? await r.json() : null;
      if (s?.entry) entry = s.entry;
      return s;
    } catch {
      return null;
    }
  };
  if (!token) ready.then(session);
  // Without a session, the runtime's entry (an app's sign-in page) brings a fresh link. At most
  // once in 30 seconds, so a link that keeps failing shows the error instead of looping. Calls
  // that fail together share one answer: the page leaves once.
  let entering = null;
  const enter = () =>
    (entering ??= (async () => {
      const s = await session();
      if (!s?.entry) return false;
      const now = Date.now();
      let last = 0;
      try {
        last = Number(sessionStorage.getItem("ark-entered")) || 0;
        sessionStorage.setItem("ark-entered", String(now));
      } catch {}
      if (now - last < 30000) return false;
      location.replace(s.entry);
      return true;
    })().then((left) => {
      if (!left) entering = null;
      return left;
    }));
  // In the window on a runtime it visits: the window's title bar and a way back. In a
  // browser, or a window opened only on that runtime (`run_window`), neither.
  const inWindow = !!window.__TAURI__;
  return {
    kind: "browser",
    shell: inWindow ? "desktop" : "browser",
    openLocal: inWindow ? () => location.assign(OWN_PAGE) : undefined,
    // A browser on this machine's loopback address reaches a runtime on this machine; any other
    // address is a runtime elsewhere (ADR 0016).
    local: ["127.0.0.1", "localhost", "[::1]"].includes(location.hostname),
    // A page in a browser cannot see the runtime's disk: no native picker.
    pickFolder: async () => undefined,
    updates: undefined,
    pageUpdates: undefined,
    /// Trade a code typed from a signed-in device for a session (ADR 0047), naming this device.
    /// True when it worked.
    async signIn(code, name) {
      const r = await fetch("/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code, name }) }).catch(() => null);
      return !!r?.ok;
    },
    /// A code for another device (ADR 0047): `{code, expires_ms}`.
    async pairCode() {
      const r = await fetch("/pair", { method: "POST", headers, body: "{}" });
      if (!r.ok) throw new Error("A code could not be made.");
      return r.json();
    },
    async call(method, params = {}) {
      await ready;
      const r = await fetch("/rpc", { method: "POST", headers, body: JSON.stringify({ method, params }) });
      if (r.status === 401) {
        // Leaving for the entry: the call never settles, so nothing flashes an error.
        if (await enter()) return new Promise(() => {});
        // A runtime with saved tokens and no entry: a code from a signed-in device (ADR 0047).
        const s = await session();
        const e = new Error(s?.pairing ? "Sign in with a code" : "This page needs the link printed at start (with its token).");
        e.signIn = !!s?.pairing;
        throw e;
      }
      const j = await r.json();
      if (!j.ok) throw new Error(j.error);
      return j.result;
    },
    // The browser's EventSource retries every few seconds forever. Instead: close it on an
    // error and reconnect with exponential backoff (1 s doubling to 30 s, with jitter); after
    // `tries()` failures, stop and report `disconnected` with a `retry` function. A
    // reconnect reports `resync`: the page reloads its state and replays the running turn.
    // A session that ended goes to the entry. With an entry, the page stops trying sooner (about
    // 15 seconds) and `retry` goes there: it starts a runtime that stopped, then comes back.
    subscribe(fn) {
      const tries = () => (entry && !token ? 4 : 10);
      let failures = 0;
      let everOpen = false;
      const open = () => {
        const es = new EventSource(token ? `/events?token=${encodeURIComponent(token)}` : "/events");
        es.onopen = () => {
          if (everOpen) fn({ kind: "resync" });
          if (failures > 0) fn({ kind: "connected" });
          everOpen = true;
          failures = 0;
        };
        es.onmessage = (e) => fn(JSON.parse(e.data));
        es.onerror = async () => {
          es.close();
          const s = await session();
          if (s && !s.signed_in && (await enter())) return;
          // Signed out where a code signs in: the page asks for it; nothing to retry.
          if (s && !s.signed_in && s.pairing && !token) return fn({ kind: "signed_out" });
          if (failures >= tries()) {
            const retry = entry && !token ? () => location.assign(entry) : () => { failures = 0; open(); };
            fn({ kind: "disconnected", retry });
            return;
          }
          const delay = Math.min(30000, 1000 * 2 ** failures) * (0.5 + Math.random() / 2);
          failures += 1;
          fn({ kind: "reconnecting", attempt: failures, delay });
          setTimeout(open, delay);
        };
      };
      ready.then(open);
    },
  };
}
