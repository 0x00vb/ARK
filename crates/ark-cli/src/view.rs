//! Renders one turn as terminal activity. The words come from `ark-interact` (an [`Activity`]
//! per event); this module decides only how they look: bullets, the `└` detail line, repeats
//! collapsed into a count, the streamed answer, the status line. The debug view adds the
//! trace line of steps, context reports and model calls.

use std::time::Instant;

use ark_interact::fmt::{clip, tokens};
use ark_interact::trace::entry;
use ark_interact::{Activity, ActivityKind, TurnEnd};
use ark_sdk::prelude::*;

use crate::app::EventHook;
use crate::markdown::MdStream;
use crate::render::toned;
use crate::term::Out;
use crate::theme::Theme;

pub(crate) struct TurnView<'a> {
    theme: &'a Theme,
    hook: Option<&'a EventHook>,
    pub verbose: bool,
    /// Draw the spinner/status line (interactive only).
    pub live: bool,
    md: MdStream,
    streaming: bool,
    started: Instant,
    step: usize,
    tokens_used: u64,
    frame: usize,
    /// What the person is typing while the agent works.
    pub typeahead: String,
    /// A message queued for after the turn.
    pub queued: Option<String>,
    pub cancelling: bool,
    pub in_menu: bool,
    /// (label, repeat count, line counter right after it was printed).
    last_activity: Option<(String, u32, u64)>,
}

impl<'a> TurnView<'a> {
    pub(crate) fn new(theme: &'a Theme, hook: Option<&'a EventHook>, verbose: bool) -> Self {
        TurnView {
            theme,
            hook,
            verbose,
            live: false,
            md: MdStream::new(theme.color_enabled(), theme.raw_accent_sgr()),
            streaming: false,
            started: Instant::now(),
            step: 0,
            tokens_used: 0,
            frame: 0,
            typeahead: String::new(),
            queued: None,
            cancelling: false,
            in_menu: false,
            last_activity: None,
        }
    }

    fn close_stream(&mut self, out: &mut Out) {
        if self.streaming {
            let tail = self.md.finish();
            out.text(&tail);
            out.end_line();
            self.streaming = false;
        }
    }

    fn activity_line(&mut self, out: &mut Out, label: &str) {
        self.close_stream(out);
        let bullet = self.theme.accented(&self.theme.glyphs.activity);
        // The same activity again, with nothing printed in between: count it on one line.
        if let Some((last, n, at)) = self.last_activity.as_mut() {
            if last == label && *at == out.lines_written() && !out.is_mid_line() {
                *n += 1;
                out.clear_status();
                out.rewind(1);
                out.line(&format!(
                    "{bullet} {label} {}",
                    self.theme.dim(&format!("×{n}"))
                ));
                *at = out.lines_written();
                return;
            }
        }
        out.line(&format!("{bullet} {label}"));
        self.last_activity = Some((label.to_string(), 1, out.lines_written()));
    }

    fn detail(&mut self, out: &mut Out, text: &str, tone: ark_interact::Tone) {
        let width = out.width().saturating_sub(6);
        let glyph = if tone == ark_interact::Tone::Error {
            self.theme.red(&self.theme.glyphs.result)
        } else {
            self.theme.dim(&self.theme.glyphs.result)
        };
        let tone = if tone == ark_interact::Tone::Normal {
            ark_interact::Tone::Muted
        } else {
            tone
        };
        out.line(&format!(
            "  {glyph} {}",
            toned(self.theme, tone, &clip(text, width))
        ));
    }

    fn debug(&mut self, out: &mut Out, text: &str) {
        if self.verbose {
            self.close_stream(out);
            let width = out.width().saturating_sub(4);
            out.line(&format!("    {}", self.theme.dim(&clip(text, width))));
        }
    }

    /// A line printed by the driver (e.g. a message sent mid-turn).
    pub(crate) fn note(&mut self, out: &mut Out, text: &str) {
        self.close_stream(out);
        out.line(&format!("  {}", self.theme.dim(text)));
    }

    pub(crate) fn event(&mut self, ev: HarnessEvent, act: Option<Activity>, out: &mut Out) {
        if let Some(hook) = self.hook {
            if let Some(line) = hook(&ev, self.theme) {
                self.close_stream(out);
                out.line(&line);
                return;
            }
        }
        match &ev {
            HarnessEvent::TextDelta { text } => {
                if !self.streaming {
                    self.streaming = true;
                    out.end_line();
                }
                let styled = self.md.push(text);
                out.text(&styled);
                return;
            }
            HarnessEvent::Answer { text, streamed } => {
                self.close_stream(out);
                if !streamed {
                    out.end_line();
                    let t = self.theme;
                    let mut md = MdStream::new(t.color_enabled(), t.raw_accent_sgr());
                    let styled = md.push(text) + &md.finish();
                    out.text(&styled);
                    out.end_line();
                }
                return;
            }
            HarnessEvent::Step { index } => self.step = *index,
            HarnessEvent::ModelCall { usage, .. } => self.tokens_used += usage.total() as u64,
            _ => {}
        }
        if let Some(a) = act {
            self.activity(out, &a);
        }
        // The debug view: the trace line of this event, and a call's arguments.
        if self.verbose {
            match &ev {
                HarnessEvent::ToolCall { action, input, .. } => {
                    self.debug(out, &format!("{action} {input}"))
                }
                HarnessEvent::ToolResult {
                    duration_ms,
                    invocation,
                    context_tokens,
                    ..
                } => {
                    let d = duration_ms.map(|d| format!("{d}ms · ")).unwrap_or_default();
                    let inv = &invocation[..8.min(invocation.len())];
                    self.debug(out, &format!("{d}+{context_tokens} tokens · inv:{inv}"));
                }
                HarnessEvent::Step { .. }
                | HarnessEvent::Context { .. }
                | HarnessEvent::ModelCall { .. }
                | HarnessEvent::SelfCheck { passed: true, .. }
                | HarnessEvent::Duplicate { .. }
                | HarnessEvent::Retrieval { .. } => {
                    if let Some((_, text, detail, _)) = entry(&ev) {
                        let d = detail.map(|d| format!(" · {d}")).unwrap_or_default();
                        self.debug(out, &format!("{text}{d}"));
                    }
                }
                _ => {}
            }
        }
    }

    fn activity(&mut self, out: &mut Out, a: &Activity) {
        let t = self.theme;
        match a.kind {
            ActivityKind::Plan => {
                self.activity_line(out, &a.label);
                for s in &a.items {
                    out.line(&format!(
                        "    {}",
                        t.dim(&clip(s, out.width().saturating_sub(6)))
                    ));
                }
            }
            ActivityKind::Tool
            | ActivityKind::Reuse
            | ActivityKind::Retrieval
            | ActivityKind::Present => self.activity_line(out, &a.label),
            ActivityKind::Capability | ActivityKind::Steered => {
                self.activity_line(out, &a.label);
                if let Some(d) = &a.detail {
                    self.detail(out, d, ark_interact::Tone::Muted);
                }
            }
            ActivityKind::Result | ActivityKind::Approval => {
                self.close_stream(out);
                if !a.label.is_empty() {
                    self.detail(out, &a.label, a.tone);
                }
            }
            ActivityKind::State | ActivityKind::Note => {
                self.close_stream(out);
                let w = out.width().saturating_sub(12);
                let d = a.detail.as_deref().map(|d| clip(d, w)).unwrap_or_default();
                out.line(&format!(
                    "  {} {}",
                    t.dim(&format!("{}:", a.label)),
                    t.dim(&d)
                ));
            }
            // Only replayed turns carry narration as an activity; live, it streams.
            ActivityKind::Narration => {
                self.close_stream(out);
                out.line(&format!(
                    "  {}",
                    t.dim(&clip(&a.label, out.width().saturating_sub(4)))
                ));
            }
            ActivityKind::Warning => {
                self.close_stream(out);
                out.line(&format!(
                    "  {} {}",
                    t.yellow(&t.glyphs.warn),
                    t.yellow(&a.label)
                ));
            }
            ActivityKind::Stopped => {
                self.close_stream(out);
                out.line(&t.yellow(&a.label));
            }
        }
    }

    /// Redraw the status line (interactive).
    pub(crate) fn tick(&mut self, out: &mut Out) {
        if !self.live || self.in_menu || self.streaming {
            return;
        }
        self.frame = (self.frame + 1) % self.theme.glyphs.spinner.len().max(1);
        let spin = self.theme.accented(&self.theme.glyphs.spinner[self.frame]);
        let secs = self.started.elapsed().as_secs();
        let what = if self.cancelling {
            "Stopping"
        } else {
            "Working"
        };
        let mut parts = vec![format!("{secs}s")];
        if self.step > 0 {
            parts.push(format!("step {}", self.step));
        }
        if self.tokens_used > 0 {
            parts.push(format!("{} tokens", tokens(self.tokens_used)));
        }
        let hint = if self.cancelling {
            String::new()
        } else if self.typeahead.is_empty() {
            self.theme.dim("  esc to stop")
        } else {
            self.theme
                .dim("  enter: send now · tab: send after this turn")
        };
        let typed = if self.typeahead.is_empty() {
            String::new()
        } else {
            format!(
                "  {} {}",
                self.theme.accented("›"),
                clip(&self.typeahead, 40)
            )
        };
        let queued = match &self.queued {
            Some(q) => format!("  {} {}", self.theme.dim("next:"), clip(q, 30)),
            None => String::new(),
        };
        out.status(&format!(
            "{spin} {what} {}{typed}{hint}{queued}",
            self.theme.dim(&format!("· {}", parts.join(" · ")))
        ));
    }

    /// End of turn: close the stream and print the footer or the error.
    pub(crate) fn finish(mut self, out: &mut Out, end: &TurnEnd) {
        self.close_stream(out);
        out.clear_status();
        match &end.result {
            Err(e) => {
                out.line(&format!(
                    "{} {}",
                    self.theme.red(&self.theme.glyphs.fail),
                    self.theme.red(&format!("{e}"))
                ));
                out.line(&self.theme.dim(
                    "  The session is intact. Try again, /model to switch, or /trace for details.",
                ));
            }
            Ok(_) if !end.footer.is_empty() && self.theme.turn_footer => {
                out.line("");
                out.line(&self.theme.dim(&format!("  {}", end.footer.join(" · "))));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::activity::describe;
    use ark_interact::Labels;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn render(events: Vec<HarnessEvent>) -> String {
        let buf = Buf::default();
        let mut out = Out::sink(Box::new(buf.clone()));
        let theme = Theme::default().with_color(false);
        let labels = Labels::default();
        let names = HashMap::new();
        let mut view = TurnView::new(&theme, None, false);
        for e in events {
            let a = describe(&labels, &names, &e);
            view.event(e, a, &mut out);
        }
        let s = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        s
    }

    #[test]
    fn repeated_activity_collapses_into_a_count() {
        let r = || HarnessEvent::Retrieval {
            what: "get_evidence inv:1".into(),
        };
        let s = render(vec![r(), r(), r()]);
        assert!(s.ends_with("● Recalling evidence ×3\n"), "{s:?}");
    }

    #[test]
    fn tool_call_and_result_read_as_activity() {
        let s = render(vec![
            HarnessEvent::ToolCall {
                id: "c1".into(),
                action: "notes.read".into(),
                input: serde_json::json!({ "file": "a.md" }),
            },
            HarnessEvent::ToolResult {
                id: "c1".into(),
                action: "notes.read".into(),
                ok: true,
                summary: "read a.md (3 lines)".into(),
                invocation: "abcdef123456".into(),
                duration_ms: Some(4),
                error: None,
                context_tokens: 20,
            },
            HarnessEvent::TextDelta {
                text: "Done.".into(),
            },
            HarnessEvent::Answer {
                text: "Done.".into(),
                streamed: true,
            },
        ]);
        assert!(
            s.contains("● Reading a.md\n  └ read a.md (3 lines)\n"),
            "{s:?}"
        );
        assert!(s.contains("Done."));
        assert!(
            !s.contains("abcdef"),
            "invocation ids only in the debug view"
        );
    }
}
