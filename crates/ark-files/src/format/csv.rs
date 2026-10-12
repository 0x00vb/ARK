//! CSV and TSV: one sheet of values. The delimiter is the file's own (comma, semicolon or
//! tab); numbers become numbers so formulas over them work, and the text of every other cell
//! is kept as it was.

use crate::model::{apply_sheet, plain_number, Cell, Content, Edit, Kind, Sheet, Value, Workbook};
use crate::{Error, Format};

pub struct Csv;

impl Format for Csv {
    fn name(&self) -> &'static str {
        "csv"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["csv", "tsv"]
    }
    fn kind(&self) -> Kind {
        Kind::Sheet
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        let text = decode(bytes);
        Ok(Content::Sheet(Workbook {
            sheets: vec![parse_upto(&text, delimiter(&text), Some(crate::READ_CELLS))],
            ..Workbook::default()
        }))
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        if edits.iter().any(|e| {
            matches!(
                e,
                Edit::AddSheet { .. } | Edit::DeleteSheet { .. } | Edit::RenameSheet { .. }
            )
        }) {
            return Err(Error::Input(
                "a CSV file has one sheet; write the workbook as .xlsx to have several".into(),
            ));
        }
        let text = decode(bytes);
        let d = delimiter(&text);
        let mut book = Workbook {
            sheets: vec![parse(&text, d)],
            ..Workbook::default()
        };
        apply_sheet(&mut book, edits)?;
        let crlf = text.contains("\r\n");
        Ok(write(&book.sheets[0], d, crlf).into_bytes())
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        match content {
            Content::Sheet(book) => match book.sheets.as_slice() {
                [one] => Ok(write(one, b',', false).into_bytes()),
                [] => Ok(Vec::new()),
                _ => Err(Error::Input(
                    "a CSV file has one sheet; use .xlsx for several".into(),
                )),
            },
            Content::Text(t) => Ok(t.clone().into_bytes()),
            _ => Err(Error::Input("a CSV file takes rows".into())),
        }
    }
}

fn decode(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(b).into_owned()
}

/// The delimiter that splits the first lines most evenly.
pub fn delimiter(text: &str) -> u8 {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).take(10).collect();
    let mut best = (b',', 0usize);
    for d in *b",;\t|" {
        let counts: Vec<usize> = lines.iter().map(|l| outside_quotes(l, d)).collect();
        let Some(&first) = counts.first() else {
            continue;
        };
        if first > 0 && counts.iter().all(|c| *c == first) && first > best.1 {
            best = (d, first);
        }
    }
    best.0
}

/// How many times `d` appears in a line outside quoted fields.
fn outside_quotes(line: &str, d: u8) -> usize {
    let mut quoted = false;
    line.bytes()
        .filter(|b| {
            if *b == b'"' {
                quoted = !quoted;
            }
            !quoted && *b == d
        })
        .count()
}

pub fn parse(text: &str, d: u8) -> Sheet {
    parse_upto(text, d, None)
}

fn put(sheet: &mut Sheet, r: u32, c: u32, s: &mut String, was_quoted: bool) {
    let v = std::mem::take(s);
    if v.is_empty() {
        return;
    }
    let value = match (was_quoted, crate::model::parse_number(&v)) {
        (false, Some(n)) => Value::Number(n),
        _ => Value::Text(v),
    };
    sheet.cells.insert(
        (r, c),
        Cell {
            value,
            ..Cell::default()
        },
    );
}

/// The rows a parse stopped before, counted without keeping them: line ends outside quoted
/// fields, and a last line without one.
fn rows_left(rest: &str) -> u32 {
    let (mut rows, mut quoted, mut content) = (0u32, false, false);
    for b in rest.bytes() {
        match b {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => {
                rows += u32::from(content);
                content = false;
            }
            b'\r' => {}
            _ => content = true,
        }
    }
    rows + u32::from(content)
}

/// [`parse`], stopping at the end of the row where the sheet reaches `limit` cells.
pub fn parse_upto(text: &str, d: u8, limit: Option<usize>) -> Sheet {
    let mut sheet = Sheet::new("Sheet1");
    let d = d as char;
    let (mut r, mut c) = (0u32, 0u32);
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = text.char_indices().peekable();
    let mut was_quoted = false;
    while let Some((at, ch)) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek().map(|p| p.1) == Some('"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cur.push(ch);
            }
        } else if ch == '"' && cur.is_empty() {
            quoted = true;
            was_quoted = true;
        } else if ch == d {
            put(&mut sheet, r, c, &mut cur, was_quoted);
            was_quoted = false;
            c += 1;
        } else if ch == '\n' || ch == '\r' {
            let mut end = at + 1;
            if ch == '\r' && chars.peek().map(|p| p.1) == Some('\n') {
                chars.next();
                end += 1;
            }
            put(&mut sheet, r, c, &mut cur, was_quoted);
            was_quoted = false;
            r += 1;
            c = 0;
            if limit.is_some_and(|l| sheet.cells.len() >= l) {
                sheet.unread = rows_left(&text[end..]);
                break;
            }
        } else {
            cur.push(ch);
        }
    }
    put(&mut sheet, r, c, &mut cur, was_quoted);
    let _ = (&mut r, &mut c);
    // Formulas typed into a CSV stay formulas.
    let formulas: Vec<(u32, u32)> = sheet
        .cells
        .iter()
        .filter(
            |(_, cell)| matches!(&cell.value, Value::Text(t) if t.starts_with('=') && t.len() > 1),
        )
        .map(|(k, _)| *k)
        .collect();
    for k in formulas {
        if let Some(cell) = sheet.cells.get_mut(&k) {
            if let Value::Text(t) = &cell.value {
                cell.formula = Some(t[1..].to_string());
            }
        }
    }
    let mut book = Workbook {
        sheets: vec![sheet],
        ..Workbook::default()
    };
    crate::formula::recalc(&mut book);
    book.sheets.pop().unwrap_or_default()
}

pub fn write(sheet: &Sheet, d: u8, crlf: bool) -> String {
    let (rows, cols) = sheet.size();
    let d = d as char;
    let mut out = String::new();
    for r in 0..rows {
        let mut line = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            let s = match sheet.get(r, c) {
                None => String::new(),
                Some(cell) => match (&cell.formula, &cell.value) {
                    (Some(f), _) => format!("={f}"),
                    (None, Value::Number(n)) => plain_number(*n),
                    (None, _) => cell.display(),
                },
            };
            let needs = s.contains(d) || s.contains('"') || s.contains('\n') || s.contains('\r');
            line.push(if needs {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s
            });
        }
        out.push_str(&line.join(&d.to_string()));
        out.push_str(if crlf { "\r\n" } else { "\n" });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_large_csv_reads_its_start_and_counts_the_rest() {
        let rows = crate::READ_CELLS / 2 + 10;
        let mut text = String::from("a,b\n");
        for r in 1..rows {
            text.push_str(&format!("{r},\"x\ny\"\n"));
        }
        let read = super::parse_upto(&text, b',', Some(crate::READ_CELLS));
        let (kept, _) = read.size();
        assert_eq!(
            kept + read.unread,
            rows as u32,
            "a quoted line break is not a row"
        );
        assert!(read.unread > 0);
        assert_eq!(super::parse(&text, b',').size().0, rows as u32);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn reads_and_edits_keeping_its_delimiter() {
        let src = "name;amount\n\"Smith; J\";10\nLee;0012\n";
        let Content::Sheet(b) = Csv.read(src.as_bytes()).unwrap() else {
            panic!()
        };
        let s = &b.sheets[0];
        assert_eq!(s.get(1, 0).unwrap().value, Value::Text("Smith; J".into()));
        assert_eq!(s.get(1, 1).unwrap().value, Value::Number(10.0));
        assert_eq!(
            s.get(2, 1).unwrap().value,
            Value::Text("0012".into()),
            "ids keep their zeros"
        );
        let edits = Edit::parse_all(
            &json!([{"cell": "B4", "value": "=SUM(B2:B3)"}, {"cell": "A4", "value": "Total"}]),
        )
        .unwrap();
        let out = String::from_utf8(Csv.edit(src.as_bytes(), &edits).unwrap()).unwrap();
        assert_eq!(
            out,
            "name;amount\n\"Smith; J\";10\nLee;0012\nTotal;=SUM(B2:B3)\n"
        );
        let Content::Sheet(b) = Csv.read(out.as_bytes()).unwrap() else {
            panic!()
        };
        assert_eq!(
            b.sheets[0].get(3, 1).unwrap().value,
            Value::Number(10.0),
            "0012 is text, not summed"
        );
        assert!(Csv
            .edit(
                src.as_bytes(),
                &Edit::parse_all(&json!([{"add_sheet": "X"}])).unwrap()
            )
            .is_err());
    }
}
