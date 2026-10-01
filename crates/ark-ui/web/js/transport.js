// One protocol, two carriers: the desktop window (Tauri IPC) or a local browser (HTTP + SSE).
// Views only see `call(method, params)` and `subscribe(fn)`.

export function connect() {
  const tauri = window.__TAURI__;
  if (tauri && tauri.core) {
    return {
      kind: "desktop",
      call: (method, params = {}) =>
        tauri.core.invoke("ark_call", { method, params }).catch((e) => {
          throw new Error(typeof e === "string" ? e : e?.message || String(e));
        }),
      subscribe(fn) {
        const ch = new tauri.core.Channel();
        ch.onmessage = fn;
        tauri.core.invoke("ark_subscribe", { channel: ch });
      },
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
  return {
    kind: "browser",
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
