const params = new URLSearchParams(location.search);
const kind = params.get("kind") === "target" ? "target" : "control";
document.body.classList.add(kind);
const line = document.querySelector("#line");
const check = document.querySelector("#check");
const stop = document.querySelector("#stop");

const invoke = window.__TAURI__?.core?.invoke;
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
const reportMotion = () => invoke?.("ark_computer_motion", { reduce: reducedMotion.matches });
reportMotion();
reducedMotion.addEventListener?.("change", reportMotion);
stop?.addEventListener("click", () => invoke?.("ark_computer_stop"));
check?.addEventListener("click", () => invoke?.("ark_computer_check"));

window.__TAURI__?.event?.listen("computer-overlay", ({ payload }) => {
  document.documentElement.style.setProperty("--accent", payload.accent || "#335eb7");
  if (line) line.textContent = payload.line || "";
  document.body.classList.toggle("hazard", ["hazardous", "submitted_unconfirmed"].includes(payload.phase));
  if (kind === "target") {
    const target = document.querySelector("#target");
    target.style.animation = "none";
    requestAnimationFrame(() => { target.style.animation = ""; });
  }
});
