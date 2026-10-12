// Development assets come from disk. Keep the browser on the saved source without adding a
// watcher, WebSocket or production code path to an application built on ARK.

const revisionUrl = new URL("__ark_dev_revision", document.baseURI);
let revision = null;

async function check() {
  try {
    const response = await fetch(revisionUrl, { cache: "no-store" });
    if (response.ok) {
      const next = await response.text();
      if (revision !== null && next !== revision) {
        location.reload();
        return;
      }
      revision = next;
    }
  } catch {
    // A Rust rebuild may stop the server briefly. The loaded page keeps trying until it returns.
  }
  setTimeout(check, document.hidden ? 1000 : 300);
}

void check();
