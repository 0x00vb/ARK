import { h } from "./dom.js";

const official = new Map([
  ["capability:mail", "gmail"], ["gmail", "gmail"], ["mail.gmail", "gmail"], ["google-mail", "gmail"],
  ["capability:calendar", "calendar"], ["googlecalendar", "calendar"], ["google-calendar", "calendar"],
]);

export function pluginArtwork(entry) {
  const brand = official.get(entry.id);
  const source = entry.source === "app" ? entry.icon : brand ? new URL(`../plugins/${brand}.png`, import.meta.url).href : null;
  let seed = 0;
  for (const char of entry.id) seed = (seed * 31 + char.codePointAt(0)) >>> 0;
  const fallback = new URL(`../plugins/module-${seed % 3}.svg`, import.meta.url).href;
  const image = h("img", { src: source || fallback, alt: "", width: 32, height: 32, decoding: "async" });
  image.addEventListener("error", () => { if (image.src !== fallback) image.src = fallback; });
  return h("span.market-icon", { "aria-hidden": "true", "data-id": entry.id, "data-artwork": source ? "brand" : "original" }, image);
}
