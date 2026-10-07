// Shortcuts and their labels from one place: ⌘ on macOS, Ctrl on Windows and Linux. Labels are
// written the Mac way (⌘K, ⌘↩, ⌃`) and shown the local way (Ctrl+K, Ctrl+Enter, Ctrl+`). The
// system is the one the person sits at, so a window on a runtime elsewhere still reads right.

const platform = (navigator.userAgentData?.platform || navigator.platform || "").toLowerCase();

/// `macos`, `windows` or `linux`. The page sets it as `data-os` on the app.
export const os = platform.includes("mac") ? "macos" : platform.includes("win") ? "windows" : "linux";

/// Whether the platform's command key is down: ⌘ on macOS, Ctrl elsewhere.
export function mod(e) {
  return os === "macos" ? e.metaKey : e.ctrlKey;
}

/// A shortcut label as this system writes it.
export function label(text) {
  if (os === "macos" || !text) return text;
  return text
    .replace(/[⌘⌃]/g, "Ctrl+")
    .replace(/⇧/g, "Shift+")
    .replace(/⌥/g, "Alt+")
    .replace(/↩/g, "Enter");
}

/// Rewrite the shortcut labels already in `root` (titles and <kbd>) for this system.
export function relabel(root) {
  if (os === "macos") return;
  for (const el of root.querySelectorAll("[title]")) el.title = label(el.title);
  for (const el of root.querySelectorAll("[placeholder]")) el.placeholder = label(el.placeholder);
  for (const el of root.querySelectorAll("kbd")) el.textContent = label(el.textContent);
}
