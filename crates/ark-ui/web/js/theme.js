// The window's palette, set on `<html data-theme>` before the first paint. The
// person's choice (Settings › Appearance) wins; else the app's default (`ArkUi::theme`, put in
// `data-theme-default` by the host); "system" follows the operating system. A classic script,
// not a module, so it runs before the page is drawn. Pages that draw their own colours listen
// for the `ark:theme` event on `window`.

(() => {
  const root = document.documentElement;
  const KEY = "ark-theme";
  const MODE_KEY = "ark-theme-mode";
  const LIGHT_KEY = "ark-theme-light";
  const DARK_KEY = "ark-theme-dark";
  const MATERIAL_KEY = "ark-window-style";
  const CHOICES = ["system", "light", "dark", "monokai", "dracula", "github", "xcode"];
  const MATERIALS = ["solid", "translucent"];
  const DARK = new Set(["dark", "monokai", "dracula"]);
  const dark = matchMedia("(prefers-color-scheme: dark)");
  const reducedTransparency = matchMedia("(prefers-reduced-transparency: reduce)");

  const read = (key) => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  };
  const old = () => read(KEY);
  const oldMode = () => old() === "system" ? "system" : DARK.has(old()) ? "dark" : old() ? "light" : null;
  const mode = () => [read(MODE_KEY), oldMode(), root.dataset.themeDefault].find((c) => ["system", "light", "dark"].includes(c)) || "system";
  const scheme = () => mode() === "system" ? (dark.matches ? "dark" : "light") : mode();
  const palette = (kind = scheme()) => {
    const choices = kind === "dark" ? ["dark", "monokai", "dracula"] : ["light", "github", "xcode"];
    const legacy = old();
    return [read(kind === "dark" ? DARK_KEY : LIGHT_KEY), legacy, kind].find((c) => choices.includes(c)) || kind;
  };
  const choice = () => mode() === "system" && palette() === scheme() ? "system" : palette();
  const materialChoice = () => {
    try {
      const value = localStorage.getItem(MATERIAL_KEY);
      return MATERIALS.includes(value) ? value : "translucent";
    } catch {
      return "translucent";
    }
  };

  function apply(force = false) {
    const selectedMode = mode();
    const selectedScheme = scheme();
    const theme = palette(selectedScheme);
    const changed = root.dataset.theme !== theme || root.dataset.colorScheme !== selectedScheme || root.dataset.themeChoice !== selectedMode;
    root.dataset.theme = theme;
    root.dataset.colorScheme = selectedScheme;
    root.dataset.themeChoice = selectedMode;
    root.dataset.material = materialChoice();
    root.dataset.reducedTransparency = reducedTransparency.matches ? "true" : "false";
    if (!changed && !force) return;
    dispatchEvent(new CustomEvent("ark:theme", { detail: theme }));
  }

  dark.addEventListener("change", apply);
  reducedTransparency.addEventListener("change", apply);
  window.arkTheme = {
    choice,
    mode,
    palette,
    palettes: () => scheme() === "dark" ? ["dark", "monokai", "dracula"] : ["light", "github", "xcode"],
    setMode(value) {
      if (!["system", "light", "dark"].includes(value)) return;
      try { localStorage.setItem(MODE_KEY, value); } catch {}
      apply(true);
    },
    setPalette(value) {
      const kind = DARK.has(value) ? "dark" : ["light", "github", "xcode"].includes(value) ? "light" : null;
      if (!kind) return;
      try { localStorage.setItem(kind === "dark" ? DARK_KEY : LIGHT_KEY, value); } catch {}
      apply(true);
    },
    set(c) {
      if (!CHOICES.includes(c)) return;
      try {
        localStorage.setItem(KEY, c);
        if (["system", "light", "dark"].includes(c)) localStorage.setItem(MODE_KEY, c);
        else {
          const kind = DARK.has(c) ? "dark" : "light";
          localStorage.setItem(MODE_KEY, kind);
          localStorage.setItem(kind === "dark" ? DARK_KEY : LIGHT_KEY, c);
        }
      } catch {}
      apply(true);
    },
  };
  window.arkMaterial = {
    choice: materialChoice,
    set(value) {
      if (!MATERIALS.includes(value)) return;
      try {
        localStorage.setItem(MATERIAL_KEY, value);
      } catch {}
      apply(true);
    },
  };
  apply();
})();
