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

use crate::app::{ShellState, Welcome};
use crate::commands::{parse, CommandContext, CommandOutcome};
use crate::driver::run_turn;
use crate::term::clip;

pub(crate) async fn interactive(mut st: ShellState, resumed: bool) -> std::process::ExitCode {
    print_welcome(&mut st, resumed);
    if resumed && st.session.turn_count() > 0 {
        print_recap(&mut st);
    }

    let mut editor = build_editor(&st);
    let prompt = ShellPrompt {
        symbol: st.cfg.theme.prompt.clone(),
        color: st.cfg.theme.accent.line_editor_color(),
        plain: !st.cfg.theme.color_enabled(),
    };
    let mut pending: Option<String> = None;
    let mut ctrl_c_hint = false;

    loop {
        // Submit typed-ahead text directly, or leave it in the editor for the user to finish.
        let line = match pending.take() {
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

        let mut submit: Option<String> = None;
        if let Some((name, args)) = parse(text) {
            match st.cfg.find_command(name) {
                None => {
                    st.out.line(
                        &st.cfg
                            .theme
                            .yellow(&format!("Unknown command /{name}. /help lists commands.")),
                    );
                }
                Some(cmd) => {
                    let args = args.to_string();
                    let outcome = {
                        let mut ctx = CommandContext { st: &mut st };
                        cmd.run(&mut ctx, &args).await
                    };
                    match outcome {
                        Ok(CommandOutcome::Continue) => {}
                        Ok(CommandOutcome::Submit(t)) => submit = Some(t),
                        Ok(CommandOutcome::Exit) => break,
                        Err(e) => st.out.line(&st.cfg.theme.red(&format!("/{name}: {e}"))),
                    }
                }
            }
            st.out.end_line();
            if submit.is_none() {
                continue;
            }
        }

        let input = submit.unwrap_or_else(|| text.to_string());
        if let Some(ahead) = run_turn(&mut st, &input).await {
            if ahead.submit {
                pending = Some(ahead.text.trim().to_string());
            } else {
                editor.run_edit_commands(&[EditCommand::InsertString(ahead.text)]);
            }
        }
        st.out.line("");
    }

    let s = &st.session;
    if s.turn_count() > 0 {
        st.out.line(&st.cfg.theme.dim(&format!(
            "Session {} saved ({} turns). Continue with `{} --resume`.",
            s.id().short(),
            s.turn_count(),
            st.cfg.bin
        )));
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
        .cfg
        .commands
        .iter()
        .flat_map(|c| {
            let mut v = vec![(c.name().to_string(), c.summary().to_string())];
            v.extend(
                c.aliases()
                    .iter()
                    .map(|a| (a.to_string(), c.summary().to_string())),
            );
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
    let info = Welcome {
        name: cfg.name.clone(),
        tagline: cfg.tagline.clone(),
        model: st.ark.router().describe(),
        project: st.project.name.clone(),
        session: st.session.id().short().to_string(),
        resumed,
        turns: st.session.turn_count(),
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

/// Show where a resumed session left off: the last exchange, clipped.
pub(crate) fn print_recap(st: &mut ShellState) {
    let theme = st.cfg.theme.clone();
    let turns = st
        .session
        .transcript(st.ark.engine(), Some(2))
        .unwrap_or_default();
    let rec = st.session.record().clone();
    st.out.line(&format!(
        "{} {} {}",
        theme.green(&theme.glyphs.ok),
        theme.bold(&format!(
            "Resumed {}",
            if rec.title.is_empty() {
                rec.id.short().to_string()
            } else {
                clip(&rec.title, 60)
            }
        )),
        theme.dim(&format!(
            "· {} turns · {}",
            rec.turn_count,
            crate::commands::ago(rec.updated_at)
        ))
    ));
    for t in turns {
        st.out.line(&format!(
            "  {} {}",
            theme.accented(theme.prompt.trim()),
            clip(&t.input, 100)
        ));
        let answer = if t.cancelled {
            theme.dim("(stopped)")
        } else {
            theme.dim(&clip(&t.answer, 160))
        };
        st.out.line(&format!("    {answer}"));
    }
    st.out.line(
        &theme.dim("  The agent continues from its working state; the transcript is not replayed."),
    );
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
