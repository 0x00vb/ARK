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
    };
  }
  const url = new URL(location.href);
  let token = url.searchParams.get("token");
  try {
    if (token) sessionStorage.setItem("ark-token", token);
    else token = sessionStorage.getItem("ark-token");
  } catch {}
  if (url.searchParams.has("token")) {
    url.searchParams.delete("token");
    history.replaceState(null, "", url.pathname + url.search);
  }
  // In the window, on a runtime it visits: the window's title bar and a way back. In a
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
    async call(method, params = {}) {
      const r = await fetch("/rpc", {
        method: "POST",
        headers: { "content-type": "application/json", "x-ark-token": token || "" },
        body: JSON.stringify({ method, params }),
      });
      if (r.status === 401) throw new Error("This page needs the link printed at start (with its token).");
      const j = await r.json();
      if (!j.ok) throw new Error(j.error);
      return j.result;
    },
    // The browser's EventSource retries every few seconds forever. Instead: close it on an
    // error and reconnect with exponential backoff (1 s doubling to 30 s, with jitter); after
    // RECONNECT_TRIES failures, stop and report `disconnected` with a `retry` function. A
    // reconnect reports `resync`: the page reloads its state and replays the running turn.
    subscribe(fn) {
      const RECONNECT_TRIES = 10;
      let failures = 0;
      let everOpen = false;
      const open = () => {
        const es = new EventSource(`/events?token=${encodeURIComponent(token || "")}`);
        es.onopen = () => {
          if (everOpen) fn({ kind: "resync" });
          if (failures > 0) fn({ kind: "connected" });
          everOpen = true;
          failures = 0;
        };
        es.onmessage = (e) => fn(JSON.parse(e.data));
        es.onerror = () => {
          es.close();
          if (failures >= RECONNECT_TRIES) {
            fn({ kind: "disconnected", retry: () => { failures = 0; open(); } });
            return;
          }
          const delay = Math.min(30000, 1000 * 2 ** failures) * (0.5 + Math.random() / 2);
          failures += 1;
          fn({ kind: "reconnecting", attempt: failures, delay });
          setTimeout(open, delay);
        };
      };
      open();
    },
  };
}
