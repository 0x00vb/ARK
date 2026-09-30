//! The interactive loop: read a line with a real line editor (reedline: history, multiline,
//! cursor movement, paste, resize, completion), then either run a slash command or hand the
//! text to the agent for a turn.

use std::borrow::Cow;

use reedline::{
    default_emacs_keybindings, ColumnarMenu, Completer, CompletionResult, DefaultHinter,
    EditCommand, Emacs, FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder, Prompt,
    PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline, ReedlineEvent,
    ReedlineMenu, Signal, Span, Suggestion, ValidationResult, Validator,
};

use std::collections::VecDeque;

use ark_interact::Effect;

use crate::app::{ShellState, Welcome};
use crate::driver::run_turn;

pub(crate) async fn interactive(mut st: ShellState, resumed: bool) -> std::process::ExitCode {
    print_welcome(&mut st, resumed);
    if resumed && st.ix.session().turn_count() > 0 {
        let recap = st.ix.recap();
        st.print(&recap);
    }

    let mut editor = build_editor(&st);
    let prompt = ShellPrompt {
        symbol: st.cfg.theme.prompt.clone(),
        color: st.cfg.theme.accent.line_editor_color(),
        plain: !st.cfg.theme.color_enabled(),
    };
    let mut pending: VecDeque<String> = VecDeque::new();
    let mut ctrl_c_hint = false;

    loop {
        // Messages set up during the last turn go first, echoed after the prompt.
        let line = match pending.pop_front() {
            Some(next) => {
                st.out.line(&format!(
                    "{}{next}",
                    st.cfg.theme.accented(&st.cfg.theme.prompt)
                ));
                next
            }
            None => {
                let signal = read_line(&mut editor, &prompt);
                match signal {
                    Ok(Signal::Success(buffer)) => {
                        ctrl_c_hint = false;
                        buffer
                    }
                    Ok(Signal::CtrlC) => {
                        if !ctrl_c_hint {
                            st.out.line(&st.cfg.theme.dim("(ctrl+d or /exit to leave)"));
                            ctrl_c_hint = true;
                        }
                        continue;
                    }
                    Ok(Signal::CtrlD) => break,
                    Ok(_) => continue,
                    Err(e) => {
                        st.out.line(&st.cfg.theme.red(&format!("input error: {e}")));
                        break;
                    }
                }
            }
        };

        let text = join_continuations(&line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }

        let input = if text.starts_with('/') {
            let reply = st.ix.execute(text).await;
            st.print(&reply.report);
            match reply.effect {
                Some(Effect::Submit(t)) => t,
                Some(Effect::Exit) => break,
                Some(Effect::ClearView) => {
                    st.out.text("\x1b[2J\x1b[3J\x1b[H");
                    continue;
                }
                None => {
                    st.out.end_line();
                    continue;
                }
            }
        } else {
            text.to_string()
        };

        let next = run_turn(&mut st, &input).await;
        pending.extend(next.submit);
        if !next.draft.trim().is_empty() {
            editor.run_edit_commands(&[EditCommand::InsertString(next.draft)]);
        }
        st.out.line("");
    }

    let s = st.ix.session();
    if s.turn_count() > 0 {
        let msg = format!(
            "Session {} saved ({} turns). Continue with `{} --resume`.",
            s.id().short(),
            s.turn_count(),
            st.cfg.bin
        );
        st.out.line(&st.cfg.theme.dim(&msg));
    }
    std::process::ExitCode::SUCCESS
}

/// Block on the line editor. On a multi-threaded runtime, tell tokio this thread is busy.
fn read_line(editor: &mut Reedline, prompt: &ShellPrompt) -> std::io::Result<Signal> {
    let flavor = tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor());
    match flavor {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => {
            tokio::task::block_in_place(|| editor.read_line(prompt))
        }
        _ => editor.read_line(prompt),
    }
    .map_err(|e| std::io::Error::other(e.to_string()))
}

fn build_editor(st: &ShellState) -> Reedline {
    let mut keys = default_emacs_keybindings();
    keys.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("commands".into()),
            ReedlineEvent::MenuNext,
        ]),
    );
    for (m, k) in [
        (KeyModifiers::ALT, KeyCode::Enter),
        (KeyModifiers::SHIFT, KeyCode::Enter),
        (KeyModifiers::CONTROL, KeyCode::Char('j')),
    ] {
        keys.add_binding(m, k, ReedlineEvent::Edit(vec![EditCommand::InsertNewline]));
    }

    let commands: Vec<(String, String)> = st
        .ix
        .commands()
        .into_iter()
        .flat_map(|c| {
            let mut v = vec![(c.name.clone(), c.summary.clone())];
            v.extend(c.aliases.iter().map(|a| (a.clone(), c.summary.clone())));
            v
        })
        .collect();

    let mut editor = Reedline::create()
        .with_edit_mode(Box::new(Emacs::new(keys)))
        .with_completer(Box::new(SlashCompleter { commands }))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(
            ColumnarMenu::default().with_name("commands"),
        )))
        .with_validator(Box::new(Continuation))
        .with_hinter(Box::new(
            DefaultHinter::default().with_style(reedline::Color::DarkGray.normal()),
        ))
        .use_bracketed_paste(true)
        .with_ansi_colors(st.cfg.theme.color_enabled());
    if let Some(path) = &st.cfg.history_file {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(h) = FileBackedHistory::with_file(1000, path.clone()) {
            editor = editor.with_history(Box::new(h));
        }
    }
    editor
}

/// Lines ending in `\` continue onto the next line.
fn join_continuations(s: &str) -> String {
    s.replace("\\\r\n", "\n").replace("\\\n", "\n")
}

struct Continuation;
impl Validator for Continuation {
    fn validate(&self, line: &str) -> ValidationResult {
        if line.ends_with('\\') {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}

/// Completes `/commands` at the start of the input.
struct SlashCompleter {
    commands: Vec<(String, String)>,
}

impl Completer for SlashCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let head = &line[..pos.min(line.len())];
        if !head.starts_with('/') || head.contains(char::is_whitespace) {
            return CompletionResult::fresh(Vec::<Suggestion>::new());
        }
        let typed = &head[1..];
        let mut out: Vec<Suggestion> = self
            .commands
            .iter()
            .filter(|(n, _)| n.starts_with(typed))
            .map(|(n, d)| Suggestion {
                value: format!("/{n}"),
                description: Some(d.clone()),
                span: Span { start: 0, end: pos },
                append_whitespace: true,
                ..Default::default()
            })
            .collect();
        out.sort_by(|a, b| a.value.cmp(&b.value));
        CompletionResult::fresh(out)
    }
}

struct ShellPrompt {
    symbol: String,
    color: reedline::Color,
    plain: bool,
}

impl Prompt for ShellPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(self.symbol.clone())
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(" ".repeat(self.symbol.chars().count()))
    }
    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let prefix = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "no match ",
        };
        Cow::Owned(format!("({prefix}search: {}) ", search.term))
    }
    fn get_indicator_color(&self) -> reedline::Color {
        if self.plain {
            reedline::Color::Default
        } else {
            self.color
        }
    }
    fn get_prompt_color(&self) -> reedline::Color {
        self.get_indicator_color()
    }
}

fn print_welcome(st: &mut ShellState, resumed: bool) {
    let cfg = st.cfg.clone();
    let sum = st.ix.summary();
    let info = Welcome {
        name: sum.name,
        tagline: sum.tagline,
        model: sum.model,
        project: sum.project.name,
        session: sum.session.short,
        resumed,
        turns: sum.session.turns,
    };
    let text = match &cfg.welcome {
        Some(f) => f(&info, &cfg.theme),
        None => default_welcome(&info, &cfg.theme),
    };
    for l in text.lines() {
        st.out.line(l);
    }
    st.out.line("");
}

fn default_welcome(w: &Welcome, t: &crate::theme::Theme) -> String {
    let mut s = String::new();
    s.push('\n');
    s.push_str(&format!(
        "  {} {}\n",
        t.bold(&t.accented(&w.name)),
        t.dim("· powered by ARK")
    ));
    if let Some(tag) = &w.tagline {
        s.push_str(&format!("  {}\n", t.dim(tag)));
    }
    let session = if w.resumed {
        format!("session {} (resumed, {} turns)", w.session, w.turns)
    } else {
        format!("session {}", w.session)
    };
    s.push_str(&format!(
        "  {}\n",
        t.dim(&format!(
            "model {} · project {} · {session}",
            w.model, w.project
        ))
    ));
    s.push_str(&format!(
        "  {}\n",
        t.dim("/help for commands · esc stops the agent · ctrl+d leaves")
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_completion_matches_prefixes_only_at_start() {
        let mut c = SlashCompleter {
            commands: vec![
                ("resume".into(), "r".into()),
                ("research".into(), "x".into()),
                ("help".into(), "h".into()),
            ],
        };
        let r = c.complete("/res", 4);
        let vals: Vec<&str> = r.suggestions().iter().map(|s| s.value.as_str()).collect();
        assert_eq!(vals, vec!["/research", "/resume"]);
        assert!(c.complete("hello /res", 10).suggestions().is_empty());
        assert!(
            c.complete("/resume abc", 11).suggestions().is_empty(),
            "no completion once args start"
        );
    }

    #[test]
    fn backslash_continues_lines() {
        assert!(matches!(
            Continuation.validate("first line \\"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            Continuation.validate("done"),
            ValidationResult::Complete
        ));
        assert_eq!(join_continuations("a \\\nb"), "a \nb");
    }
}
