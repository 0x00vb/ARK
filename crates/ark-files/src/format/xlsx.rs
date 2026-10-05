//! Excel workbooks (`.xlsx`, `.xlsm`). An edit rewrites only the cells it changes, inside the
//! sheets it changes: every other byte of the file (styles, column widths, merged cells,
//! charts, macros, other sheets) is written back as it was. A changed cell keeps its style.
//! The file is marked to recalculate when a spreadsheet app opens it, and its calculation
//! chain, which Excel rebuilds, is dropped.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::formula;
use crate::model::{apply_sheet, cell_ref, Cell, Content, Edit, Fmt, Kind, Sheet, Value, Workbook};
use crate::package::{
    add_override, insert_before_close, next_rel_id, relationships, rels_of, remove_override,
    resolve, Package,
};
use crate::xml::{self, attr, escape, local, Lexer, Tok};
use crate::{Error, Format};

pub struct Xlsx;

const NS_MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_SHEET: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
const CT_SHEET: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";

impl Format for Xlsx {
    fn name(&self) -> &'static str {
        "xlsx"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["xlsx", "xlsm"]
    }
    fn kind(&self) -> Kind {
        Kind::Sheet
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        let book = Book::read(Package::read(bytes)?)?;
        Ok(Content::Sheet(book.model()))
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        let mut book = Book::read(Package::read(bytes)?)?;
        let before = book.model();
        let mut after = before.clone();
        apply_sheet(&mut after, edits)?;
        book.check_deletions(&after)?;
        book.write(&before, &after)
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        let mut after = match content {
            Content::Sheet(b) => b.clone(),
            _ => {
                return Err(Error::Input(
                    "a spreadsheet takes `sheets` or `rows`".into(),
                ))
            }
        };
        if after.sheets.is_empty() {
            after.sheets.push(Sheet::new("Sheet1"));
        }
        for s in &mut after.sheets {
            s.origin = None;
        }
        formula::recalc(&mut after);
        let mut book = Book::read(template())?;
        book.write(&Workbook::default(), &after)
    }
}

/// An empty workbook: the parts every one needs, no sheet yet.
fn template() -> Package {
    let mut p = Package::new();
    p.set(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#,
    );
    p.set(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    p.set(
        "xl/workbook.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><bookViews><workbookView/></bookViews><sheets></sheets><calcPr calcId="191029" fullCalcOnLoad="1"/></workbook>"#
        ),
    );
    p.set(
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
    );
    p.set(
        "xl/styles.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}"><fonts count="2"><font><sz val="11"/><name val="Calibri"/><family val="2"/></font><font><b/><sz val="11"/><name val="Calibri"/><family val="2"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#
        ),
    );
    p
}

// ---- reading -----------------------------------------------------------------------------------

struct Book {
    pkg: Package,
    wb_part: String,
    /// The `<sheet>` elements of the workbook, in order; chart sheets included.
    entries: Vec<Entry>,
    sheets: Vec<SheetPart>,
    styles: Styles,
    date1904: bool,
}

struct Entry {
    /// The raw `<sheet .../>` element.
    raw: String,
    name: String,
    /// Index into `sheets` for a worksheet; `None` for a chart sheet or a missing part.
    sheet: Option<usize>,
}

struct SheetPart {
    name: String,
    part: String,
    xml: String,
    /// The prefix the part uses for its elements (`x:` or none).
    p: String,
    /// The `<sheetData>` element's byte range.
    data: (usize, usize),
    rows: BTreeMap<u32, String>,
    cells: BTreeMap<(u32, u32), RawCell>,
    model: Sheet,
}

struct RawCell {
    raw: String,
    style: Option<u32>,
    /// The shared-formula group the cell belongs to.
    shared: Option<String>,
    /// Whether the cell had a value or formula (an empty styled cell does not).
    content: bool,
}

impl Book {
    fn read(pkg: Package) -> Result<Book, Error> {
        let root_rels = pkg.text_opt("_rels/.rels").unwrap_or_default();
        let wb_part = relationships(&root_rels)
            .into_iter()
            .find(|r| r.1.ends_with("/officeDocument"))
            .map(|r| resolve("", &r.2))
            .unwrap_or_else(|| "xl/workbook.xml".into());
        let wb = pkg.text(&wb_part)?;
        let rels = relationships(&pkg.text_opt(&rels_of(&wb_part)).unwrap_or_default());
        let target = |id: &str| {
            rels.iter()
                .find(|r| r.0 == id)
                .map(|r| (r.1.clone(), resolve(&wb_part, &r.2)))
        };
        let date1904 = Lexer::new(&wb).any(|t| match t {
            Tok::Open { name, attrs, .. } if local(name) == "workbookPr" => {
                matches!(attr(attrs, "date1904").as_deref(), Some("1" | "true"))
            }
            _ => false,
        });
        let styles_part = rels
            .iter()
            .find(|r| r.1.ends_with("/styles"))
            .map(|r| resolve(&wb_part, &r.2));
        let styles = Styles::read(styles_part, &pkg);
        let shared = rels
            .iter()
            .find(|r| r.1.ends_with("/sharedStrings"))
            .and_then(|r| pkg.text_opt(&resolve(&wb_part, &r.2)))
            .map(|x| shared_strings(&x))
            .unwrap_or_default();

        let mut entries = Vec::new();
        let mut sheets = Vec::new();
        for t in Lexer::new(&wb) {
            let Tok::Open {
                name,
                attrs,
                start,
                end,
                ..
            } = t
            else {
                continue;
            };
            if local(name) != "sheet" {
                continue;
            }
            let sheet_name = attr(attrs, "name").unwrap_or_default().into_owned();
            let rid = xml::attrs(attrs)
                .into_iter()
                .find(|(k, _)| k.ends_with(":id"))
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
            let raw = wb[start..end].to_string();
            let sheet = match target(&rid) {
                Some((kind, part)) if kind.ends_with("/worksheet") && pkg.has(&part) => {
                    let xml = pkg.text(&part)?;
                    sheets.push(SheetPart::read(
                        &sheet_name,
                        part,
                        xml,
                        &shared,
                        &styles,
                        date1904,
                        sheets.len(),
                    )?);
                    Some(sheets.len() - 1)
                }
                _ => None,
            };
            entries.push(Entry {
                raw,
                name: sheet_name,
                sheet,
            });
        }
        if sheets.is_empty() && !entries.is_empty() {
            return Err(Error::Format(
                "the workbook has no worksheet ARK can read".into(),
            ));
        }
        let mut book = Book {
            pkg,
            wb_part,
            entries,
            sheets,
            styles,
            date1904,
        };
        // Shared formulas reach their cells; then values the file left stale are computed.
        let mut model = book.model();
        formula::recalc(&mut model);
        for (s, m) in book.sheets.iter_mut().zip(model.sheets) {
            s.model = m;
        }
        Ok(book)
    }

    fn model(&self) -> Workbook {
        Workbook {
            sheets: self.sheets.iter().map(|s| s.model.clone()).collect(),
        }
    }

    /// A sheet that formulas of other sheets read cannot go.
    fn check_deletions(&self, after: &Workbook) -> Result<(), Error> {
        let kept: BTreeSet<usize> = after.sheets.iter().filter_map(|s| s.origin).collect();
        for (i, gone) in self.sheets.iter().enumerate() {
            if kept.contains(&i) {
                continue;
            }
            for s in &after.sheets {
                for (k, c) in &s.cells {
                    if let Some(f) = &c.formula {
                        if formula::rename_sheet(f, &gone.name, "\u{1}") != *f {
                            return Err(Error::Input(format!(
                                "{}!{} reads sheet {}; change that formula before deleting the sheet",
                                s.name,
                                cell_ref(k.0, k.1),
                                gone.name
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn write(&mut self, before: &Workbook, after: &Workbook) -> Result<Vec<u8>, Error> {
        let mut wb = self.pkg.text(&self.wb_part)?;
        let rels_part = rels_of(&self.wb_part);
        let mut rels = self.pkg.text_opt(&rels_part).unwrap_or_else(|| {
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#.into()
        });
        let mut types = self.pkg.text("[Content_Types].xml")?;
        let mut changed_any = false;

        // Sheets that stay, in their order, with their new content.
        let mut kept: HashMap<usize, &Sheet> = HashMap::new();
        for s in &after.sheets {
            if let Some(o) = s.origin {
                kept.insert(o, s);
            }
        }
        for (i, part) in self.sheets.iter().enumerate() {
            match kept.get(&i) {
                Some(new) => {
                    let old = before.sheets.get(i).unwrap_or(&part.model);
                    if old.cells != new.cells {
                        let xml = part.rewrite(new, &mut self.styles, self.date1904);
                        self.pkg.set(&part.part, xml);
                        changed_any = true;
                    }
                }
                None => {
                    // A deleted sheet: its part, its relationships, its content type.
                    self.pkg.remove(&part.part);
                    self.pkg.remove(&rels_of(&part.part));
                    types = remove_override(&types, &part.part);
                    changed_any = true;
                }
            }
        }

        // The workbook's sheet list: kept entries (renamed), chart sheets, then new sheets.
        let mut list = String::new();
        let mut deleted_positions = Vec::new();
        let mut position = 0usize;
        for (pos, e) in self.entries.iter().enumerate() {
            match e.sheet {
                Some(i) => match kept.get(&i) {
                    Some(new) => {
                        if new.name != e.name {
                            list.push_str(&set_attr(&e.raw, "name", &new.name));
                            changed_any = true;
                        } else {
                            list.push_str(&e.raw);
                        }
                        position += 1;
                    }
                    None => {
                        deleted_positions.push(pos);
                        let rid = xml::attrs(&e.raw)
                            .into_iter()
                            .find(|(k, _)| k.ends_with(":id"))
                            .map(|(_, v)| v.to_string());
                        if let Some(rid) = rid {
                            rels = remove_relationship(&rels, &rid);
                        }
                    }
                },
                None => {
                    list.push_str(&e.raw);
                    position += 1;
                }
            }
        }
        let _ = position;
        let first_id = self
            .entries
            .iter()
            .filter_map(|e| attr(&e.raw, "sheetId").and_then(|v| v.parse::<u32>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let wb_prefix = prefix_of(&wb, "workbook");
        for (next_id, new) in (first_id..).zip(after.sheets.iter().filter(|s| s.origin.is_none())) {
            let part = (1..)
                .map(|n| format!("{}/worksheets/sheet{n}.xml", dir_of(&self.wb_part)))
                .find(|p| !self.pkg.has(p))
                .unwrap_or_default();
            let rid = next_rel_id(&rels);
            let target = part
                .trim_start_matches(&format!("{}/", dir_of(&self.wb_part)))
                .to_string();
            rels = insert_before_close(
                &rels,
                "Relationships",
                &format!("<Relationship Id=\"{rid}\" Type=\"{REL_SHEET}\" Target=\"{target}\"/>"),
            )
            .unwrap_or(rels);
            types = add_override(&types, &part, CT_SHEET);
            list.push_str(&format!(
                "<{wb_prefix}sheet name=\"{}\" sheetId=\"{next_id}\" r:id=\"{rid}\"/>",
                escape(&new.name)
            ));
            let xml = new_sheet_xml(new, &mut self.styles, self.date1904);
            self.pkg.set(&part, xml);
            changed_any = true;
        }
        if !changed_any {
            return self.pkg.write();
        }
        let (s0, s1) = xml::find(&wb, &format!("{wb_prefix}sheets"), 0)
            .ok_or_else(|| Error::Format("the workbook has no sheet list".into()))?;
        let open_end = wb[s0..].find('>').map_or(s1, |i| s0 + i + 1);
        let open = wb[s0..open_end]
            .trim_end_matches("/>")
            .trim_end_matches('>')
            .to_string();
        wb = format!(
            "{}{open}>{list}</{wb_prefix}sheets>{}",
            &wb[..s0],
            &wb[s1..]
        );
        wb = fix_defined_names(&wb, &deleted_positions, before, after);
        if !wb.contains(" r:id=") || !wb.contains("xmlns:r=") {
            // New sheet elements use `r:`; make sure the prefix is declared.
            if !wb.contains("xmlns:r=") {
                let at = wb
                    .find(&format!("<{wb_prefix}workbook"))
                    .map(|i| i + 1 + wb_prefix.len() + "workbook".len());
                if let Some(at) = at {
                    wb.insert_str(at, &format!(" xmlns:r=\"{NS_REL}\""));
                }
            }
        }
        wb = full_calc_on_load(&wb, &wb_prefix);
        if let Some(active) = active_tab_fix(&wb, after.sheets.len()) {
            wb = active;
        }
        // The calculation chain names formula cells; after an edit it may name the wrong ones.
        if let Some(r) = relationships(&rels)
            .into_iter()
            .find(|r| r.1.ends_with("/calcChain"))
        {
            let part = resolve(&self.wb_part, &r.2);
            self.pkg.remove(&part);
            types = remove_override(&types, &part);
            rels = remove_relationship(&rels, &r.0);
        }
        if let Some(styles) = self.styles.changed() {
            self.pkg.set(&styles.0, styles.1);
        }
        self.pkg.set(&self.wb_part, wb);
        self.pkg.set(&rels_part, rels);
        self.pkg.set("[Content_Types].xml", types);
        self.pkg.write()
    }
}

fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The prefix (`x:`) of `<x:workbook>`, or none.
fn prefix_of(xml: &str, root: &str) -> String {
    Lexer::new(xml)
        .find_map(|t| match t {
            Tok::Open { name, .. } if local(name) == root => {
                Some(name.strip_suffix(root).unwrap_or("").to_string())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// An element with attribute `key` set to `value` (added when missing).
fn set_attr(raw: &str, key: &str, value: &str) -> String {
    let v = escape(value);
    let pat = format!(" {key}=\"");
    if let Some(i) = raw.find(&pat) {
        let start = i + pat.len();
        if let Some(len) = raw[start..].find('"') {
            return format!("{}{v}{}", &raw[..start], &raw[start + len..]);
        }
    }
    // After the last attribute.
    let at = if raw.ends_with("/>") {
        raw.len() - 2
    } else {
        raw.len() - 1
    };
    let at = raw[..at].trim_end().len();
    format!("{} {key}=\"{v}\"{}", &raw[..at], &raw[at..])
}

fn remove_relationship(rels: &str, id: &str) -> String {
    for t in Lexer::new(rels) {
        if let Tok::Open {
            name,
            attrs,
            start,
            end,
            empty,
        } = t
        {
            if local(name) == "Relationship" && attr(attrs, "Id").as_deref() == Some(id) {
                let end = if empty {
                    end
                } else {
                    xml::element_end(rels, start)
                };
                return format!("{}{}", &rels[..start], &rels[end..]);
            }
        }
    }
    rels.to_string()
}

fn full_calc_on_load(wb: &str, p: &str) -> String {
    for t in Lexer::new(wb) {
        if let Tok::Open {
            name, start, end, ..
        } = t
        {
            if local(name) == "calcPr" {
                let raw = &wb[start..end];
                let new = set_attr(raw, "fullCalcOnLoad", "1");
                return format!("{}{new}{}", &wb[..start], &wb[end..]);
            }
        }
    }
    // calcPr follows these, in schema order.
    for after in [
        "definedNames",
        "externalReferences",
        "functionGroups",
        "sheets",
    ] {
        if let Some((_, end)) = xml::find(wb, &format!("{p}{after}"), 0) {
            return format!(
                "{}<{p}calcPr fullCalcOnLoad=\"1\"/>{}",
                &wb[..end],
                &wb[end..]
            );
        }
    }
    wb.to_string()
}

fn active_tab_fix(wb: &str, sheets: usize) -> Option<String> {
    for t in Lexer::new(wb) {
        if let Tok::Open {
            name,
            attrs,
            start,
            end,
            ..
        } = t
        {
            if local(name) == "workbookView" {
                let n: usize = attr(attrs, "activeTab")?.parse().ok()?;
                if n < sheets {
                    return None;
                }
                let raw = set_attr(&wb[start..end], "activeTab", "0");
                let raw = set_attr(&raw, "firstSheet", "0");
                return Some(format!("{}{raw}{}", &wb[..start], &wb[end..]));
            }
        }
    }
    None
}

/// Defined names (print areas, named ranges) follow renamed sheets; those local to a deleted
/// sheet go, and the sheet positions after it shift.
fn fix_defined_names(wb: &str, deleted: &[usize], before: &Workbook, after: &Workbook) -> String {
    let renames: Vec<(String, String)> = after
        .sheets
        .iter()
        .filter_map(|s| {
            let o = before.sheets.get(s.origin?)?;
            (o.name != s.name).then(|| (o.name.clone(), s.name.clone()))
        })
        .collect();
    if deleted.is_empty() && renames.is_empty() {
        return wb.to_string();
    }
    let mut out = String::new();
    let mut last = 0;
    let toks: Vec<Tok> = Lexer::new(wb).collect();
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Open {
            name,
            attrs,
            start,
            empty: false,
            ..
        } = toks[i]
        {
            if local(name) == "definedName" {
                let end = xml::element_end(wb, start);
                let local_id = attr(attrs, "localSheetId").and_then(|v| v.parse::<usize>().ok());
                out.push_str(&wb[last..start]);
                last = end;
                let drop = local_id.is_some_and(|l| deleted.contains(&l));
                if !drop {
                    let raw = &wb[start..end];
                    let open_end = raw.find('>').map_or(raw.len(), |x| x + 1);
                    let mut open = raw[..open_end].to_string();
                    if let Some(l) = local_id {
                        let shift = deleted.iter().filter(|d| **d < l).count();
                        if shift > 0 {
                            open = set_attr(&open, "localSheetId", &(l - shift).to_string());
                        }
                    }
                    let close = raw.rfind("</").unwrap_or(raw.len());
                    let mut body = xml::unescape(&raw[open_end..close]).into_owned();
                    for (from, to) in &renames {
                        body = formula::rename_sheet(&body, from, to);
                    }
                    out.push_str(&format!("{open}{}{}", escape(&body), &raw[close..]));
                }
                while i < toks.len() && toks[i].end() <= end {
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&wb[last..]);
    out
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((s, e)) = next_element(xml, "si", pos) {
        // Phonetic runs (`rPh`) are reading aids, not the text.
        let body = &xml[s..e];
        let mut text = String::new();
        let mut skip = 0usize;
        let mut in_t = 0usize;
        for t in Lexer::new(body) {
            match t {
                Tok::Open {
                    name, empty: false, ..
                } if local(name) == "rPh" => skip += 1,
                Tok::Close { name, .. } if local(name) == "rPh" => skip = skip.saturating_sub(1),
                Tok::Open {
                    name, empty: false, ..
                } if local(name) == "t" => in_t += 1,
                Tok::Close { name, .. } if local(name) == "t" => in_t = in_t.saturating_sub(1),
                Tok::Text { raw, .. } if in_t > 0 && skip == 0 => {
                    text.push_str(&xml::unescape(raw))
                }
                _ => {}
            }
        }
        out.push(text);
        pos = e;
    }
    out
}

/// The next element with local name `name` at or after `from`.
fn next_element(xml: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    Lexer::at(xml, from).find_map(|t| match t {
        Tok::Open {
            name: n,
            start,
            empty,
            end,
            ..
        } if local(n) == name => Some((
            start,
            if empty {
                end
            } else {
                xml::element_end(xml, start)
            },
        )),
        _ => None,
    })
}

impl SheetPart {
    #[allow(clippy::too_many_arguments)]
    fn read(
        name: &str,
        part: String,
        xml: String,
        shared: &[String],
        styles: &Styles,
        date1904: bool,
        index: usize,
    ) -> Result<SheetPart, Error> {
        let p = prefix_of(&xml, "worksheet");
        let data = next_element(&xml, "sheetData", 0)
            .ok_or_else(|| Error::Format(format!("sheet {name} has no cell data")))?;
        let mut rows = BTreeMap::new();
        let mut cells = BTreeMap::new();
        let mut model = Sheet::new(name);
        model.origin = Some(index);
        let mut masters: HashMap<String, (u32, u32, String)> = HashMap::new();
        let mut dependents: Vec<((u32, u32), String)> = Vec::new();
        let (mut row, mut col) = (0u32, 0u32);
        let mut pos = data.0;
        let body_end = data.1;
        let mut first = true;
        while let Some(t) = Lexer::at(&xml, pos).next() {
            if t.start() >= body_end {
                break;
            }
            pos = t.end();
            let Tok::Open {
                name: tag,
                attrs,
                start,
                empty,
                end,
            } = t
            else {
                continue;
            };
            if first {
                // The `<sheetData>` tag itself.
                first = false;
                if empty {
                    break;
                }
                continue;
            }
            match local(tag) {
                "row" => {
                    row = attr(attrs, "r")
                        .and_then(|v| v.parse::<u32>().ok())
                        .map(|r| r - 1)
                        .unwrap_or(if rows.is_empty() { 0 } else { row + 1 });
                    col = 0;
                    let kept: Vec<String> = xml::attrs(attrs)
                        .into_iter()
                        .filter(|(k, _)| *k != "r" && *k != "spans")
                        .map(|(k, v)| format!("{k}=\"{v}\""))
                        .collect();
                    rows.insert(row, kept.join(" "));
                }
                "c" => {
                    let at = attr(attrs, "r")
                        .and_then(|v| crate::model::parse_ref(&v))
                        .map(|(_, c)| c)
                        .unwrap_or(col);
                    col = at + 1;
                    let cend = if empty {
                        end
                    } else {
                        xml::element_end(&xml, start)
                    };
                    pos = cend;
                    let raw = xml[start..cend].to_string();
                    let style = attr(attrs, "s").and_then(|v| v.parse::<u32>().ok());
                    let typ = attr(attrs, "t").map(|t| t.into_owned());
                    let (v, f, fattrs, inline) = cell_parts(&raw);
                    let fmt = style.map(|s| styles.fmt(s)).unwrap_or_default();
                    let mut value = match typ.as_deref() {
                        Some("s") => v
                            .as_deref()
                            .and_then(|i| i.trim().parse::<usize>().ok())
                            .and_then(|i| shared.get(i))
                            .map(|s| Value::Text(s.clone()))
                            .unwrap_or_default(),
                        Some("str") => v.map(Value::Text).unwrap_or_default(),
                        Some("inlineStr") => inline.map(Value::Text).unwrap_or_default(),
                        Some("b") => Value::Bool(v.as_deref().map(str::trim) == Some("1")),
                        Some("e") => v.map(Value::Error).unwrap_or_default(),
                        Some("d") => v
                            .as_deref()
                            .and_then(|d| crate::model::date_to_serial(&d[..d.len().min(10)]))
                            .map(Value::Number)
                            .unwrap_or_default(),
                        _ => v
                            .as_deref()
                            .and_then(|n| n.trim().parse::<f64>().ok())
                            .map(Value::Number)
                            .unwrap_or_default(),
                    };
                    if date1904 {
                        if let (Value::Number(n), Fmt::Date | Fmt::DateTime) = (&value, fmt) {
                            value = Value::Number(n + 1462.0);
                        }
                    }
                    let shared_si = fattrs
                        .as_deref()
                        .filter(|a| attr(a, "t").as_deref() == Some("shared"))
                        .and_then(|a| attr(a, "si").map(|s| s.into_owned()));
                    let array = fattrs
                        .as_deref()
                        .is_some_and(|a| attr(a, "t").as_deref() == Some("array"));
                    let mut formula = f.filter(|f| !f.trim().is_empty());
                    if let Some(si) = &shared_si {
                        match &formula {
                            Some(text) => {
                                masters.insert(si.clone(), (row, at, text.clone()));
                            }
                            None => dependents.push(((row, at), si.clone())),
                        }
                    }
                    if array {
                        // Array formulas are left to the spreadsheet app.
                        formula = None;
                    }
                    let content = value != Value::Empty || formula.is_some() || shared_si.is_some();
                    if content {
                        model.cells.insert(
                            (row, at),
                            Cell {
                                value,
                                formula,
                                fmt,
                            },
                        );
                    }
                    cells.insert(
                        (row, at),
                        RawCell {
                            raw,
                            style,
                            shared: shared_si,
                            content,
                        },
                    );
                }
                _ => {}
            }
        }
        for ((r, c), si) in dependents {
            if let Some((mr, mc, text)) = masters.get(&si) {
                let f = formula::shift(text, r as i64 - *mr as i64, c as i64 - *mc as i64);
                if let Some(cell) = model.cells.get_mut(&(r, c)) {
                    cell.formula = Some(f);
                }
            }
        }
        Ok(SheetPart {
            name: name.to_string(),
            part,
            xml,
            p,
            data,
            rows,
            cells,
            model,
        })
    }

    /// The part with its cell data rewritten for `new`. Unchanged cells keep their bytes.
    fn rewrite(&self, new: &Sheet, styles: &mut Styles, date1904: bool) -> String {
        let old = &self.model;
        let changed: BTreeSet<(u32, u32)> = old
            .cells
            .keys()
            .chain(new.cells.keys())
            .filter(|k| old.cells.get(k) != new.cells.get(k))
            .copied()
            .collect();
        // A shared formula keeps its text in one cell; when any cell of the group changes,
        // every cell of it is written with its own formula.
        let broken: BTreeSet<&str> = changed
            .iter()
            .filter_map(|k| self.cells.get(k)?.shared.as_deref())
            .collect();
        let p = &self.p;
        let mut row_keys: BTreeSet<u32> = self.rows.keys().copied().collect();
        row_keys.extend(new.cells.keys().map(|k| k.0));
        let mut out = String::new();
        for r in row_keys {
            let mut cols: BTreeSet<u32> = self
                .cells
                .range((r, 0)..=(r, u32::MAX))
                .map(|(k, _)| k.1)
                .collect();
            cols.extend(new.cells.range((r, 0)..=(r, u32::MAX)).map(|(k, _)| k.1));
            let attrs = self
                .rows
                .get(&r)
                .map(|a| format!(" {a}"))
                .unwrap_or_default();
            let attrs = attrs.trim_end();
            if cols.is_empty() {
                out.push_str(&format!("<{p}row r=\"{}\"{attrs}/>", r + 1));
                continue;
            }
            out.push_str(&format!("<{p}row r=\"{}\"{attrs}>", r + 1));
            for c in cols {
                let key = (r, c);
                let raw = self.cells.get(&key);
                let regroup = raw
                    .and_then(|x| x.shared.as_deref())
                    .is_some_and(|s| broken.contains(s));
                if !changed.contains(&key) && !regroup {
                    if let Some(raw) = raw {
                        out.push_str(&raw.raw);
                        continue;
                    }
                }
                let style = raw.and_then(|x| x.style);
                match new.cells.get(&key) {
                    Some(cell) => {
                        let s = styles.style_for(style, cell.fmt);
                        out.push_str(&cell_xml(p, key, s, cell, date1904));
                    }
                    None => {
                        // Cleared: the cell keeps its look, not its value.
                        if let Some(s) = style {
                            out.push_str(&format!("<{p}c r=\"{}\" s=\"{s}\"/>", cell_ref(r, c)));
                        } else if let Some(raw) = raw.filter(|x| !x.content) {
                            out.push_str(&raw.raw);
                        }
                    }
                }
            }
            out.push_str(&format!("</{p}row>"));
        }
        let (s0, s1) = self.data;
        let mut xml = format!(
            "{}<{p}sheetData>{out}</{p}sheetData>{}",
            &self.xml[..s0],
            &self.xml[s1..]
        );
        xml = set_dimension(&xml, new);
        xml
    }
}

/// `(value, formula, formula attributes, inline string)` of a `<c>` element.
fn cell_parts(
    raw: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let (mut v, mut f, mut fa, mut inline) = (None, None, None, None);
    let mut pos = 0;
    let toks: Vec<Tok> = Lexer::new(raw).collect();
    while pos < toks.len() {
        if let Tok::Open {
            name,
            attrs,
            start,
            empty,
            ..
        } = toks[pos]
        {
            match local(name) {
                "v" if !empty => {
                    v = Some(xml::text_of(&raw[start..xml::element_end(raw, start)], "v"))
                }
                "f" => {
                    fa = Some(attrs.to_string());
                    f = if empty {
                        Some(String::new())
                    } else {
                        Some(xml::text_of(&raw[start..xml::element_end(raw, start)], "f"))
                    };
                }
                "is" if !empty => {
                    let body = &raw[start..xml::element_end(raw, start)];
                    inline = Some(xml::text_of(body, "t"));
                }
                _ => {}
            }
        }
        pos += 1;
    }
    (
        v,
        f.filter(|s| !s.is_empty())
            .or(if fa.is_some() {
                Some(String::new())
            } else {
                None
            })
            .filter(|s| !s.is_empty()),
        fa,
        inline,
    )
}

fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn cell_xml(
    p: &str,
    (r, c): (u32, u32),
    style: Option<u32>,
    cell: &Cell,
    date1904: bool,
) -> String {
    let at = cell_ref(r, c);
    let s = style.map(|s| format!(" s=\"{s}\"")).unwrap_or_default();
    let value = match (&cell.value, cell.fmt, date1904) {
        (Value::Number(n), Fmt::Date | Fmt::DateTime, true) => Value::Number(n - 1462.0),
        (v, _, _) => v.clone(),
    };
    let f = cell
        .formula
        .as_ref()
        .map(|f| format!("<{p}f>{}</{p}f>", escape(f)))
        .unwrap_or_default();
    match (&value, cell.formula.is_some()) {
        (Value::Empty, _) => format!("<{p}c r=\"{at}\"{s}>{f}</{p}c>"),
        (Value::Number(n), _) => format!("<{p}c r=\"{at}\"{s}>{f}<{p}v>{}</{p}v></{p}c>", number_text(*n)),
        (Value::Bool(b), _) => format!("<{p}c r=\"{at}\"{s} t=\"b\">{f}<{p}v>{}</{p}v></{p}c>", u8::from(*b)),
        (Value::Error(e), _) => format!("<{p}c r=\"{at}\"{s} t=\"e\">{f}<{p}v>{}</{p}v></{p}c>", escape(e)),
        (Value::Text(t), true) => format!("<{p}c r=\"{at}\"{s} t=\"str\">{f}<{p}v>{}</{p}v></{p}c>", escape(t)),
        (Value::Text(t), false) => format!(
            "<{p}c r=\"{at}\"{s} t=\"inlineStr\"><{p}is><{p}t xml:space=\"preserve\">{}</{p}t></{p}is></{p}c>",
            escape(t)
        ),
    }
}

fn set_dimension(xml: &str, sheet: &Sheet) -> String {
    for t in Lexer::new(xml) {
        if let Tok::Open {
            name, start, end, ..
        } = t
        {
            if local(name) == "dimension" {
                let raw = set_attr(&xml[start..end], "ref", &sheet.range());
                return format!("{}{raw}{}", &xml[..start], &xml[end..]);
            }
            if local(name) == "sheetData" {
                break;
            }
        }
    }
    xml.to_string()
}

/// A new sheet's part. Its first row is bold when it reads as a header, and its columns are
/// as wide as their text.
fn new_sheet_xml(sheet: &Sheet, styles: &mut Styles, date1904: bool) -> String {
    let (rows, cols) = sheet.size();
    let header = rows > 1
        && (0..cols).all(|c| {
            matches!(
                sheet.get(0, c).map(|x| &x.value),
                Some(Value::Text(_)) | None
            )
        })
        && (0..cols).any(|c| sheet.get(0, c).is_some());
    let bold = if header { styles.bold() } else { None };
    let mut widths: Vec<usize> = vec![0; cols as usize];
    for ((_, c), cell) in &sheet.cells {
        let w = cell.display().chars().count();
        if let Some(x) = widths.get_mut(*c as usize) {
            *x = (*x).max(w);
        }
    }
    let mut data = String::new();
    for r in 0..rows {
        let cells: Vec<(&(u32, u32), &Cell)> = sheet.cells.range((r, 0)..=(r, u32::MAX)).collect();
        if cells.is_empty() {
            continue;
        }
        data.push_str(&format!("<row r=\"{}\">", r + 1));
        for (k, cell) in cells {
            let base = if r == 0 { bold } else { None };
            let s = styles.style_for(base, cell.fmt);
            data.push_str(&cell_xml("", *k, s, cell, date1904));
        }
        data.push_str("</row>");
    }
    let cols_xml = if widths.iter().any(|w| *w > 8) {
        let list: String = widths
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let width = (*w as f64 * 1.1 + 2.0).clamp(8.43, 60.0);
                format!(
                    "<col min=\"{n}\" max=\"{n}\" width=\"{width:.2}\" customWidth=\"1\"/>",
                    n = i + 1
                )
            })
            .collect();
        format!("<cols>{list}</cols>")
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><dimension ref="{}"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/>{cols_xml}<sheetData>{data}</sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#,
        sheet.range()
    )
}

// ---- styles ------------------------------------------------------------------------------------

/// The workbook's cell formats: how each style shows a number, and new styles for cells that
/// need one (a percentage, a date, a bold header).
struct Styles {
    part: Option<String>,
    xml: String,
    p: String,
    fmts: Vec<Fmt>,
    /// Each `<xf>` of `cellXfs`, raw.
    xfs: Vec<String>,
    codes: HashMap<u32, String>,
    made: HashMap<(Option<u32>, u32), u32>,
    bold: Option<Option<u32>>,
    dirty: bool,
}

impl Styles {
    fn read(part: Option<String>, pkg: &Package) -> Styles {
        let xml = part
            .as_deref()
            .and_then(|p| pkg.text_opt(p))
            .unwrap_or_default();
        let p = prefix_of(&xml, "styleSheet");
        let mut codes = HashMap::new();
        for t in Lexer::new(&xml) {
            if let Tok::Open { name, attrs, .. } = t {
                if local(name) == "numFmt" {
                    if let (Some(id), Some(code)) =
                        (attr(attrs, "numFmtId"), attr(attrs, "formatCode"))
                    {
                        if let Ok(id) = id.parse() {
                            codes.insert(id, code.into_owned());
                        }
                    }
                }
            }
        }
        let mut xfs = Vec::new();
        let mut fmts = Vec::new();
        if let Some((s, e)) = next_element(&xml, "cellXfs", 0) {
            let body = &xml[s..e];
            let mut depth = 0usize;
            let mut pos = 0;
            let toks: Vec<Tok> = Lexer::new(body).collect();
            while pos < toks.len() {
                match toks[pos] {
                    Tok::Open {
                        name,
                        attrs,
                        start,
                        empty,
                        end,
                    } => {
                        if depth == 1 && local(name) == "xf" {
                            let xend = if empty {
                                end
                            } else {
                                xml::element_end(body, start)
                            };
                            xfs.push(body[start..xend].to_string());
                            let id: u32 = attr(attrs, "numFmtId")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            fmts.push(fmt_of(id, codes.get(&id).map(String::as_str)));
                            while pos < toks.len() && toks[pos].end() <= xend {
                                pos += 1;
                            }
                            continue;
                        }
                        if !empty {
                            depth += 1;
                        }
                    }
                    Tok::Close { .. } => depth = depth.saturating_sub(1),
                    _ => {}
                }
                pos += 1;
            }
        }
        Styles {
            part,
            xml,
            p,
            fmts,
            xfs,
            codes,
            made: HashMap::new(),
            bold: None,
            dirty: false,
        }
    }

    fn fmt(&self, s: u32) -> Fmt {
        self.fmts.get(s as usize).copied().unwrap_or_default()
    }

    /// The style for a cell that had `base` and now shows numbers as `fmt`.
    fn style_for(&mut self, base: Option<u32>, fmt: Fmt) -> Option<u32> {
        let current = base.map(|b| self.fmt(b)).unwrap_or_default();
        if current == fmt || fmt == Fmt::General || self.part.is_none() {
            return base;
        }
        let id = self.num_fmt_id(fmt);
        if let Some(s) = self.made.get(&(base, id)) {
            return Some(*s);
        }
        let template = base
            .and_then(|b| self.xfs.get(b as usize).cloned())
            .or_else(|| self.xfs.first().cloned())
            .unwrap_or_else(|| {
                format!(
                    "<{}xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>",
                    self.p
                )
            });
        let xf = set_attr(
            &set_attr(&template, "numFmtId", &id.to_string()),
            "applyNumberFormat",
            "1",
        );
        let s = self.push_xf(xf)?;
        self.fmts.push(fmt);
        self.made.insert((base, id), s);
        Some(s)
    }

    /// A bold style (for a header row), made once.
    fn bold(&mut self) -> Option<u32> {
        if let Some(b) = self.bold {
            return b;
        }
        let made = self.make_bold();
        self.bold = Some(made);
        made
    }

    fn make_bold(&mut self) -> Option<u32> {
        // An existing style that is only bold.
        let (fs, fe) = next_element(&self.xml, "fonts", 0)?;
        let fonts = self.xml[fs..fe].to_string();
        let mut list = Vec::new();
        let mut pos = 0;
        while let Some((s, e)) = next_element(&fonts, "font", pos) {
            list.push(fonts[s..e].to_string());
            pos = e;
        }
        let first = list.first()?.clone();
        let bold_font = list
            .iter()
            .position(|f| f.contains("<b/>") || f.contains(&format!("<{}b/>", self.p)));
        let font_id = match bold_font {
            Some(i) => i,
            None => {
                let open_end = first.find('>')? + 1;
                let font = if first.ends_with("/>") {
                    format!("<{p}font><{p}b/></{p}font>", p = self.p)
                } else {
                    format!("{}<{}b/>{}", &first[..open_end], self.p, &first[open_end..])
                };
                let new = insert_before_close(&fonts, &format!("{}fonts", self.p), &font)?;
                let new = set_attr(
                    &new[..new.find('>')? + 1],
                    "count",
                    &(list.len() + 1).to_string(),
                ) + &new[new.find('>')? + 1..];
                self.xml = format!("{}{new}{}", &self.xml[..fs], &self.xml[fe..]);
                self.dirty = true;
                list.len()
            }
        };
        let base = self.xfs.first().cloned().unwrap_or_else(|| {
            format!(
                "<{}xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>",
                self.p
            )
        });
        let xf = set_attr(
            &set_attr(&base, "fontId", &font_id.to_string()),
            "applyFont",
            "1",
        );
        let s = self.push_xf(xf)?;
        self.fmts.push(Fmt::General);
        Some(s)
    }

    fn push_xf(&mut self, xf: String) -> Option<u32> {
        let (s, e) = next_element(&self.xml, "cellXfs", 0)?;
        let body = &self.xml[s..e];
        let new = insert_before_close(body, &format!("{}cellXfs", self.p), &xf)?;
        let open_end = new.find('>')? + 1;
        let count = self.xfs.len() + 1;
        let new = format!(
            "{}{}",
            set_attr(&new[..open_end], "count", &count.to_string()),
            &new[open_end..]
        );
        self.xml = format!("{}{new}{}", &self.xml[..s], &self.xml[e..]);
        self.xfs.push(xf);
        self.dirty = true;
        Some((count - 1) as u32)
    }

    fn num_fmt_id(&mut self, fmt: Fmt) -> u32 {
        let code = match fmt {
            Fmt::General => return 0,
            Fmt::Percent(0) => return 9,
            Fmt::Percent(2) => return 10,
            Fmt::Decimals(0) => return 1,
            Fmt::Decimals(2) => return 2,
            Fmt::Grouped(0) => return 3,
            Fmt::Grouped(2) => return 4,
            Fmt::DateTime => return 22,
            Fmt::Date => "yyyy-mm-dd".to_string(),
            Fmt::Percent(d) => format!("0.{}%", "0".repeat(d as usize)),
            Fmt::Decimals(d) => format!("0.{}", "0".repeat(d as usize)),
            Fmt::Grouped(d) => format!("#,##0.{}", "0".repeat(d as usize)),
        };
        if let Some((id, _)) = self.codes.iter().find(|(_, c)| **c == code) {
            return *id;
        }
        let id = self.codes.keys().copied().max().unwrap_or(163).max(163) + 1;
        let el = format!(
            "<{}numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
            self.p,
            escape(&code)
        );
        if let Some((s, e)) = next_element(&self.xml, "numFmts", 0) {
            let body = &self.xml[s..e];
            let new = if body.ends_with("/>") {
                format!("<{p}numFmts count=\"1\">{el}</{p}numFmts>", p = self.p)
            } else {
                let new = insert_before_close(body, &format!("{}numFmts", self.p), &el)
                    .unwrap_or_else(|| body.to_string());
                let open_end = new.find('>').map_or(0, |i| i + 1);
                format!(
                    "{}{}",
                    set_attr(
                        &new[..open_end],
                        "count",
                        &(self.codes.len() + 1).to_string()
                    ),
                    &new[open_end..]
                )
            };
            self.xml = format!("{}{new}{}", &self.xml[..s], &self.xml[e..]);
        } else if let Some(open) = Lexer::new(&self.xml).find_map(|t| match t {
            Tok::Open { name, end, .. } if local(name) == "styleSheet" => Some(end),
            _ => None,
        }) {
            self.xml.insert_str(
                open,
                &format!("<{p}numFmts count=\"1\">{el}</{p}numFmts>", p = self.p),
            );
        }
        self.codes.insert(id, code);
        self.dirty = true;
        id
    }

    fn changed(&self) -> Option<(String, String)> {
        if !self.dirty {
            return None;
        }
        Some((self.part.clone()?, self.xml.clone()))
    }
}

/// How a number format shows numbers. Built-in ids first, then the format code.
fn fmt_of(id: u32, code: Option<&str>) -> Fmt {
    match id {
        1 => return Fmt::Decimals(0),
        2 => return Fmt::Decimals(2),
        3 | 37 | 38 => return Fmt::Grouped(0),
        4 | 39 | 40 => return Fmt::Grouped(2),
        9 => return Fmt::Percent(0),
        10 => return Fmt::Percent(2),
        14..=17 => return Fmt::Date,
        22 => return Fmt::DateTime,
        _ => {}
    }
    let Some(code) = code else {
        return Fmt::General;
    };
    // The positive section, without quoted text, escapes and [colour] or [locale] tags.
    let first = code.split(';').next().unwrap_or("");
    let mut clean = String::new();
    let mut chars = first.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '[' => {
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            c => clean.push(c.to_ascii_lowercase()),
        }
    }
    let decimals = clean
        .split_once('.')
        .map(|(_, d)| d.chars().take_while(|c| *c == '0' || *c == '#').count() as u8)
        .unwrap_or(0);
    if clean.contains('y') || clean.contains('d') {
        return if clean.contains('h') {
            Fmt::DateTime
        } else {
            Fmt::Date
        };
    }
    if clean.contains('%') {
        return Fmt::Percent(decimals);
    }
    if clean.contains('0') || clean.contains('#') {
        return if clean.contains(',') {
            Fmt::Grouped(decimals)
        } else {
            Fmt::Decimals(decimals)
        };
    }
    Fmt::General
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sheet_of(c: &Content, i: usize) -> &Sheet {
        let Content::Sheet(b) = c else { panic!() };
        &b.sheets[i]
    }

    #[test]
    fn creates_reads_and_edits_in_place() {
        let mut s = Sheet::new("Sales");
        crate::model::apply_sheet(&mut Workbook { sheets: vec![] }, &[]).unwrap();
        let mut book = Workbook {
            sheets: vec![Sheet::new("Sales")],
        };
        crate::model::apply_sheet(
            &mut book,
            &Edit::parse_all(&json!([{"cell": "A1", "rows": [["Region", "Q1", "Share"], ["North", 120, "60%"], ["South", 80, "40%"], ["Total", "=SUM(B2:B3)"]]}])).unwrap(),
        )
        .unwrap();
        s.cells = book.sheets[0].cells.clone();
        let bytes = Xlsx
            .create(&Content::Sheet(Workbook { sheets: vec![s] }))
            .unwrap();
        let read = Xlsx.read(&bytes).unwrap();
        let sh = sheet_of(&read, 0);
        assert_eq!(sh.name, "Sales");
        assert_eq!(sh.get(3, 1).unwrap().value, Value::Number(200.0));
        assert_eq!(sh.get(3, 1).unwrap().formula.as_deref(), Some("SUM(B2:B3)"));
        assert_eq!(
            sh.get(1, 2).unwrap().display(),
            "60%",
            "a typed percentage keeps its look"
        );

        let edits = Edit::parse_all(&json!([
            {"cell": "B2", "value": 150},
            {"add_sheet": "Summary", "rows": [["Best", "=Sales!A2"], ["Total", "=Sales!B4"]]}
        ]))
        .unwrap();
        let edited = Xlsx.edit(&bytes, &edits).unwrap();
        let read = Xlsx.read(&edited).unwrap();
        assert_eq!(
            sheet_of(&read, 0).get(3, 1).unwrap().value,
            Value::Number(230.0)
        );
        assert_eq!(
            sheet_of(&read, 1).get(1, 1).unwrap().value,
            Value::Number(230.0)
        );
        let pkg = Package::read(&edited).unwrap();
        let wb = pkg.text("xl/workbook.xml").unwrap();
        assert!(
            wb.contains("name=\"Summary\"") && wb.contains("fullCalcOnLoad=\"1\""),
            "{wb}"
        );
        assert!(pkg
            .text("[Content_Types].xml")
            .unwrap()
            .contains("/xl/worksheets/sheet2.xml"));

        let renamed = Xlsx
            .edit(
                &edited,
                &Edit::parse_all(&json!([{"rename_sheet": "Sales", "to": "Q1 Sales"}])).unwrap(),
            )
            .unwrap();
        let read = Xlsx.read(&renamed).unwrap();
        assert_eq!(
            sheet_of(&read, 1).get(1, 1).unwrap().formula.as_deref(),
            Some("'Q1 Sales'!B4")
        );
        let refused = Xlsx.edit(
            &renamed,
            &Edit::parse_all(&json!([{"delete_sheet": "Q1 Sales"}])).unwrap(),
        );
        assert!(refused.unwrap_err().to_string().contains("Summary!B1"));
    }

    /// A workbook as Excel writes it: shared strings, styles, a shared formula, a merged
    /// range and a styled empty cell. An edit changes one cell and nothing else.
    #[test]
    fn an_edit_keeps_what_it_does_not_touch() {
        let mut p = template();
        p.set("xl/workbook.xml", format!(r#"<?xml version="1.0"?><workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets><sheet name="Data" sheetId="1" r:id="rId9"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">Data!$A$1:$C$4</definedName></definedNames><calcPr calcId="1"/></workbook>"#));
        p.set("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/data.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/></Relationships>"#);
        p.set("xl/sharedStrings.xml", r#"<sst><si><t>Item</t></si><si><r><t>Pr</t></r><r><t>ice</t></r><rPh><t>x</t></rPh></si><si><t>Pen</t></si></sst>"#);
        p.set("xl/calcChain.xml", "<calcChain/>");
        p.set("xl/styles.xml", format!(r#"<styleSheet xmlns="{NS_MAIN}"><numFmts count="1"><numFmt numFmtId="164" formatCode="&quot;$&quot;#,##0.00"/></numFmts><fonts count="1"><font><sz val="11"/></font></fonts><cellXfs count="2"><xf numFmtId="0" fontId="0"/><xf numFmtId="164" fontId="0" applyNumberFormat="1"/></cellXfs></styleSheet>"#));
        let sheet = format!(
            r#"<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:C3"/><cols><col min="1" max="1" width="30" customWidth="1"/></cols><sheetData><row r="1" spans="1:3" ht="20" customHeight="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" s="1"/></row><row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2" s="1"><v>2.5</v></c><c r="C2"><f t="shared" ref="C2:C3" si="0">B2*2</f><v>5</v></c></row><row r="3"><c r="B3" s="1"><v>4</v></c><c r="C3"><f t="shared" si="0"/><v>8</v></c></row></sheetData><mergeCells count="1"><mergeCell ref="A3:A4"/></mergeCells></worksheet>"#
        );
        p.set("xl/worksheets/data.xml", sheet.clone());
        p.set(
            "[Content_Types].xml",
            p.text("[Content_Types].xml").unwrap().replace(
                "</Types>",
                "<Override PartName=\"/xl/calcChain.xml\" ContentType=\"x\"/></Types>",
            ),
        );
        let bytes = p.write().unwrap();

        let read = Xlsx.read(&bytes).unwrap();
        let sh = sheet_of(&read, 0);
        assert_eq!(
            sh.get(0, 1).unwrap().value,
            Value::Text("Price".into()),
            "phonetic runs are not text"
        );
        assert_eq!(sh.get(1, 1).unwrap().display(), "2.50");
        assert_eq!(
            sh.get(2, 2).unwrap().formula.as_deref(),
            Some("B3*2"),
            "a shared formula reaches its cells"
        );

        let out = Xlsx
            .edit(
                &bytes,
                &Edit::parse_all(&json!([{"cell": "B3", "value": 10}])).unwrap(),
            )
            .unwrap();
        let pkg = Package::read(&out).unwrap();
        let x = pkg.text("xl/worksheets/data.xml").unwrap();
        assert!(
            x.contains(r#"<row r="1" ht="20" customHeight="1"><c r="A1" t="s"><v>0</v></c>"#),
            "{x}"
        );
        assert!(
            x.contains(r#"<c r="C1" s="1"/>"#),
            "a styled empty cell stays"
        );
        assert!(
            x.contains(r#"<c r="B3" s="1"><v>10</v></c>"#),
            "the edited cell keeps its style: {x}"
        );
        assert!(
            x.contains(r#"<c r="C3"><f>B3*2</f><v>20</v></c>"#),
            "the shared group is written out: {x}"
        );
        assert!(x.contains(r#"<c r="C2"><f>B2*2</f><v>5</v></c>"#), "{x}");
        assert!(
            x.contains("<cols><col min=\"1\" max=\"1\" width=\"30\" customWidth=\"1\"/></cols>")
        );
        assert!(x.contains("<mergeCell ref=\"A3:A4\"/>"));
        assert!(!pkg.has("xl/calcChain.xml"));
        assert!(!pkg
            .text("[Content_Types].xml")
            .unwrap()
            .contains("calcChain"));
        assert!(pkg
            .text("xl/workbook.xml")
            .unwrap()
            .contains(r#"<calcPr calcId="1" fullCalcOnLoad="1"/>"#));
        assert_eq!(
            pkg.text("xl/sharedStrings.xml").unwrap(),
            p.text("xl/sharedStrings.xml").unwrap()
        );

        let renamed = Xlsx
            .edit(
                &bytes,
                &Edit::parse_all(&json!([{"rename_sheet": "Data", "to": "My Data"}])).unwrap(),
            )
            .unwrap();
        let wb = Package::read(&renamed)
            .unwrap()
            .text("xl/workbook.xml")
            .unwrap();
        assert!(wb.contains("'My Data'!$A$1:$C$4"), "{wb}");
    }

    #[test]
    fn number_formats() {
        assert_eq!(fmt_of(164, Some("\"$\"#,##0.00")), Fmt::Grouped(2));
        assert_eq!(fmt_of(165, Some("dd/mm/yyyy")), Fmt::Date);
        assert_eq!(fmt_of(166, Some("0.0%")), Fmt::Percent(1));
        assert_eq!(fmt_of(167, Some("[Red]0.000;-0")), Fmt::Decimals(3));
        assert_eq!(fmt_of(168, Some("@")), Fmt::General);
    }
}
