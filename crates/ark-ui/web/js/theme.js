// The window's theme, set on `<html data-theme="light|dark">` before the first paint. The
// person's choice (Settings › Appearance) wins; else the app's default (`ArkUi::theme`, put in
// `data-theme-default` by the host); "system" follows the operating system. A classic script,
// not a module, so it runs before the page is drawn. Pages that draw their own colours listen
// for the `ark:theme` event on `window`.

(() => {
  const root = document.documentElement;
  const KEY = "ark-theme";
  const CHOICES = ["light", "dark", "system"];
  const dark = matchMedia("(prefers-color-scheme: dark)");

  const saved = () => {
    try {
      return localStorage.getItem(KEY);
    } catch {
      return null;
    }
  };
  const choice = () => [saved(), root.dataset.themeDefault].find((c) => CHOICES.includes(c)) || "system";

  function apply() {
    const c = choice();
    const theme = c === "system" ? (dark.matches ? "dark" : "light") : c;
    if (root.dataset.theme === theme) return;
    root.dataset.theme = theme;
    dispatchEvent(new CustomEvent("ark:theme", { detail: theme }));
  }

  dark.addEventListener("change", apply);
  window.arkTheme = {
    choice,
    set(c) {
      try {
        localStorage.setItem(KEY, c);
      } catch {}
      apply();
    },
  };
  apply();
})();
