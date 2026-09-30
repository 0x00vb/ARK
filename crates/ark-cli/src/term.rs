//! Terminal output. Everything the shell prints goes through [`Out`], which knows three
//! things the rest of the code should not have to: whether the terminal is in raw mode (so a
//! newline needs a carriage return), whether the cursor sits mid-line (streamed text), and
//! whether the transient status line is on screen and must be erased first.

use std::io::Write;

pub struct Out {
    w: Box<dyn Write + Send + Sync>,
    raw: bool,
    /// The cursor is not at the start of a line (streamed text is mid-line).
    mid_line: bool,
    /// A status line is drawn on the current line.
    status: bool,
    width: usize,
}

impl Out {
    pub fn stdout() -> Self {
        let width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(100);
        Out {
            w: Box::new(std::io::stdout()),
            raw: false,
            mid_line: false,
            status: false,
            width,
        }
    }

    /// An in-memory sink (tests).
    pub fn sink(w: Box<dyn Write + Send + Sync>) -> Self {
        Out {
            w,
            raw: false,
            mid_line: false,
            status: false,
            width: 100,
        }
    }

    pub(crate) fn set_raw(&mut self, raw: bool) {
        self.raw = raw;
    }

    pub fn width(&self) -> usize {
        self.width.max(20)
    }

    pub(crate) fn refresh_width(&mut self) {
        if let Ok((w, _)) = crossterm::terminal::size() {
            self.width = w as usize;
        }
    }

    pub fn is_mid_line(&self) -> bool {
        self.mid_line
    }

    fn put(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.raw {
            let _ = self.w.write_all(s.replace('\n', "\r\n").as_bytes());
        } else {
            let _ = self.w.write_all(s.as_bytes());
        }
        // Colour codes end in 'm'; only a real trailing newline puts us at column 0.
        self.mid_line = !s.ends_with('\n');
    }

    /// Write text as-is (streaming). Erases the status line first.
    pub fn text(&mut self, s: &str) {
        self.clear_status();
        self.put(s);
        let _ = self.w.flush();
    }

    /// Write one full line, starting on a fresh line.
    pub fn line(&mut self, s: &str) {
        self.clear_status();
        if self.mid_line {
            self.put("\n");
        }
        self.put(s);
        self.put("\n");
        let _ = self.w.flush();
    }

    /// Finish any partial line.
    pub fn end_line(&mut self) {
        self.clear_status();
        if self.mid_line {
            self.put("\n");
            let _ = self.w.flush();
        }
    }

    /// A blank separator line (never two in a row at the start of a block).
    pub fn blank(&mut self) {
        self.end_line();
        self.put("\n");
        let _ = self.w.flush();
    }

    /// Draw the transient status line. Skipped while streamed text is mid-line.
    pub fn status(&mut self, s: &str) {
        if self.mid_line && !self.status {
            return;
        }
        let clipped: String = s.chars().take(self.width().saturating_sub(1)).collect();
        let _ = self.w.write_all(b"\r\x1b[2K");
        let _ = self.w.write_all(clipped.as_bytes());
        let _ = self.w.flush();
        self.status = true;
        self.mid_line = true;
    }

    pub fn clear_status(&mut self) {
        if self.status {
            let _ = self.w.write_all(b"\r\x1b[2K");
            self.status = false;
            self.mid_line = false;
        }
    }

    /// Move up `n` lines and clear to the end of the screen (redraw a menu in place).
    pub(crate) fn rewind(&mut self, n: usize) {
        if n > 0 {
            let _ = write!(self.w, "\r\x1b[{n}A\x1b[J");
        } else {
            let _ = self.w.write_all(b"\r\x1b[J");
        }
        self.mid_line = false;
        self.status = false;
        let _ = self.w.flush();
    }
}

/// Raw mode for the duration of a turn, restored on drop (including on panic unwind).
pub(crate) struct RawMode {
    on: bool,
}

impl RawMode {
    pub(crate) fn enable(want: bool) -> Self {
        let on = want && crossterm::terminal::enable_raw_mode().is_ok();
        RawMode { on }
    }
    pub(crate) fn is_on(&self) -> bool {
        self.on
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.on {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Clip to `max` characters on one line.
pub fn clip(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// `12345` → `12.3k`.
pub fn tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub(crate) struct Buf(pub Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn raw_mode_translates_newlines_and_status_is_erased() {
        let buf = Buf::default();
        let mut out = Out::sink(Box::new(buf.clone()));
        out.set_raw(true);
        out.status("working");
        out.line("hello");
        let s = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(s.ends_with("\r\x1b[2Khello\r\n"), "{s:?}");
    }

    #[test]
    fn line_after_streamed_text_starts_fresh() {
        let buf = Buf::default();
        let mut out = Out::sink(Box::new(buf.clone()));
        out.text("partial");
        out.line("next");
        assert_eq!(
            String::from_utf8(buf.0.lock().unwrap().clone()).unwrap(),
            "partial\nnext\n"
        );
    }

    #[test]
    fn token_formatting() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1840), "1.8k");
        assert_eq!(tokens(64_000), "64k");
    }
}
