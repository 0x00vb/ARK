// The window's palette, set on `<html data-theme>` before the first paint. The
// person's choice (Settings › Appearance) wins; else the app's default (`ArkUi::theme`, put in
// `data-theme-default` by the host); "system" follows the operating system. A classic script,
// not a module, so it runs before the page is drawn. Pages that draw their own colours listen
// for the `ark:theme` event on `window`.

(() => {
  const root = document.documentElement;
  const KEY = "ark-theme";
  const CHOICES = ["system", "light", "dark", "monokai", "dracula", "github", "xcode"];
  const DARK = new Set(["dark", "monokai", "dracula"]);
  const dark = matchMedia("(prefers-color-scheme: dark)");

  const saved = () => {
    try {
      return localStorage.getItem(KEY);
    } catch {
      return null;
    }
  };
  const choice = () => [saved(), root.dataset.themeDefault].find((c) => CHOICES.includes(c)) || "system";

  function apply(force = false) {
    const c = choice();
    const scheme = c === "system" ? (dark.matches ? "dark" : "light") : DARK.has(c) ? "dark" : "light";
    const theme = c === "system" ? scheme : c;
    const changed = root.dataset.theme !== theme || root.dataset.colorScheme !== scheme || root.dataset.themeChoice !== c;
    root.dataset.theme = theme;
    root.dataset.colorScheme = scheme;
    root.dataset.themeChoice = c;
    if (!changed && !force) return;
    dispatchEvent(new CustomEvent("ark:theme", { detail: theme }));
  }

  dark.addEventListener("change", apply);
  window.arkTheme = {
    choice,
    set(c) {
      if (!CHOICES.includes(c)) return;
      try {
        localStorage.setItem(KEY, c);
      } catch {}
      apply(true);
    },
  };
  apply();
})();
