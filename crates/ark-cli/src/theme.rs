//! Presentation settings a consumer can change: accent colour, prompt symbol, glyphs. Colour
//! is switched off when `NO_COLOR` is set or the output is not a terminal.

/// The accent colour, used for the prompt, activity bullets and headings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accent {
    Cyan,
    Magenta,
    Green,
    Yellow,
    Blue,
    Red,
    White,
    Rgb(u8, u8, u8),
}

impl Accent {
    fn sgr(&self) -> String {
        match self {
            Accent::Red => "31".into(),
            Accent::Green => "32".into(),
            Accent::Yellow => "33".into(),
            Accent::Blue => "34".into(),
            Accent::Magenta => "35".into(),
            Accent::Cyan => "36".into(),
            Accent::White => "37".into(),
            Accent::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        }
    }

    pub(crate) fn line_editor_color(&self) -> reedline::Color {
        match self {
            Accent::Red => reedline::Color::Red,
            Accent::Green => reedline::Color::Green,
            Accent::Yellow => reedline::Color::Yellow,
            Accent::Blue => reedline::Color::Blue,
            Accent::Magenta => reedline::Color::Magenta,
            Accent::Cyan => reedline::Color::Cyan,
            Accent::White => reedline::Color::White,
            Accent::Rgb(r, g, b) => reedline::Color::Rgb(*r, *g, *b),
        }
    }
}

/// The symbols the shell draws with.
#[derive(Clone, Debug)]
pub struct Glyphs {
    pub activity: String,
    pub result: String,
    pub ok: String,
    pub fail: String,
    pub warn: String,
    pub spinner: Vec<String>,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs {
            activity: "●".into(),
            result: "└".into(),
            ok: "✓".into(),
            fail: "✗".into(),
            warn: "!".into(),
            spinner: ["◐", "◓", "◑", "◒"].iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub accent: Accent,
    /// Drawn before the input line.
    pub prompt: String,
    pub glyphs: Glyphs,
    /// Print a one-line summary (tools, tokens, time) after each turn.
    pub turn_footer: bool,
    pub(crate) color: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: Accent::Cyan,
            prompt: "› ".into(),
            glyphs: Glyphs::default(),
            turn_footer: true,
            color: true,
        }
    }
}

impl Theme {
    pub fn accent(mut self, a: Accent) -> Self {
        self.accent = a;
        self
    }
    pub fn prompt(mut self, p: impl Into<String>) -> Self {
        self.prompt = p.into();
        self
    }
    pub fn glyphs(mut self, g: Glyphs) -> Self {
        self.glyphs = g;
        self
    }
    pub fn turn_footer(mut self, yes: bool) -> Self {
        self.turn_footer = yes;
        self
    }

    pub(crate) fn with_color(mut self, on: bool) -> Self {
        self.color = on;
        self
    }

    pub fn color_enabled(&self) -> bool {
        self.color
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn accented(&self, s: &str) -> String {
        self.paint(&self.accent.sgr(), s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn code(&self, s: &str) -> String {
        self.paint("36", s)
    }
    pub(crate) fn raw_accent_sgr(&self) -> String {
        self.accent.sgr()
    }
}

/// Whether colour should be used on this stream.
pub(crate) fn want_color(is_tty: bool) -> bool {
    is_tty && std::env::var_os("NO_COLOR").is_none()
}
