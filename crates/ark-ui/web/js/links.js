// Web links open in the person's browser. The window opens no windows of its own, so a plain
// `target="_blank"` click or `window.open` there would do nothing: everything goes through
// `openOutside`, which the app points at the window's opener (`setOpener`).

let opener = (url) => window.open(url, "_blank", "noopener");

/// Where `openOutside` sends a web address. The app sets it once, from its transport.
export function setOpener(fn) {
  opener = fn;
}

/// Opens a web address in the person's browser.
export function openOutside(url) {
  opener(url);
}

/// Sends clicks on http(s) links of another origin to `open(url)`. Links inside text the person
/// is editing are left to the editor.
export function openLinksOutside(doc, open = openOutside) {
  doc.addEventListener("click", (e) => {
    if (e.defaultPrevented || e.button !== 0) return;
    const a = e.target.closest?.("a[href]");
    if (!a || a.closest("[contenteditable=true], [contenteditable='']")) return;
    let url;
    try {
      url = new URL(a.getAttribute("href"), doc.baseURI);
    } catch {
      return;
    }
    if (!/^https?:$/.test(url.protocol) || url.origin === doc.location?.origin) return;
    e.preventDefault();
    open(url.href);
  });
}
