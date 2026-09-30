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
    subscribe(fn) {
      let dropped = false;
      const es = new EventSource(`/events?token=${encodeURIComponent(token || "")}`);
      es.onmessage = (e) => fn(JSON.parse(e.data));
      es.onerror = () => { dropped = true; };
      es.onopen = () => {
        if (dropped) fn({ kind: "resync" });
        dropped = false;
      };
    },
  };
}
