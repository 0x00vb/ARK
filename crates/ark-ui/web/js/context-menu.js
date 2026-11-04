import { h, icon } from "./dom.js";

let current;
export function contextMenu(event, anchor, actions, { handled = false } = {}) {
  if (event.defaultPrevented && !handled || event.target.closest("input,textarea,[contenteditable],a,.xterm")) return;
  if (String(window.getSelection() || "") && !event.target.closest("button,[role=treeitem],.auto-node,.auto-gallery-item")) return;
  const items = actions.filter(Boolean);
  if (!items.length) return;
  event.preventDefault(); event.stopPropagation(); current?.(); document.querySelector(".popover")?.close?.();
  const focused = document.activeElement;
  const scrollPositions = new Map();
  for (let node = anchor; node; node = node.parentElement) scrollPositions.set(node, { top: node.scrollTop, left: node.scrollLeft });
  const menu = h("div.popover.context-menu.open", { role: "menu", tabindex: "-1", "aria-label": "Actions" });
  let closed = false, origin = null;
  const clamp = () => { if (!origin) return; menu.style.left = `${Math.max(8, Math.min(origin.x, innerWidth - menu.offsetWidth - 8))}px`; menu.style.top = `${Math.max(8, Math.min(origin.y, innerHeight - menu.offsetHeight - 8))}px`; };
  const observer = new MutationObserver(() => { if (!anchor.isConnected) close(); });
  const close = (restore = false) => {
    if (closed) return;
    closed = true; observer.disconnect(); menu.remove();
    document.removeEventListener("pointerdown", outside, true);
    document.removeEventListener("keydown", keys, true);
    document.removeEventListener("scroll", scroll, true);
    window.removeEventListener("resize", resize);
    window.removeEventListener("blur", resize);
    if (current === close) current = null;
    if (restore) {
      const target = anchor.matches("button,a,input,textarea,select,[tabindex]") && anchor.isConnected ? anchor : focused?.isConnected ? focused : null;
      target?.focus({ preventScroll: true });
    }
  };
  const enabled = () => [...menu.querySelectorAll("button:not(:disabled)")];
  const stack = [];
  const keys = e => {
    if (e.key === "ArrowLeft" && stack.length) { e.preventDefault(); render(stack.pop()); return; }
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); return; }
    if (e.key === "Tab") { close(true); return; }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    e.preventDefault(); e.stopPropagation();
    const buttons = enabled(), index = buttons.indexOf(document.activeElement);
    const next = e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : (index + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next]?.focus();
  };
  const outside = e => { if (!menu.contains(e.target)) close(); };
  const scroll = e => {
    if (menu.contains(e.target)) return;
    const target = e.target === document ? document.scrollingElement : e.target;
    const before = scrollPositions.get(target);
    if (!before || before.top !== target.scrollTop || before.left !== target.scrollLeft) close();
  };
  const resize = () => close();
  function render(actions) {
    menu.replaceChildren();
    if (stack.length) menu.append(h("button.menu-item", {type: "button", role: "menuitem", onclick: () => render(stack.pop())}, icon("back"), "Back"));
    for (const action of actions) {
      const control = h("button.menu-item", { type: "button", role: "menuitem", disabled: action.disabled,
        ...(action.items ? {"aria-haspopup": "menu"} : {}), onclick: () => {
          if (action.disabled) return;
          if (action.items) { stack.push(actions); render(typeof action.items === "function" ? action.items() : action.items); return; }
          close(true); action.run();
        } }, action.icon ? icon(action.icon) : null, h("span", action.label), action.items ? icon("chevron-right") : null);
      control.addEventListener("keydown", event => { if (action.items && event.key === "ArrowRight") { event.preventDefault(); control.click(); } });
      menu.append(control);
    }
    clamp(); (enabled()[0] || menu).focus({preventScroll: true});
  }
  render(items);
  document.body.append(menu);
  const rect = anchor.getBoundingClientRect();
  const keyboard = event.type === "keydown" || event.clientX === 0 && event.clientY === 0;
  const x = keyboard ? rect.left : event.clientX, y = keyboard ? rect.bottom : event.clientY;
  origin = {x, y}; clamp();
  document.addEventListener("pointerdown", outside, true);
  document.addEventListener("keydown", keys, true);
  document.addEventListener("scroll", scroll, true);
  window.addEventListener("resize", resize);
  window.addEventListener("blur", resize);
  observer.observe(document.body, { childList: true, subtree: true });
  current = close; (enabled()[0] || menu).focus({ preventScroll: true });
  return close;
}

export function bindContextMenu(element, actions) {
  let close;
  const open = event => { close = contextMenu(event, element.matches("button,[tabindex]") ? element : element.querySelector("button,[tabindex]") || element, actions()); };
  const keys = event => {
    if (event.key === "ContextMenu" || event.shiftKey && event.key === "F10") open(event);
  };
  element.addEventListener("contextmenu", open);
  element.addEventListener("keydown", keys);
  return () => { close?.(); element.removeEventListener("contextmenu", open); element.removeEventListener("keydown", keys); };
}

export function controlActions(controls) {
  return [...controls].map(control => ({ label: control.getAttribute("aria-label") || control.dataset.action || control.textContent.trim(), disabled: control.matches(":disabled") || control.getAttribute("aria-disabled") === "true", run: () => control.click() }));
}
