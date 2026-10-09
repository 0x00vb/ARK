#[test]
fn appearance_offers_modes_palettes_and_a_window_style() {
    let settings = include_str!("../web/js/settings.js");
    let theme = include_str!("../web/js/theme.js");

    for choice in [
        "System", "Light", "Dark", "Monokai", "Dracula", "GitHub", "Xcode",
    ] {
        assert!(
            settings.contains(choice),
            "missing appearance choice {choice}"
        );
    }
    assert!(settings.contains("Window style"));
    assert!(settings.contains("Translucent"));
    assert!(theme.contains("ark-window-style"));
    assert!(theme.contains("root.dataset.material = materialChoice()"));
}

#[test]
fn appearance_uses_real_previews_instead_of_detached_swatches() {
    let settings = include_str!("../web/js/settings.js");
    let css = include_str!("../web/app.css");

    assert!(settings.contains("appearance-preview-rail"));
    assert!(settings.contains("appearance-preview-page"));
    assert!(css.contains(".appearance-mode-grid"));
    assert!(css.contains(".appearance-palette-grid"));
    assert!(!settings.contains("theme-swatch"));
}
