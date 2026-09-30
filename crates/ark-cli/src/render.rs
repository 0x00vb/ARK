//! Terminal rendering of [`Report`]s. The only place that decides how a command's result looks
//! in a terminal: tones become colours, fields align, the last table column is clipped to the
//! window.

use ark_interact::fmt::clip;
use ark_interact::report::{column_widths, Align, Block, Report, Table, Tone};

use crate::theme::Theme;

pub(crate) fn toned(t: &Theme, tone: Tone, s: &str) -> String {
    match tone {
        Tone::Normal => s.to_string(),
        Tone::Muted => t.dim(s),
        Tone::Accent => t.accented(s),
        Tone::Success => t.green(s),
        Tone::Warning => t.yellow(s),
        Tone::Error => t.red(s),
    }
}

/// A report as terminal lines, fitted to `width` columns.
pub fn render(r: &Report, t: &Theme, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(title) = &r.title {
        out.push(t.bold(&t.accented(title)));
    }
    for (i, b) in r.blocks.iter().enumerate() {
        match b {
            Block::Heading { text } => {
                if i > 0 || r.title.is_some() {
                    out.push(String::new());
                }
                out.push(t.bold(&t.accented(text)));
            }
            Block::Text { text, tone } => {
                for l in text.lines() {
                    out.push(match tone {
                        Tone::Success => format!("{} {l}", t.green(&t.glyphs.ok)),
                        Tone::Warning | Tone::Error => toned(t, *tone, l),
                        _ => format!("  {}", toned(t, *tone, l)),
                    });
                }
            }
            Block::Fields { fields } => {
                let w = fields
                    .iter()
                    .map(|f| f.label.chars().count())
                    .max()
                    .unwrap_or(0)
                    .clamp(12, 22);
                for f in fields {
                    let room = width.saturating_sub(w + 5).max(20);
                    out.push(format!(
                        "  {} {}",
                        t.dim(&format!("{:<w$}", f.label)),
                        toned(t, f.tone, &clip(&f.value, room))
                    ));
                }
            }
            Block::Table(table) => out.extend(table_lines(table, t, width)),
            Block::Code { text } => {
                for l in text.lines() {
                    out.push(format!("  {l}"));
                }
            }
            Block::Meter { label, used, total } => {
                let cells = 24usize;
                let ratio = if *total == 0 {
                    0.0
                } else {
                    *used as f64 / *total as f64
                };
                let filled = ((ratio * cells as f64).round() as usize).min(cells);
                out.push(format!(
                    "  {}{} {}",
                    t.accented(&"━".repeat(filled)),
                    t.dim(&"━".repeat(cells - filled)),
                    t.dim(&format!(
                        "{} / {} {label} ({:.0}%)",
                        ark_interact::fmt::tokens(*used),
                        ark_interact::fmt::tokens(*total),
                        ratio * 100.0
                    ))
                ));
            }
            Block::Actions { actions } => {
                for a in actions {
                    out.push(t.dim(&format!("  {}  {}", a.command, a.label)));
                }
            }
        }
    }
    out
}

fn table_lines(table: &Table, t: &Theme, width: usize) -> Vec<String> {
    let mut w = column_widths(table);
    let n = w.len();
    if n == 0 {
        return Vec::new();
    }
    // Everything before the last column keeps its width; the last one gets what is left.
    let fixed: usize = w[..n - 1].iter().sum::<usize>() + 2 * (n - 1) + 2;
    w[n - 1] = w[n - 1].min(width.saturating_sub(fixed).max(12));
    let line = |cells: Vec<(String, Tone)>, row_tone: Tone| {
        let parts: Vec<String> = cells
            .into_iter()
            .enumerate()
            .map(|(i, (text, tone))| {
                let width = w.get(i).copied().unwrap_or(0);
                let text = if i == n - 1 { clip(&text, width) } else { text };
                let padded = match table.columns.get(i).map(|c| c.align) {
                    Some(Align::Right) => format!("{text:>width$}"),
                    _ if i == n - 1 => text,
                    _ => format!("{text:<width$}"),
                };
                let tone = if row_tone == Tone::Normal {
                    tone
                } else {
                    row_tone
                };
                toned(t, tone, &padded)
            })
            .collect();
        format!("  {}", parts.join("  ")).trim_end().to_string()
    };
    let mut out = Vec::new();
    if table.has_header() {
        out.push(line(
            table
                .columns
                .iter()
                .map(|c| (c.title.clone(), Tone::Muted))
                .collect(),
            Tone::Normal,
        ));
    }
    for r in &table.rows {
        out.push(line(
            r.cells.iter().map(|c| (c.text.clone(), c.tone)).collect(),
            r.tone,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::report::Table;

    #[test]
    fn renders_fields_tables_and_notes_without_colour() {
        let t = Theme::default().with_color(false);
        let r = Report::titled("Usage")
            .field("turns", "3")
            .table(
                Table::plain(2)
                    .right(1)
                    .row(["Instructions", "412"])
                    .row(["Task", "9"]),
            )
            .note("~ estimates")
            .success("compacted");
        let lines = render(&r, &t, 80);
        assert_eq!(lines[0], "Usage");
        assert_eq!(lines[1], "  turns        3");
        assert_eq!(lines[2], "  Instructions  412");
        assert_eq!(lines[3], "  Task            9");
        assert_eq!(lines[4], "  ~ estimates");
        assert_eq!(lines[5], "✓ compacted");
    }

    #[test]
    fn last_column_is_clipped_to_the_width() {
        let t = Theme::default().with_color(false);
        let r = Report::new().table(Table::plain(2).row(["name", &"x".repeat(200)]));
        let line = &render(&r, &t, 40)[0];
        assert!(line.chars().count() <= 40, "{line}");
        assert!(line.ends_with('…'));
    }
}
