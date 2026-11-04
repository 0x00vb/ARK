import { h, icon } from "./dom.js";
import { bindContextMenu, controlActions } from "./context-menu.js";

const scenes = new Map();
const point = value => value && Number.isFinite(value.x) && Number.isFinite(value.y) && value.x >= 0 && value.y >= 0 && value.x <= 10000 && value.y <= 10000;
export function canvasScene(key, report) {
  if (scenes.has(key)) { const scene = scenes.get(key); scene.report = report; report(scene.error); return scene; }
  let data = { version: 1, positions: {}, marks: [] }, failure = "";
  try {
    const stored = JSON.parse(localStorage.getItem(`ark.canvas:${key}`) || "null");
    if (stored) {
      if (stored.version !== 1 || !stored.positions || !Array.isArray(stored.marks)) throw Error("Unsupported canvas data");
      data.positions = Object.fromEntries(Object.entries(stored.positions).filter(([, value]) => point(value)));
      data.marks = stored.marks.filter(mark => ["draw", "shape"].includes(mark.kind) && Array.isArray(mark.points) && mark.points.length <= 2000 && mark.points.every(point)).slice(-200);
    }
  } catch (error) { failure = `Canvas layout could not be loaded: ${error.message}`; }
  const scene = { data, report, error: failure, save() {
    try { localStorage.setItem(`ark.canvas:${key}`, JSON.stringify(data)); this.error = ""; }
    catch (error) { this.error = `Canvas layout could not be saved on this device: ${error.message}`; }
    this.report(this.error);
  } };
  scenes.set(key, scene); scene.report(scene.error); return scene;
}

export function canvasTools({ viewport, plane, scene, toolbar, scale = () => 1, create }) {
  let mode = "select", gesture = null;
  const tools = h("div.canvas-mode-tools", { role: "group", "aria-label": "Canvas tools" });
  const modes = [["select", "Select", "cursor"], ["pan", "Pan", "hand"], ["draw", "Draw", "pencil"], ["shape", "Rectangle", "square"]];
  function choose(next) {
    mode = next; viewport.dataset.tool = mode;
    for (const control of tools.querySelectorAll("[data-tool]")) control.setAttribute("aria-pressed", String(control.dataset.tool === mode));
  }
  for (const [value, name, glyph] of modes) tools.append(h("button.auto-icon", { type: "button", title: name, "aria-label": name, "aria-pressed": String(value === mode), dataset: { tool: value }, onclick: () => choose(value) }, icon(glyph)));
  toolbar.prepend(tools);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.classList.add("canvas-marks"); svg.setAttribute("aria-hidden", "true"); plane.prepend(svg);
  function paint(preview) {
    svg.replaceChildren();
    for (const mark of [...scene.data.marks, ...(preview ? [preview] : [])]) {
      if (!mark.points.length) continue;
      const shape = document.createElementNS(svg.namespaceURI, "path");
      const first = mark.points[0], last = mark.points.at(-1);
      shape.setAttribute("d", mark.kind === "shape" ? `M${first.x},${first.y} H${last.x} V${last.y} H${first.x} Z` : mark.points.map((p, i) => `${i ? "L" : "M"}${p.x},${p.y}`).join(" "));
      svg.append(shape);
    }
    undo.disabled = !scene.data.marks.length;
  }
  const undo = h("button.auto-icon", { type: "button", title: "Undo drawing", "aria-label": "Undo drawing", onclick: () => { scene.data.marks.pop(); scene.save(); paint(); } }, icon("undo"));
  tools.append(undo); paint();
  const position = event => {
    const bounds = plane.getBoundingClientRect();
    return { x: Math.max(0, Math.min(10000, (event.clientX - bounds.left) / scale())), y: Math.max(0, Math.min(10000, (event.clientY - bounds.top) / scale())) };
  };
  const empty = target => !target.closest("button,input,textarea,select,summary,.auto-node,.auto-gallery-item");
  viewport.addEventListener("pointerdown", event => {
    if (event.button !== 0 || !event.isPrimary || !empty(event.target) || mode === "select") return;
    gesture = { id: event.pointerId, client: { x: event.clientX, y: event.clientY }, pan: { x: viewport.scrollLeft, y: viewport.scrollTop }, mark: { kind: mode, points: [position(event)] }, moved: false };
    viewport.setPointerCapture(event.pointerId); event.preventDefault();
  });
  viewport.addEventListener("pointermove", event => {
    if (!gesture || gesture.id !== event.pointerId) return;
    const dx = event.clientX - gesture.client.x, dy = event.clientY - gesture.client.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) gesture.moved = true;
    if (mode === "pan") viewport.scrollTo(gesture.pan.x - dx, gesture.pan.y - dy);
    else {
      const next = position(event);
      if (mode === "shape") gesture.mark.points[1] = next;
      else if (gesture.mark.points.length < 2000) gesture.mark.points.push(next);
      paint(gesture.mark);
    }
  });
  function finish(event, cancelled) {
    if (!gesture || gesture.id !== event.pointerId) return;
    if (!cancelled && gesture.moved && mode !== "pan") {
      scene.data.marks.push(gesture.mark); scene.data.marks = scene.data.marks.slice(-200); scene.save();
    }
    gesture = null; paint();
    if (viewport.hasPointerCapture(event.pointerId)) viewport.releasePointerCapture(event.pointerId);
  }
  viewport.addEventListener("pointerup", event => finish(event, false));
  viewport.addEventListener("pointercancel", event => finish(event, true));
  viewport.addEventListener("keydown", event => {
    if (event.target !== viewport) return;
    if (event.key === "Escape") { choose("select"); gesture = null; paint(); }
    if (event.key === "Enter") { event.preventDefault(); create?.({ x: (viewport.scrollLeft + 80) / scale(), y: (viewport.scrollTop + 80) / scale() }); }
  });
  bindContextMenu(viewport, () => controlActions(toolbar.querySelectorAll("button")));
  choose("select");
  return { paint, choose };
}
