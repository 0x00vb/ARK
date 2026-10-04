// What an agent sees of a page: its title, address, text, and the elements it can act on,
// each tagged with a number (data-ark-ref) that stays the same while the page lives.
(() => {
  const SEL = 'a[href], button, input:not([type=hidden]), select, textarea, summary, [role=button], [role=link], [role=checkbox], [role=radio], [role=tab], [role=menuitem], [role=option], [role=combobox], [role=textbox], [role=switch], [onclick], [contenteditable=""], [contenteditable=true]';
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const s = getComputedStyle(el);
    return s.visibility !== "hidden" && s.display !== "none" && s.opacity !== "0";
  };
  const clean = (s) => (s || "").replace(/\s+/g, " ").trim().slice(0, 80);
  const roleOf = (el) => {
    const r = el.getAttribute("role");
    if (r) return r;
    const t = el.tagName.toLowerCase();
    if (t === "a") return "link";
    if (t === "select") return "select";
    if (t === "textarea") return "textbox";
    if (t === "summary") return "button";
    if (t === "input") {
      const k = (el.getAttribute("type") || "text").toLowerCase();
      if (["button", "submit", "reset", "image"].includes(k)) return "button";
      if (["checkbox", "radio"].includes(k)) return k;
      return "textbox";
    }
    if (el.isContentEditable) return "textbox";
    return t === "button" ? "button" : "clickable";
  };
  const nameOf = (el) => {
    const label = el.id && document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
    return clean(
      el.getAttribute("aria-label") ||
      (label && label.innerText) ||
      (el.tagName === "INPUT" ? "" : el.innerText) ||
      el.getAttribute("placeholder") ||
      (["submit", "button", "reset"].includes((el.getAttribute("type") || "").toLowerCase()) ? el.value : "") ||
      el.getAttribute("title") ||
      el.getAttribute("alt") ||
      el.getAttribute("name") || ""
    );
  };
  window.__arkNext = window.__arkNext || 1;
  const elements = [];
  for (const el of document.querySelectorAll(SEL)) {
    if (elements.length >= 200) break;
    if (!visible(el) || el.disabled) continue;
    let ref = el.getAttribute("data-ark-ref");
    if (!ref) {
      ref = String(window.__arkNext++);
      el.setAttribute("data-ark-ref", ref);
    }
    const e = { ref: Number(ref), role: roleOf(el), name: nameOf(el) };
    if (el.tagName === "A") e.href = el.href;
    if (e.role === "textbox" || e.role === "select" || e.role === "combobox") {
      const v = el.isContentEditable ? el.innerText : el.value;
      e.value = (el.getAttribute("type") || "").toLowerCase() === "password" ? (v ? "••••" : "") : clean(v);
    }
    if (e.role === "checkbox" || e.role === "radio") e.checked = !!el.checked;
    elements.push(e);
  }
  const text = (document.body ? document.body.innerText : "").replace(/\n{3,}/g, "\n\n").slice(0, 50000);
  return { url: location.href, title: document.title, text, elements };
})()
