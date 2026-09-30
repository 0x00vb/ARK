//! A small streaming Markdown styler. Model text arrives in arbitrary chunks, so this is a
//! character state machine: it styles headings, `**bold**`, `inline code`, fenced blocks and
//! bullets as the text streams, holding back only the few characters needed to decide what a
//! marker means. Without colour it passes text through untouched.

#[derive(Default)]
pub struct MdStream {
    color: bool,
    accent: String,
    line_start: bool,
    head: String,
    heading: bool,
    bold: bool,
    code: bool,
    fence: bool,
    fence_line: bool,
    star: bool,
    /// The style currently applied, as an SGR parameter list ("" = plain).
    applied: String,
}

impl MdStream {
    pub fn new(color: bool, accent_sgr: String) -> Self {
        MdStream {
            color,
            accent: accent_sgr,
            line_start: true,
            ..Default::default()
        }
    }

    /// Style the next chunk. The output may lag the input by a few characters.
    pub fn push(&mut self, input: &str) -> String {
        if !self.color {
            return input.to_string();
        }
        let mut out = String::new();
        for c in input.chars() {
            self.feed(c, &mut out);
        }
        out
    }

    /// Flush anything held back and reset styling.
    pub fn finish(&mut self) -> String {
        if !self.color {
            return String::new();
        }
        let mut out = String::new();
        if !self.head.is_empty() {
            let head = std::mem::take(&mut self.head);
            self.line_start = false;
            for c in head.chars() {
                self.inline(c, &mut out);
            }
        }
        if self.star {
            self.star = false;
            out.push('*');
        }
        self.heading = false;
        self.bold = false;
        self.code = false;
        self.fence_line = false;
        self.restyle(&mut out);
        self.line_start = true;
        out
    }

    fn feed(&mut self, c: char, out: &mut String) {
        if c == '\n' {
            if !self.head.is_empty() {
                let head = std::mem::take(&mut self.head);
                self.line_start = false;
                for h in head.chars() {
                    self.inline(h, out);
                }
            }
            if self.star {
                self.star = false;
                out.push('*');
            }
            self.heading = false;
            self.bold = false;
            self.code = false;
            self.fence_line = false;
            self.restyle(out);
            out.push('\n');
            self.line_start = true;
            return;
        }
        if self.line_start {
            self.head.push(c);
            self.decide_head(out);
            return;
        }
        self.inline(c, out);
    }

    /// Decide what the buffered start of a line is, or keep waiting.
    fn decide_head(&mut self, out: &mut String) {
        let h = self.head.clone();
        let trimmed = h.trim_start_matches(' ');
        let indent = &h[..h.len() - trimmed.len()];
        // Still ambiguous: keep buffering.
        let waiting = trimmed.is_empty()
            || (!trimmed.is_empty()
                && trimmed.chars().all(|c| c == '#')
                && trimmed.len() <= 6
                && indent.is_empty())
            || trimmed == "`"
            || trimmed == "``"
            || trimmed == "-"
            || trimmed == "*";
        if waiting && h.len() < 8 {
            return;
        }
        self.head.clear();
        self.line_start = false;
        if indent.is_empty()
            && trimmed.starts_with('#')
            && trimmed.trim_start_matches('#').starts_with(' ')
            && !self.fence
        {
            self.heading = true;
            self.restyle(out);
            return;
        }
        if trimmed == "```" || (trimmed.starts_with("```") && !self.fence) {
            self.fence = !self.fence;
            self.fence_line = true;
            self.restyle(out);
            out.push_str(indent);
            out.push_str(trimmed);
            return;
        }
        if !self.fence && (trimmed.starts_with("- ") || trimmed.starts_with("* ")) {
            out.push_str(indent);
            out.push_str("• ");
            for c in trimmed[2..].chars() {
                self.inline(c, out);
            }
            return;
        }
        for c in h.chars() {
            self.inline(c, out);
        }
    }

    fn inline(&mut self, c: char, out: &mut String) {
        if self.fence || self.fence_line {
            out.push(c);
            return;
        }
        if self.star {
            self.star = false;
            if c == '*' {
                self.bold = !self.bold;
                self.restyle(out);
                return;
            }
            out.push('*');
        }
        match c {
            '*' => self.star = true,
            '`' => {
                self.code = !self.code;
                self.restyle(out);
            }
            _ => out.push(c),
        }
    }

    fn restyle(&mut self, out: &mut String) {
        let mut codes: Vec<String> = Vec::new();
        if self.heading {
            codes.push("1".into());
            codes.push(self.accent.clone());
        }
        if self.bold {
            codes.push("1".into());
        }
        if self.code {
            codes.push("36".into());
        }
        if self.fence || self.fence_line {
            codes.push("2".into());
        }
        let want = codes.join(";");
        if want != self.applied {
            out.push_str("\x1b[0m");
            if !want.is_empty() {
                out.push_str(&format!("\x1b[{want}m"));
            }
            self.applied = want;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if c == '\x1b' {
                esc = true;
                continue;
            }
            if esc {
                if c == 'm' {
                    esc = false;
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    fn render_chunked(text: &str, chunk: usize) -> String {
        let mut md = MdStream::new(true, "36".into());
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        for part in chars.chunks(chunk) {
            out.push_str(&md.push(&part.iter().collect::<String>()));
        }
        out.push_str(&md.finish());
        out
    }

    #[test]
    fn markers_are_consumed_regardless_of_chunking() {
        let text = "# Title\nSome **bold** and `code`.\n- item one\n* item two\nplain * star\n";
        let expected = "Title\nSome bold and code.\n• item one\n• item two\nplain * star\n";
        for chunk in [1, 2, 3, 7, 100] {
            assert_eq!(
                strip(&render_chunked(text, chunk)),
                expected,
                "chunk size {chunk}"
            );
        }
    }

    #[test]
    fn bold_is_styled_and_reset() {
        let out = render_chunked("a **b** c", 1);
        assert!(out.contains("\x1b[1mb\x1b[0m"), "{out:?}");
    }

    #[test]
    fn fenced_code_is_dim_and_kept_verbatim() {
        let out = render_chunked("```rust\nlet **x** = 1;\n```\nafter\n", 2);
        assert_eq!(strip(&out), "```rust\nlet **x** = 1;\n```\nafter\n");
        assert!(out.contains("\x1b[2m"));
    }

    #[test]
    fn no_color_is_passthrough() {
        let mut md = MdStream::new(false, String::new());
        assert_eq!(md.push("# **x**"), "# **x**");
        assert_eq!(md.finish(), "");
    }

    #[test]
    fn unfinished_head_is_flushed() {
        assert_eq!(strip(&render_chunked("##", 1)), "##");
        assert_eq!(strip(&render_chunked("-", 1)), "-");
    }
}
