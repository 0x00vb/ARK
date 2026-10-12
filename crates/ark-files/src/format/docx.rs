//! Word documents (`.docx`). The body reads as blocks (headings, paragraphs, list items,
//! tables). An edit rewrites only the blocks it changes: every other block, and everything
//! that is not the body (headers, footers, images, comments, settings), is written back as it
//! was. A changed block keeps its paragraph's look; a new one takes the document's own
//! heading, list and table styles when it has them.

use std::collections::HashMap;

use crate::model::{
    apply_document, inline_runs, runs_markdown, Block, BlockKind, BlockLayout, Content, Document,
    DocumentLayout, Edit, Kind, SectionMargins,
};
use crate::package::{
    add_override, insert_before_close, next_rel_id, relationships, rels_of, resolve, Package,
};
use crate::xml::{self, attr, escape, local, Lexer, Tok};
use crate::{Error, Format};

pub struct Docx;

const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const CT_NUMBERING: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const REL_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const CT_STYLES: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";

#[derive(Clone, Debug, Default)]
struct LayoutPatch {
    line_spacing: Option<f64>,
    paragraph_before: Option<f64>,
    paragraph_after: Option<f64>,
    character_spacing: Option<f64>,
    margin_top: Option<f64>,
    margin_right: Option<f64>,
    margin_bottom: Option<f64>,
    margin_left: Option<f64>,
    section_margins: Option<Vec<SectionMargins>>,
}

impl LayoutPatch {
    fn from_edits(edits: &[Edit]) -> Self {
        let mut out = Self::default();
        for edit in edits {
            if let Edit::Layout {
                line_spacing,
                paragraph_before,
                paragraph_after,
                character_spacing,
                margin_top,
                margin_right,
                margin_bottom,
                margin_left,
                section_margins,
            } = edit
            {
                macro_rules! take {
                    ($field:ident) => {
                        if $field.is_some() {
                            out.$field = $field.clone();
                        }
                    };
                }
                take!(line_spacing);
                take!(paragraph_before);
                take!(paragraph_after);
                take!(character_spacing);
                take!(margin_top);
                take!(margin_right);
                take!(margin_bottom);
                take!(margin_left);
                take!(section_margins);
            }
        }
        out
    }

    fn paragraph(&self) -> bool {
        self.line_spacing.is_some()
            || self.paragraph_before.is_some()
            || self.paragraph_after.is_some()
    }

    fn margins(&self) -> bool {
        self.margin_top.is_some()
            || self.margin_right.is_some()
            || self.margin_bottom.is_some()
            || self.margin_left.is_some()
            || self.section_margins.is_some()
    }
}

impl Format for Docx {
    fn name(&self) -> &'static str {
        "docx"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["docx"]
    }
    fn kind(&self) -> Kind {
        Kind::Document
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        Ok(Content::Document(Doc::read(Package::read(bytes)?)?.model()))
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        let mut doc = Doc::read(Package::read(bytes)?)?;
        let mut model = doc.model();
        apply_document(&mut model, edits)?;
        doc.write(&model, &LayoutPatch::from_edits(edits))
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        let blocks = match content {
            Content::Document(d) => d.blocks.clone(),
            Content::Text(md) => crate::model::parse_markdown(md),
            _ => return Err(Error::Input("a document takes `text` (Markdown)".into())),
        };
        let blocks: Vec<Block> = blocks
            .into_iter()
            .map(|b| Block { origin: None, ..b })
            .collect();
        let mut doc = Doc::read(template())?;
        let document = Document {
            blocks,
            ..Document::default()
        };
        doc.write(&document, &LayoutPatch::default())
    }
}

// ---- reading -----------------------------------------------------------------------------------

struct Doc {
    pkg: Package,
    part: String,
    xml: String,
    /// The body's inner range.
    body: (usize, usize),
    els: Vec<El>,
    /// Bytes after the last block and before `sectPr` (or the body's end).
    tail: (usize, usize),
    sect: Option<(usize, usize)>,
    styles: Styles,
    lists: Lists,
}

struct El {
    /// Bytes between the previous block and this one (bookmarks, markers).
    lead: (usize, usize),
    range: (usize, usize),
    block: Block,
    /// `<w:pPr>…</w:pPr>`, or a table's `<w:tblPr>…</w:tblPr>`.
    props: Option<String>,
    /// The first run's `<w:rPr>…</w:rPr>`.
    run: Option<String>,
    /// A list marker typed as text (`\t•\t`), which a rewritten item keeps.
    marker: Option<String>,
}

impl Doc {
    fn read(pkg: Package) -> Result<Doc, Error> {
        let root = pkg.text_opt("_rels/.rels").unwrap_or_default();
        let part = relationships(&root)
            .into_iter()
            .find(|r| r.1.ends_with("/officeDocument"))
            .map(|r| resolve("", &r.2))
            .unwrap_or_else(|| "word/document.xml".into());
        let xml = pkg.text(&part)?;
        let rels = relationships(&pkg.text_opt(&rels_of(&part)).unwrap_or_default());
        let find = |kind: &str| {
            rels.iter()
                .find(|r| r.1.ends_with(kind))
                .map(|r| resolve(&part, &r.2))
        };
        let styles_part = find("/styles");
        let styles = Styles::read(
            styles_part.clone(),
            styles_part
                .as_ref()
                .and_then(|p| pkg.text_opt(p))
                .unwrap_or_default(),
        );
        let numbering_part = find("/numbering");
        let lists = Lists::read(
            numbering_part.clone(),
            numbering_part.and_then(|p| pkg.text_opt(&p)),
        );

        let (b0, b1) = Lexer::new(&xml)
            .find_map(|t| match t {
                Tok::Open {
                    name,
                    start,
                    empty: false,
                    ..
                } if local(name) == "body" => Some((start, xml::element_end(&xml, start))),
                _ => None,
            })
            .ok_or_else(|| Error::Format("the document has no body".into()))?;
        let inner_start = xml[b0..].find('>').map_or(b0, |i| b0 + i + 1);
        let inner_end = xml[..b1].rfind("</").unwrap_or(b1);

        let mut els = Vec::new();
        let mut pos = inner_start;
        let mut lead_start = inner_start;
        let mut sect = None;
        while pos < inner_end {
            let Some(t) = Lexer::at(&xml, pos).next() else {
                break;
            };
            if t.start() >= inner_end {
                break;
            }
            let (start, end, name) = match t {
                Tok::Open {
                    name,
                    start,
                    empty,
                    end,
                    ..
                } => (
                    start,
                    if empty {
                        end
                    } else {
                        xml::element_end(&xml, start)
                    },
                    name,
                ),
                other => {
                    pos = other.end();
                    continue;
                }
            };
            pos = end;
            let raw = &xml[start..end];
            let (mut block, props, run, marker) = match local(name) {
                "p" => paragraph(raw, &styles, &lists),
                "tbl" => (
                    Block::table(table_rows(raw, &styles, &lists)),
                    first_child(raw, "tblPr"),
                    None,
                    None,
                ),
                "sdt" => {
                    let mut b = Block::new(BlockKind::Other("contents".into()), "");
                    b.text = content_text(raw, &styles, &lists);
                    (b, None, None, None)
                }
                "sectPr" => {
                    sect = Some((start, end));
                    continue;
                }
                // Bookmarks and other markers ride with the next block.
                _ => continue,
            };
            block.layout = styles.block_layout(props.as_deref(), run.as_deref());
            els.push(El {
                lead: (lead_start, start),
                range: (start, end),
                block: Block {
                    origin: Some(els.len()),
                    ..block
                },
                props,
                run,
                marker,
            });
            lead_start = end;
        }
        let tail_end = sect.map_or(inner_end, |s| s.0);
        Ok(Doc {
            pkg,
            part,
            xml,
            body: (inner_start, inner_end),
            els,
            tail: (lead_start.min(tail_end), tail_end),
            sect,
            styles,
            lists,
        })
    }

    fn model(&self) -> Document {
        Document {
            blocks: self.els.iter().map(|e| e.block.clone()).collect(),
            layout: document_layout(&self.xml, &self.styles),
        }
    }

    fn write(&mut self, document: &Document, patch: &LayoutPatch) -> Result<Vec<u8>, Error> {
        let blocks = &document.blocks;
        let w = self.styles.w.clone();
        let mut out = String::new();
        let mut led = vec![false; self.els.len()];
        let lead = |out: &mut String, upto: usize, led: &mut Vec<bool>, els: &[El], xml: &str| {
            for i in 0..upto.min(els.len()) {
                if !led[i] {
                    led[i] = true;
                    out.push_str(&xml[els[i].lead.0..els[i].lead.1]);
                }
            }
        };
        for (n, b) in blocks.iter().enumerate() {
            match b.origin.filter(|o| *o < self.els.len()) {
                Some(o) => {
                    lead(&mut out, o + 1, &mut led, &self.els, &self.xml);
                    let el = &self.els[o];
                    if el.block.same(b) {
                        out.push_str(&self.xml[el.range.0..el.range.1]);
                    } else {
                        let same_kind = el.block.kind == b.kind;
                        let props = if same_kind { el.props.clone() } else { None };
                        let run = el.run.clone();
                        let marked;
                        let b = match el.marker.as_ref().filter(|_| same_kind) {
                            Some(m) => {
                                marked = Block {
                                    text: format!("{m}{}", b.text),
                                    ..b.clone()
                                };
                                &marked
                            }
                            None => b,
                        };
                        out.push_str(&self.render(b, props, run, blocks, n)?);
                    }
                }
                None => {
                    let xml = self.render(b, None, None, blocks, n)?;
                    out.push_str(&xml);
                }
            }
        }
        lead(&mut out, self.els.len(), &mut led, &self.els, &self.xml);
        out.push_str(&self.xml[self.tail.0..self.tail.1]);
        if let Some((s, e)) = self.sect {
            out.push_str(&self.xml[s..e]);
        }
        let _ = w;
        let mut xml = format!(
            "{}{out}{}",
            &self.xml[..self.body.0],
            &self.xml[self.body.1..]
        );
        xml = set_all_margins(&xml, patch, &document.layout);
        self.pkg.set(&self.part, xml);
        self.styles
            .set_defaults(patch, &document.layout, &mut self.pkg, &self.part);
        self.lists.save(&mut self.pkg, &self.part);
        self.pkg.write()
    }

    /// The XML of a new or changed block.
    fn render(
        &mut self,
        b: &Block,
        props: Option<String>,
        run: Option<String>,
        all: &[Block],
        n: usize,
    ) -> Result<String, Error> {
        let w = self.styles.w.clone();
        // A heading keeps its run's weight (a heading made bold by hand stays bold).
        let base = match (&b.kind, run.as_deref()) {
            (BlockKind::Heading(_), Some(r)) => inner(r),
            (_, Some(r)) => strip_marks(r),
            _ => String::new(),
        };
        Ok(match &b.kind {
            BlockKind::Table => self.table(b, props, &base),
            BlockKind::Other(_) => {
                // An image or contents cannot be written from text; it stays only unchanged.
                String::new()
            }
            kind => {
                let (ppr, extra) = match props {
                    Some(p) => (p, String::new()),
                    None => self.paragraph_props(kind, all, n),
                };
                let base = format!("{base}{extra}");
                format!("<{w}p>{ppr}{}</{w}p>", runs(&w, &b.text, &base))
            }
        })
    }

    /// `<w:pPr>` for a new block of `kind`, and run properties it needs when the document has
    /// no style for it.
    fn paragraph_props(&mut self, kind: &BlockKind, all: &[Block], n: usize) -> (String, String) {
        let w = self.styles.w.clone();
        match kind {
            BlockKind::Heading(level) => match self.styles.heading(*level) {
                Some(id) => (
                    format!("<{w}pPr><{w}pStyle {w}val=\"{}\"/></{w}pPr>", escape(&id)),
                    String::new(),
                ),
                None => {
                    let size = match level {
                        1 => 32,
                        2 => 28,
                        3 => 24,
                        _ => 22,
                    };
                    (
                        format!("<{w}pPr><{w}outlineLvl {w}val=\"{}\"/></{w}pPr>", level - 1),
                        format!("<{w}b/><{w}sz {w}val=\"{size}\"/>"),
                    )
                }
            },
            BlockKind::Bullet(level) | BlockKind::Numbered(level) => {
                let ordered = matches!(kind, BlockKind::Numbered(_));
                // A numbered list continues the list just before it, or starts its own.
                let prev_same = n > 0
                    && matches!(
                        (&all[n - 1].kind, ordered),
                        (BlockKind::Numbered(_), true) | (BlockKind::Bullet(_), false)
                    );
                let num = self.lists.num_for(ordered, prev_same, &mut self.pkg);
                let style = self
                    .styles
                    .by_name("list paragraph")
                    .map(|id| format!("<{w}pStyle {w}val=\"{}\"/>", escape(&id)))
                    .unwrap_or_default();
                match num {
                    Some(num) => (
                        format!(
                            "<{w}pPr>{style}<{w}numPr><{w}ilvl {w}val=\"{level}\"/><{w}numId {w}val=\"{num}\"/></{w}numPr></{w}pPr>"
                        ),
                        String::new(),
                    ),
                    None => (format!("<{w}pPr>{style}</{w}pPr>"), String::new()),
                }
            }
            _ => (String::new(), String::new()),
        }
    }

    fn table(&mut self, b: &Block, props: Option<String>, base: &str) -> String {
        let w = self.styles.w.clone();
        let cols = b.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let tblpr = props.unwrap_or_else(|| {
            let style = self
                .styles
                .by_name("table grid")
                .map(|id| format!("<{w}tblStyle {w}val=\"{}\"/>", escape(&id)))
                .unwrap_or_default();
            let line = |side: &str| format!("<{w}{side} {w}val=\"single\" {w}sz=\"4\" {w}space=\"0\" {w}color=\"auto\"/>");
            format!(
                "<{w}tblPr>{style}<{w}tblW {w}w=\"5000\" {w}type=\"pct\"/><{w}tblBorders>{}{}{}{}{}{}</{w}tblBorders></{w}tblPr>",
                line("top"),
                line("left"),
                line("bottom"),
                line("right"),
                line("insideH"),
                line("insideV")
            )
        });
        let width = 9000 / cols;
        let grid: String = (0..cols)
            .map(|_| format!("<{w}gridCol {w}w=\"{width}\"/>"))
            .collect();
        let mut out = format!("<{w}tbl>{tblpr}<{w}tblGrid>{grid}</{w}tblGrid>");
        for (i, row) in b.rows.iter().enumerate() {
            out.push_str(&format!("<{w}tr>"));
            for c in 0..cols {
                let text = row.get(c).map(String::as_str).unwrap_or("");
                let rpr = if i == 0 {
                    format!("{base}<{w}b/>")
                } else {
                    base.to_string()
                };
                out.push_str(&format!(
                    "<{w}tc><{w}tcPr><{w}tcW {w}w=\"{width}\" {w}type=\"dxa\"/></{w}tcPr><{w}p>{}</{w}p></{w}tc>",
                    runs(&w, text, &rpr)
                ));
            }
            out.push_str(&format!("</{w}tr>"));
        }
        out.push_str(&format!("</{w}tbl>"));
        out
    }
}

/// Runs for inline Markdown, on top of the run properties `base`.
fn runs(w: &str, text: &str, base: &str) -> String {
    let mut out = String::new();
    for (t, b, i) in inline_runs(text) {
        let mut rpr = String::new();
        // Schema order: style and fonts first, then bold and italic, then the rest.
        let (head, base_b, base_i, rest) = split_rpr(base);
        rpr.push_str(&head);
        if b || base_b {
            rpr.push_str(&format!("<{w}b/>"));
        }
        if i || base_i {
            rpr.push_str(&format!("<{w}i/>"));
        }
        rpr.push_str(&rest);
        let rpr = if rpr.is_empty() {
            String::new()
        } else {
            format!("<{w}rPr>{rpr}</{w}rPr>")
        };
        let mut body = String::new();
        let mut cur = String::new();
        let flush = |body: &mut String, cur: &mut String| {
            if !cur.is_empty() {
                body.push_str(&format!(
                    "<{w}t xml:space=\"preserve\">{}</{w}t>",
                    escape(cur)
                ));
                cur.clear();
            }
        };
        for ch in t.chars() {
            match ch {
                '\n' => {
                    flush(&mut body, &mut cur);
                    body.push_str(&format!("<{w}br/>"));
                }
                '\t' => {
                    flush(&mut body, &mut cur);
                    body.push_str(&format!("<{w}tab/>"));
                }
                c => cur.push(c),
            }
        }
        flush(&mut body, &mut cur);
        out.push_str(&format!("<{w}r>{rpr}{body}</{w}r>"));
    }
    out
}

/// The children of run properties: (style and fonts, everything after bold and italic).
fn split_rpr(base: &str) -> (String, bool, bool, String) {
    let mut head = String::new();
    let mut rest = String::new();
    let (mut b, mut i) = (false, false);
    let mut pos = 0;
    while pos < base.len() {
        let Some(Tok::Open {
            name,
            start,
            empty,
            end,
            ..
        }) = Lexer::at(base, pos).find(|t| matches!(t, Tok::Open { .. }))
        else {
            break;
        };
        let e = if empty {
            end
        } else {
            xml::element_end(base, start)
        };
        let raw = &base[start..e];
        match local(name) {
            "rStyle" | "rFonts" => head.push_str(raw),
            "b" => b = true,
            "i" => i = true,
            _ => rest.push_str(raw),
        }
        pos = e;
    }
    (head, b, i, rest)
}

/// The children of an element.
fn inner(el: &str) -> String {
    if el.ends_with("/>") && !el.contains("</") {
        return String::new();
    }
    let start = el.find('>').map_or(0, |i| i + 1);
    let end = el.rfind("</").unwrap_or(el.len()).max(start);
    el[start..end].to_string()
}

/// The children of `<w:rPr>` without bold, italic and their complex-script twins.
fn strip_marks(rpr: &str) -> String {
    let inner_start = rpr.find('>').map_or(0, |i| i + 1);
    let inner_end = rpr.rfind("</").unwrap_or(rpr.len());
    if rpr.ends_with("/>") && inner_start == rpr.len() {
        return String::new();
    }
    let inner = &rpr[inner_start..inner_end.max(inner_start)];
    let mut out = String::new();
    let mut pos = 0;
    while pos < inner.len() {
        let Some(Tok::Open {
            name,
            start,
            empty,
            end,
            ..
        }) = Lexer::at(inner, pos).find(|t| matches!(t, Tok::Open { .. }))
        else {
            break;
        };
        let e = if empty {
            end
        } else {
            xml::element_end(inner, start)
        };
        if !matches!(local(name), "b" | "bCs" | "i" | "iCs" | "rPrChange") {
            out.push_str(&inner[start..e]);
        }
        pos = e;
    }
    out
}

fn first_child(raw: &str, name: &str) -> Option<String> {
    let mut depth = 0usize;
    for t in Lexer::new(raw) {
        match t {
            Tok::Open {
                name: n,
                start,
                empty,
                end,
                ..
            } => {
                if depth == 1 && local(n) == name {
                    let e = if empty {
                        end
                    } else {
                        xml::element_end(raw, start)
                    };
                    return Some(raw[start..e].to_string());
                }
                if !empty {
                    depth += 1;
                }
            }
            Tok::Close { .. } => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn on(attrs: &str) -> bool {
    !matches!(
        xml::attrs(attrs)
            .into_iter()
            .find(|(k, _)| local(k) == "val")
            .map(|(_, v)| v),
        Some("0" | "false" | "off" | "none")
    )
}

/// A paragraph as a block, with its properties and first run's properties.
type Para = (Block, Option<String>, Option<String>, Option<String>);

/// A paragraph as a block, with its properties, its first run's properties, and a list marker
/// typed as text.
fn paragraph(raw: &str, styles: &Styles, lists: &Lists) -> Para {
    let props = first_child(raw, "pPr");
    let mut style = None;
    let mut ilvl = 0u8;
    let mut num = None;
    let mut outline = None;
    if let Some(p) = &props {
        for t in Lexer::new(p) {
            if let Tok::Open { name, attrs, .. } = t {
                let val = || {
                    xml::attrs(attrs)
                        .into_iter()
                        .find(|(k, _)| local(k) == "val")
                        .map(|(_, v)| xml::unescape(v).into_owned())
                };
                match local(name) {
                    "pStyle" => style = val(),
                    "ilvl" => ilvl = val().and_then(|v| v.parse().ok()).unwrap_or(0),
                    "numId" => num = val(),
                    "outlineLvl" => outline = val().and_then(|v| v.parse::<u8>().ok()),
                    "rPr" => break,
                    _ => {}
                }
            }
        }
    }
    let mut segs: Vec<(String, bool, bool)> = Vec::new();
    let mut first_run = None;
    let (mut b, mut i) = (false, false);
    let mut in_t = 0usize;
    let mut in_rpr = false;
    let mut image = false;
    let mut skip = 0usize;
    for t in Lexer::new(raw) {
        match t {
            Tok::Open {
                name,
                attrs,
                start,
                empty,
                ..
            } => match local(name) {
                "pPr" if !empty => skip += 1,
                "r" => {
                    b = false;
                    i = false;
                }
                "rPr" if skip == 0 => {
                    if first_run.is_none() {
                        first_run = Some(
                            raw[start..if empty {
                                t.end()
                            } else {
                                xml::element_end(raw, start)
                            }]
                                .to_string(),
                        );
                    }
                    in_rpr = !empty;
                }
                "b" if in_rpr => b = on(attrs),
                "i" if in_rpr => i = on(attrs),
                "t" if !empty && skip == 0 => in_t += 1,
                "tab" if skip == 0 && !in_rpr => segs.push(("\t".into(), b, i)),
                "br" | "cr" if skip == 0 => {
                    if attr(attrs, "w:type")
                        .as_deref()
                        .is_none_or(|t| t == "textWrapping")
                    {
                        segs.push(("\n".into(), b, i));
                    }
                }
                "drawing" | "pict" | "object" => image = true,
                _ => {}
            },
            Tok::Close { name, .. } => match local(name) {
                "pPr" => skip = skip.saturating_sub(1),
                "rPr" => in_rpr = false,
                "t" => in_t = in_t.saturating_sub(1),
                _ => {}
            },
            Tok::Text { raw: text, .. } if in_t > 0 => {
                segs.push((xml::unescape(text).into_owned(), b, i))
            }
            _ => {}
        }
    }
    // Merge neighbouring runs of the same marks.
    let mut merged: Vec<(String, bool, bool)> = Vec::new();
    for s in segs {
        match merged.last_mut() {
            Some(last) if last.1 == s.1 && last.2 == s.2 => last.0.push_str(&s.0),
            _ => merged.push(s),
        }
    }
    let text = runs_markdown(&merged);
    let kind = if text.trim().is_empty() && image {
        BlockKind::Other("image".into())
    } else if let Some(level) = style.as_deref().and_then(|s| styles.heading_level(s)) {
        BlockKind::Heading(level)
    } else if let (Some(n), true) = (num.as_deref(), num.as_deref() != Some("0")) {
        if lists.is_bullet(n, ilvl) {
            BlockKind::Bullet(ilvl)
        } else {
            BlockKind::Numbered(ilvl)
        }
    } else if let Some(k) = style.as_deref().and_then(|s| styles.list_kind(s)) {
        k
    } else if let Some(o) = outline.filter(|o| *o < 6) {
        BlockKind::Heading(o + 1)
    } else {
        BlockKind::Paragraph
    };
    let words: Vec<&(String, bool, bool)> =
        merged.iter().filter(|s| !s.0.trim().is_empty()).collect();
    let all_bold = !words.is_empty() && words.iter().all(|s| s.1);
    if let BlockKind::Heading(_) = kind {
        // A heading's weight is its look, not a mark in its text.
        if all_bold {
            let plain: String = merged.iter().map(|s| s.0.as_str()).collect();
            return (Block::new(kind, plain.trim()), props, first_run, None);
        }
    }
    if kind != BlockKind::Paragraph {
        return (Block::new(kind, text), props, first_run, None);
    }
    // Made by hand, without styles (as TextEdit, Pages and many people do): a list item that
    // starts with a typed marker, a heading that is short, bold and large.
    if let Some((marker, rest, ordered)) = typed_marker(&text) {
        let kind = if ordered {
            BlockKind::Numbered(0)
        } else {
            BlockKind::Bullet(0)
        };
        return (Block::new(kind, rest), props, first_run, Some(marker));
    }
    let size = first_run.as_deref().and_then(run_size).unwrap_or(0);
    if all_bold && size >= 28 && text.chars().count() <= 150 {
        let level = if size >= 40 {
            1
        } else if size >= 32 {
            2
        } else {
            3
        };
        let plain: String = merged.iter().map(|s| s.0.as_str()).collect();
        return (
            Block::new(BlockKind::Heading(level), plain.trim()),
            props,
            first_run,
            None,
        );
    }
    (
        Block::new(BlockKind::Paragraph, text),
        props,
        first_run,
        None,
    )
}

/// `\t•\tText` as (`\t•\t`, `Text`, numbered?).
fn typed_marker(text: &str) -> Option<(String, String, bool)> {
    let body = text.trim_start_matches([' ', '\t']);
    let lead = text.len() - body.len();
    let mut chars = body.char_indices();
    let (_, first) = chars.next()?;
    let after = if "•◦▪‣·–".contains(first) {
        first.len_utf8()
    } else {
        let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > 3 || !matches!(body[digits..].chars().next(), Some('.' | ')')) {
            return None;
        }
        digits + 1
    };
    let gap = body[after..].len() - body[after..].trim_start_matches([' ', '\t']).len();
    if gap == 0 || body[after + gap..].trim().is_empty() {
        return None;
    }
    let cut = lead + after + gap;
    Some((
        text[..cut].to_string(),
        text[cut..].to_string(),
        !"•◦▪‣·–".contains(first),
    ))
}

/// A run's font size in half-points (`<w:sz w:val="28"/>` is 14 pt).
fn run_size(rpr: &str) -> Option<u32> {
    Lexer::new(rpr).find_map(|t| match t {
        Tok::Open { name, attrs, .. } if local(name) == "sz" => xml::attrs(attrs)
            .into_iter()
            .find(|(k, _)| local(k) == "val")
            .and_then(|(_, v)| v.parse().ok()),
        _ => None,
    })
}

/// A table's rows of cell text (a cell's paragraphs joined by line breaks).
fn table_rows(raw: &str, styles: &Styles, lists: &Lists) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut pos = 0;
    let mut depth = 0usize;
    // Only the table's own rows: nested tables read as their cell's text.
    while let Some(t) = Lexer::at(raw, pos).next() {
        pos = t.end();
        match t {
            Tok::Open {
                name,
                start,
                empty: false,
                ..
            } => {
                if local(name) == "tr" && depth == 1 {
                    let end = xml::element_end(raw, start);
                    let row = &raw[start..end];
                    let mut cells = Vec::new();
                    let mut cp = 0;
                    let mut cd = 0usize;
                    while let Some(ct) = Lexer::at(row, cp).next() {
                        cp = ct.end();
                        match ct {
                            Tok::Open {
                                name,
                                start,
                                empty: false,
                                ..
                            } => {
                                if local(name) == "tc" && cd == 1 {
                                    let ce = xml::element_end(row, start);
                                    // Cells are plain text; a table's first row is drawn bold.
                                    cells.push(crate::model::plain(&content_text(
                                        &row[start..ce],
                                        styles,
                                        lists,
                                    )));
                                    cp = ce;
                                    continue;
                                }
                                cd += 1;
                            }
                            Tok::Close { .. } => cd = cd.saturating_sub(1),
                            _ => {}
                        }
                    }
                    rows.push(cells);
                    pos = end;
                    continue;
                }
                depth += 1;
            }
            Tok::Close { .. } => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    rows
}

/// The text of every paragraph inside `raw`, joined by line breaks.
fn content_text(raw: &str, styles: &Styles, lists: &Lists) -> String {
    let mut parts = Vec::new();
    let mut pos = 0;
    while let Some(t) = Lexer::at(raw, pos).next() {
        pos = t.end();
        if let Tok::Open {
            name,
            start,
            empty: false,
            ..
        } = t
        {
            if local(name) == "p" {
                let end = xml::element_end(raw, start);
                let (block, .., marker) = paragraph(&raw[start..end], styles, lists);
                parts.push(format!("{}{}", marker.unwrap_or_default(), block.text));
                pos = end;
            }
        }
    }
    parts.join("\n")
}

fn element_range(raw: &str, wanted: &str) -> Option<(usize, usize)> {
    Lexer::new(raw).find_map(|t| match t {
        Tok::Open {
            name,
            start,
            empty,
            end,
            ..
        } if local(name) == wanted => Some((
            start,
            if empty {
                end
            } else {
                xml::element_end(raw, start)
            },
        )),
        _ => None,
    })
}

fn element_named<'a>(raw: &'a str, wanted: &str) -> Option<&'a str> {
    let (a, b) = element_range(raw, wanted)?;
    Some(&raw[a..b])
}

fn paragraph_style(ppr: &str) -> Option<String> {
    Lexer::new(ppr).find_map(|t| match t {
        Tok::Open { name, attrs, .. } if local(name) == "pStyle" => xml::attrs(attrs)
            .into_iter()
            .find(|(k, _)| local(k) == "val")
            .map(|(_, v)| xml::unescape(v).into_owned()),
        _ => None,
    })
}

fn spacing_attrs(raw: &str) -> Option<HashMap<String, String>> {
    Lexer::new(raw).find_map(|t| match t {
        Tok::Open { name, attrs, .. } if local(name) == "spacing" => Some(
            xml::attrs(attrs)
                .into_iter()
                .map(|(k, v)| (local(k).to_string(), v.to_string()))
                .collect(),
        ),
        _ => None,
    })
}

fn apply_paragraph_spacing(
    raw: &str,
    layout: &mut DocumentLayout,
    flags: Option<&mut BlockLayout>,
) {
    let Some(a) = spacing_attrs(raw) else { return };
    let mut line_override = false;
    let mut before_override = false;
    let mut after_override = false;
    if let Some(v) = a.get("line").and_then(|v| v.parse::<f64>().ok()) {
        layout.line_spacing = v / 240.0;
        line_override = true;
    }
    if let Some(v) = a.get("before").and_then(|v| v.parse::<f64>().ok()) {
        layout.paragraph_before = v / 20.0;
        before_override = true;
    }
    if let Some(v) = a.get("after").and_then(|v| v.parse::<f64>().ok()) {
        layout.paragraph_after = v / 20.0;
        after_override = true;
    }
    if let Some(f) = flags {
        f.line_override |= line_override;
        f.before_override |= before_override;
        f.after_override |= after_override;
    }
}

fn apply_character_spacing(
    raw: &str,
    layout: &mut DocumentLayout,
    flags: Option<&mut BlockLayout>,
) {
    let Some(v) = spacing_attrs(raw).and_then(|a| a.get("val").and_then(|v| v.parse::<f64>().ok()))
    else {
        return;
    };
    layout.character_spacing = v / 20.0;
    if let Some(f) = flags {
        f.character_override = true;
    }
}

fn document_layout(xml: &str, styles: &Styles) -> DocumentLayout {
    let mut layout = styles.base.clone();
    let mut margins: Vec<[f64; 4]> = Vec::new();
    let mut at = 0;
    while let Some((a, b)) = Lexer::at(xml, at).find_map(|t| match t {
        Tok::Open {
            name,
            start,
            empty,
            end,
            ..
        } if local(name) == "sectPr" => Some((
            start,
            if empty {
                end
            } else {
                xml::element_end(xml, start)
            },
        )),
        _ => None,
    }) {
        if let Some(pg) = element_named(&xml[a..b], "pgMar") {
            let attrs = Lexer::new(pg)
                .find_map(|t| match t {
                    Tok::Open { attrs, .. } => Some(attrs),
                    _ => None,
                })
                .unwrap_or("");
            let get = |name: &str| {
                xml::attrs(attrs)
                    .into_iter()
                    .find(|(k, _)| local(k) == name)
                    .and_then(|(_, v)| v.parse::<f64>().ok())
                    .unwrap_or(1440.0)
                    / 1440.0
            };
            margins.push([get("top"), get("right"), get("bottom"), get("left")]);
        } else {
            margins.push([1.0; 4]);
        }
        at = b;
    }
    if margins.is_empty() {
        margins.push([1.0; 4]);
    }
    let same = |i: usize| {
        margins
            .first()
            .map(|m| m[i])
            .filter(|v| margins.iter().all(|m| (m[i] - *v).abs() < 0.0001))
    };
    layout.margin_top = same(0);
    layout.margin_right = same(1);
    layout.margin_bottom = same(2);
    layout.margin_left = same(3);
    layout.section_margins = margins
        .into_iter()
        .map(|m| SectionMargins {
            top: m[0],
            right: m[1],
            bottom: m[2],
            left: m[3],
        })
        .collect();
    layout
}

fn set_xml_attr(raw: &str, key: &str, value: &str) -> String {
    let needle = format!(" {key}=\"");
    if let Some(at) = raw.find(&needle) {
        let start = at + needle.len();
        if let Some(end) = raw[start..].find('"') {
            return format!("{}{}{}", &raw[..start], escape(value), &raw[start + end..]);
        }
    }
    let at = raw.find('>').unwrap_or(raw.len());
    let at = raw[..at].trim_end_matches('/').trim_end().len();
    format!("{} {key}=\"{}\"{}", &raw[..at], escape(value), &raw[at..])
}

fn set_child_value(raw: &str, prefix: &str, name: &str, value: i64) -> String {
    if let Some((a, b)) = element_range(raw, name) {
        let child = set_xml_attr(&raw[a..b], &format!("{prefix}val"), &value.to_string());
        return format!("{}{}{}", &raw[..a], child, &raw[b..]);
    }
    let child = format!("<{prefix}{name} {prefix}val=\"{value}\"/>");
    insert_xml_child(raw, &child)
}

fn insert_xml_child(raw: &str, child: &str) -> String {
    if raw.trim_end().ends_with("/>") {
        let trimmed = raw.trim_end();
        let whitespace = &raw[trimmed.len()..];
        let name = Lexer::new(trimmed)
            .find_map(|t| match t {
                Tok::Open { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .unwrap_or_default();
        return format!(
            "{}>{child}</{name}>{whitespace}",
            trimmed.trim_end_matches("/>")
        );
    }
    let close = raw.rfind("</").unwrap_or(raw.len());
    format!("{}{}{}", &raw[..close], child, &raw[close..])
}

fn prefix_for(raw: &str, element_name: &str) -> String {
    Lexer::new(raw)
        .find_map(|t| match t {
            Tok::Open { name, .. } if local(name) == element_name => {
                Some(name.strip_suffix(element_name).unwrap_or("w:").to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| "w:".into())
}

fn set_local_attr(raw: &str, prefix: &str, name: &str, value: &str) -> String {
    let open_end = raw.find('>').unwrap_or(raw.len());
    let attrs = Lexer::new(raw).find_map(|t| match t {
        Tok::Open { attrs, .. } => Some(attrs),
        _ => None,
    });
    if let Some((key, _)) = attrs.and_then(|attrs| {
        xml::attrs(attrs)
            .into_iter()
            .find(|(key, _)| local(key) == name)
    }) {
        return set_xml_attr(raw, key, value);
    }
    let key = format!("{prefix}{name}");
    let mut open = raw[..open_end].to_string();
    let insert = open.trim_end_matches('/').trim_end().len();
    open.insert_str(insert, &format!(" {key}=\"{}\"", escape(value)));
    format!("{}{}", open, &raw[open_end..])
}

fn set_spacing_element(ppr: &str, patch: &LayoutPatch, layout: &DocumentLayout) -> String {
    let prefix = Lexer::new(ppr)
        .find_map(|t| match t {
            Tok::Open { name, .. } if local(name) == "pPr" => {
                Some(name.strip_suffix("pPr").unwrap_or("w:").to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| "w:".into());
    let (mut spacing, range) = if let Some((a, b)) = element_range(ppr, "spacing") {
        (ppr[a..b].to_string(), Some((a, b)))
    } else {
        (format!("<{prefix}spacing/>"), None)
    };
    if patch.paragraph_before.is_some() {
        spacing = set_local_attr(
            &spacing,
            &prefix,
            "before",
            &((layout.paragraph_before * 20.0).round() as i64).to_string(),
        );
    }
    if patch.paragraph_after.is_some() {
        spacing = set_local_attr(
            &spacing,
            &prefix,
            "after",
            &((layout.paragraph_after * 20.0).round() as i64).to_string(),
        );
    }
    if patch.line_spacing.is_some() {
        spacing = set_local_attr(
            &spacing,
            &prefix,
            "line",
            &((layout.line_spacing * 240.0).round() as i64).to_string(),
        );
        spacing = set_local_attr(&spacing, &prefix, "lineRule", "auto");
    }
    if let Some((a, b)) = range {
        format!("{}{}{}", &ppr[..a], spacing, &ppr[b..])
    } else {
        insert_xml_child(ppr, &spacing)
    }
}

fn set_all_margins(xml: &str, patch: &LayoutPatch, layout: &DocumentLayout) -> String {
    if !patch.margins() {
        return xml.to_string();
    }
    let mut out = xml.to_string();
    if !Lexer::new(&out).any(|t| matches!(t, Tok::Open { name, .. } if local(name) == "sectPr")) {
        let prefix = prefix_for(&out, "document");
        let body = format!("{prefix}body");
        if let Some(next) = insert_before_close(&out, &body, &format!("<{prefix}sectPr/>")) {
            out = next;
        }
    }
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some((a, b)) = Lexer::at(&out, at).find_map(|t| match t {
        Tok::Open {
            name,
            start,
            empty,
            end,
            ..
        } if local(name) == "sectPr" => Some((
            start,
            if empty {
                end
            } else {
                xml::element_end(&out, start)
            },
        )),
        _ => None,
    }) {
        ranges.push((a, b));
        at = b;
    }
    let section_count = ranges.len();
    for (index, (a, b)) in ranges.into_iter().enumerate().rev() {
        let mut sect = out[a..b].to_string();
        let prefix = prefix_for(&sect, "sectPr");
        let (mut margin, range) = match element_range(&sect, "pgMar") {
            Some((m0, m1)) => (sect[m0..m1].to_string(), Some((m0, m1))),
            None => (format!("<{prefix}pgMar/>"), None),
        };
        let exact = patch
            .section_margins
            .as_ref()
            .filter(|values| values.len() == section_count)
            .and_then(|values| values.get(index));
        let values = [
            (
                "top",
                exact
                    .map(|m| m.top)
                    .or(patch.margin_top)
                    .or(layout.margin_top),
            ),
            (
                "right",
                exact
                    .map(|m| m.right)
                    .or(patch.margin_right)
                    .or(layout.margin_right),
            ),
            (
                "bottom",
                exact
                    .map(|m| m.bottom)
                    .or(patch.margin_bottom)
                    .or(layout.margin_bottom),
            ),
            (
                "left",
                exact
                    .map(|m| m.left)
                    .or(patch.margin_left)
                    .or(layout.margin_left),
            ),
        ];
        for (name, value) in values {
            if exact.is_some()
                || match name {
                    "top" => patch.margin_top.is_some(),
                    "right" => patch.margin_right.is_some(),
                    "bottom" => patch.margin_bottom.is_some(),
                    _ => patch.margin_left.is_some(),
                }
            {
                if let Some(value) = value {
                    margin = set_local_attr(
                        &margin,
                        &prefix,
                        name,
                        &((value * 1440.0).round() as i64).to_string(),
                    );
                }
            }
        }
        sect = if let Some((m0, m1)) = range {
            format!("{}{}{}", &sect[..m0], margin, &sect[m1..])
        } else {
            if sect.ends_with("/>") {
                let name = Lexer::new(&sect)
                    .find_map(|t| match t {
                        Tok::Open { name, .. } => Some(name.to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| format!("{prefix}sectPr"));
                format!("{}>{margin}</{name}>", sect.trim_end_matches("/>"))
            } else {
                let close = sect.rfind("</").unwrap_or(sect.len());
                format!("{}{}{}", &sect[..close], margin, &sect[close..])
            }
        };
        out = format!("{}{}{}", &out[..a], sect, &out[b..]);
    }
    out
}

// ---- styles and lists --------------------------------------------------------------------------

struct Styles {
    /// The prefix of the WordprocessingML namespace (`w:`).
    w: String,
    part: Option<String>,
    xml: String,
    base: DocumentLayout,
    by_id: HashMap<String, Style>,
}

struct Style {
    name: String,
    based: Option<String>,
    outline: Option<u8>,
    ppr: Option<String>,
    rpr: Option<String>,
}

impl Styles {
    fn read(part: Option<String>, xml: String) -> Styles {
        let w = Lexer::new(&xml)
            .find_map(|t| match t {
                Tok::Open { name, .. } if local(name) == "styles" => {
                    Some(name.strip_suffix("styles").unwrap_or("w:").to_string())
                }
                _ => None,
            })
            .unwrap_or_else(|| "w:".into());
        let mut by_id = HashMap::new();
        let mut pos = 0;
        while let Some(t) = Lexer::at(&xml, pos).next() {
            pos = t.end();
            let Tok::Open {
                name,
                attrs,
                start,
                empty: false,
                ..
            } = t
            else {
                continue;
            };
            if local(name) != "style" {
                continue;
            }
            let end = xml::element_end(&xml, start);
            let id = xml::attrs(attrs)
                .into_iter()
                .find(|(k, _)| local(k) == "styleId")
                .map(|(_, v)| xml::unescape(v).into_owned());
            let ty = xml::attrs(attrs)
                .into_iter()
                .find(|(k, _)| local(k) == "type")
                .map(|(_, v)| v.to_string());
            let body = &xml[start..end];
            let (mut sname, mut based, mut outline) = (String::new(), None, None);
            for t in Lexer::new(body) {
                if let Tok::Open { name, attrs, .. } = t {
                    let val = xml::attrs(attrs)
                        .into_iter()
                        .find(|(k, _)| local(k) == "val")
                        .map(|(_, v)| xml::unescape(v).into_owned());
                    match local(name) {
                        "name" => sname = val.unwrap_or_default().to_lowercase(),
                        "basedOn" => based = val,
                        "outlineLvl" => outline = val.and_then(|v| v.parse().ok()),
                        _ => {}
                    }
                }
            }
            if let (Some(id), Some("paragraph") | None) = (id, ty.as_deref()) {
                by_id.insert(
                    id,
                    Style {
                        name: sname,
                        based,
                        outline,
                        ppr: first_child(body, "pPr"),
                        rpr: first_child(body, "rPr"),
                    },
                );
            }
            pos = end;
        }
        let mut base = DocumentLayout::default();
        if let Some(defaults) = element_named(&xml, "docDefaults") {
            if let Some(ppr) =
                element_named(defaults, "pPrDefault").and_then(|x| element_named(x, "pPr"))
            {
                apply_paragraph_spacing(ppr, &mut base, None);
            }
            if let Some(rpr) =
                element_named(defaults, "rPrDefault").and_then(|x| element_named(x, "rPr"))
            {
                apply_character_spacing(rpr, &mut base, None);
            }
        }
        Styles {
            w,
            part,
            xml,
            base,
            by_id,
        }
    }

    fn heading_level(&self, id: &str) -> Option<u8> {
        let mut cur = Some(id.to_string());
        for _ in 0..6 {
            let style = self.by_id.get(cur.as_deref()?)?;
            if let Some(n) = style
                .name
                .strip_prefix("heading ")
                .and_then(|n| n.trim().parse::<u8>().ok())
            {
                return Some(n.clamp(1, 6));
            }
            if style.name == "title" {
                return Some(1);
            }
            if let Some(o) = style.outline.filter(|o| *o < 6) {
                return Some(o + 1);
            }
            cur = style.based.clone();
        }
        None
    }

    fn list_kind(&self, id: &str) -> Option<BlockKind> {
        let style = self.by_id.get(id)?;
        if style.name.starts_with("list bullet") {
            Some(BlockKind::Bullet(0))
        } else if style.name.starts_with("list number") {
            Some(BlockKind::Numbered(0))
        } else {
            None
        }
    }

    fn by_name(&self, name: &str) -> Option<String> {
        self.by_id
            .iter()
            .find(|(_, v)| v.name == name)
            .map(|(k, _)| k.clone())
    }

    fn heading(&self, level: u8) -> Option<String> {
        self.by_name(&format!("heading {level}"))
            .or_else(|| (level == 1).then(|| self.by_name("title")).flatten())
    }

    fn block_layout(&self, ppr: Option<&str>, rpr: Option<&str>) -> BlockLayout {
        let mut values = self.base.clone();
        let mut out = BlockLayout::default();
        let style_id = ppr.and_then(paragraph_style);
        let mut chain = Vec::new();
        let mut cur = style_id;
        for _ in 0..8 {
            let Some(id) = cur else { break };
            let Some(style) = self.by_id.get(&id) else {
                break;
            };
            chain.push(id);
            cur = style.based.clone();
        }
        for id in chain.into_iter().rev() {
            if let Some(style) = self.by_id.get(&id) {
                if let Some(p) = &style.ppr {
                    apply_paragraph_spacing(p, &mut values, Some(&mut out));
                }
                if let Some(r) = &style.rpr {
                    apply_character_spacing(r, &mut values, Some(&mut out));
                }
            }
        }
        if let Some(p) = ppr {
            apply_paragraph_spacing(p, &mut values, Some(&mut out));
        }
        if let Some(r) = rpr {
            apply_character_spacing(r, &mut values, Some(&mut out));
        }
        out.line_spacing = values.line_spacing;
        out.paragraph_before = values.paragraph_before;
        out.paragraph_after = values.paragraph_after;
        out.character_spacing = values.character_spacing;
        out
    }

    fn set_defaults(
        &mut self,
        patch: &LayoutPatch,
        layout: &DocumentLayout,
        pkg: &mut Package,
        document_part: &str,
    ) {
        if !patch.paragraph() && patch.character_spacing.is_none() {
            return;
        }
        let part = self.part.clone().unwrap_or_else(|| {
            document_part.rsplit_once('/').map_or_else(
                || "styles.xml".into(),
                |(dir, _)| format!("{dir}/styles.xml"),
            )
        });
        if self.xml.is_empty() {
            self.w = "w:".into();
            self.xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:styles xmlns:w=\"{NS_W}\"></w:styles>"
            );
        }
        let prefix = self.w.clone();
        if element_range(&self.xml, "docDefaults").is_none() {
            let defaults = format!("<{prefix}docDefaults></{prefix}docDefaults>");
            if let Some(next) =
                insert_before_close(&self.xml, &format!("{prefix}styles"), &defaults)
            {
                self.xml = next;
            }
        }
        let Some((d0, d1)) = element_range(&self.xml, "docDefaults") else {
            return;
        };
        let mut defaults = self.xml[d0..d1].to_string();
        if patch.paragraph() {
            if element_range(&defaults, "pPrDefault").is_none() {
                let child = format!("<{prefix}pPrDefault><{prefix}pPr/></{prefix}pPrDefault>");
                defaults = insert_xml_child(&defaults, &child);
            }
            if let Some((a, b)) = element_range(&defaults, "pPrDefault") {
                let mut wrapper = defaults[a..b].to_string();
                if element_range(&wrapper, "pPr").is_none() {
                    let child = format!("<{prefix}pPr/>");
                    wrapper = insert_xml_child(&wrapper, &child);
                }
                if let Some((p0, p1)) = element_range(&wrapper, "pPr") {
                    let ppr = set_spacing_element(&wrapper[p0..p1], patch, layout);
                    wrapper = format!("{}{}{}", &wrapper[..p0], ppr, &wrapper[p1..]);
                }
                defaults = format!("{}{}{}", &defaults[..a], wrapper, &defaults[b..]);
            }
        }
        if patch.character_spacing.is_some() {
            if element_range(&defaults, "rPrDefault").is_none() {
                let child = format!("<{prefix}rPrDefault><{prefix}rPr/></{prefix}rPrDefault>");
                defaults = insert_xml_child(&defaults, &child);
            }
            if let Some((a, b)) = element_range(&defaults, "rPrDefault") {
                let mut wrapper = defaults[a..b].to_string();
                if element_range(&wrapper, "rPr").is_none() {
                    let child = format!("<{prefix}rPr/>");
                    wrapper = insert_xml_child(&wrapper, &child);
                }
                if let Some((r0, r1)) = element_range(&wrapper, "rPr") {
                    let rpr = set_child_value(
                        &wrapper[r0..r1],
                        &prefix,
                        "spacing",
                        (layout.character_spacing * 20.0).round() as i64,
                    );
                    wrapper = format!("{}{}{}", &wrapper[..r0], rpr, &wrapper[r1..]);
                }
                defaults = format!("{}{}{}", &defaults[..a], wrapper, &defaults[b..]);
            }
        }
        self.xml = format!("{}{}{}", &self.xml[..d0], defaults, &self.xml[d1..]);
        pkg.set(&part, self.xml.clone());
        if self.part.is_none() {
            let rels_part = rels_of(document_part);
            let mut rels = pkg.text_opt(&rels_part).unwrap_or_else(|| {
                "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into()
            });
            let id = next_rel_id(&rels);
            let target = part
                .rsplit_once('/')
                .map_or(part.as_str(), |(_, file)| file);
            if let Some(next) = insert_before_close(
                &rels,
                "Relationships",
                &format!(
                    "<Relationship Id=\"{id}\" Type=\"{REL_STYLES}\" Target=\"{}\"/>",
                    escape(target)
                ),
            ) {
                rels = next;
            }
            pkg.set(&rels_part, rels);
            if let Some(types) = pkg.text_opt("[Content_Types].xml") {
                pkg.set(
                    "[Content_Types].xml",
                    add_override(&types, &part, CT_STYLES),
                );
            }
            self.part = Some(part);
        }
        self.base = layout.clone();
    }
}

/// The document's numbering: which list is bullets, and lists added for new items.
struct Lists {
    part: Option<String>,
    xml: Option<String>,
    /// numId to abstractNumId.
    nums: HashMap<String, String>,
    /// (abstractNumId, level) to "bullet" or another format.
    formats: HashMap<(String, u8), String>,
    /// The list new bullets use, and the one the last numbered list used.
    bullet: Option<String>,
    numbered: Option<String>,
    dirty: bool,
}

impl Lists {
    fn read(part: Option<String>, xml: Option<String>) -> Lists {
        let mut nums = HashMap::new();
        let mut formats = HashMap::new();
        if let Some(x) = &xml {
            let mut abs: Option<String> = None;
            let mut lvl = 0u8;
            let mut num: Option<String> = None;
            for t in Lexer::new(x) {
                if let Tok::Open { name, attrs, .. } = t {
                    let get = |k: &str| {
                        xml::attrs(attrs)
                            .into_iter()
                            .find(|(n, _)| local(n) == k)
                            .map(|(_, v)| v.to_string())
                    };
                    match local(name) {
                        "abstractNum" => abs = get("abstractNumId"),
                        "lvl" => lvl = get("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0),
                        "numFmt" => {
                            if let (Some(a), Some(f)) = (&abs, get("val")) {
                                formats.entry((a.clone(), lvl)).or_insert(f);
                            }
                        }
                        "num" => num = get("numId"),
                        "abstractNumId" => {
                            if let (Some(n), Some(a)) = (&num, get("val")) {
                                nums.insert(n.clone(), a);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Lists {
            part,
            xml,
            nums,
            formats,
            bullet: None,
            numbered: None,
            dirty: false,
        }
    }

    fn is_bullet(&self, num: &str, lvl: u8) -> bool {
        self.nums
            .get(num)
            .and_then(|a| {
                self.formats
                    .get(&(a.clone(), lvl))
                    .or_else(|| self.formats.get(&(a.clone(), 0)))
            })
            .is_none_or(|f| f == "bullet")
    }

    /// The numId for a new list item. Bullets share one list; a numbered item continues the
    /// numbered list right before it, or starts a new one at 1.
    fn num_for(&mut self, ordered: bool, continues: bool, _pkg: &mut Package) -> Option<String> {
        if !ordered {
            if self.bullet.is_none() {
                self.bullet = self
                    .nums
                    .iter()
                    .filter(|(n, _)| self.is_bullet(n, 0))
                    .map(|(n, _)| n.clone())
                    .min_by_key(|n| n.parse::<u32>().unwrap_or(u32::MAX))
                    .or_else(|| self.add(false));
            }
            return self.bullet.clone();
        }
        if continues {
            if let Some(n) = &self.numbered {
                return Some(n.clone());
            }
        }
        self.numbered = self.add(true);
        self.numbered.clone()
    }

    /// Add an abstract list and a list that uses it. Returns the new numId.
    fn add(&mut self, ordered: bool) -> Option<String> {
        let w = "w:";
        let xml = self.xml.get_or_insert_with(|| {
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="{NS_W}"></w:numbering>"#)
        });
        let next_abs = self
            .formats
            .keys()
            .filter_map(|(a, _)| a.parse::<u32>().ok())
            .max()
            .map_or(0, |m| m + 1)
            .max(
                self.nums
                    .values()
                    .filter_map(|a| a.parse::<u32>().ok())
                    .max()
                    .map_or(0, |m| m + 1),
            );
        let next_num = self
            .nums
            .keys()
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .map_or(1, |m| m + 1);
        let levels: String = (0..9)
            .map(|l| {
                let (fmt, text) = if ordered {
                    (["decimal", "lowerLetter", "lowerRoman"][l % 3], format!("%{}.", l + 1))
                } else {
                    ("bullet", ["•", "◦", "▪"][l % 3].to_string())
                };
                format!(
                    "<{w}lvl {w}ilvl=\"{l}\"><{w}start {w}val=\"1\"/><{w}numFmt {w}val=\"{fmt}\"/><{w}lvlText {w}val=\"{text}\"/><{w}lvlJc {w}val=\"left\"/><{w}pPr><{w}ind {w}left=\"{}\" {w}hanging=\"360\"/></{w}pPr></{w}lvl>",
                    720 * (l + 1)
                )
            })
            .collect();
        let abs = format!("<{w}abstractNum {w}abstractNumId=\"{next_abs}\"><{w}multiLevelType {w}val=\"hybridMultilevel\"/>{levels}</{w}abstractNum>");
        let num = format!(
            "<{w}num {w}numId=\"{next_num}\"><{w}abstractNumId {w}val=\"{next_abs}\"/></{w}num>"
        );
        // Abstract lists come before lists in the schema.
        let at = xml
            .find(&format!("<{w}num "))
            .or_else(|| xml.rfind(&format!("</{w}numbering>")))?;
        xml.insert_str(at, &abs);
        let new = insert_before_close(xml, &format!("{w}numbering"), &num)?;
        *xml = new;
        self.nums.insert(next_num.to_string(), next_abs.to_string());
        self.formats.insert(
            (next_abs.to_string(), 0),
            if ordered { "decimal" } else { "bullet" }.to_string(),
        );
        self.dirty = true;
        Some(next_num.to_string())
    }

    fn save(&mut self, pkg: &mut Package, doc_part: &str) {
        if !self.dirty {
            return;
        }
        let Some(xml) = &self.xml else { return };
        let part = match &self.part {
            Some(p) => p.clone(),
            None => {
                let dir = doc_part.rsplit_once('/').map_or("", |(d, _)| d);
                let part = format!("{dir}/numbering.xml");
                let rels_part = rels_of(doc_part);
                let rels = pkg.text_opt(&rels_part).unwrap_or_else(|| {
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#.into()
                });
                let id = next_rel_id(&rels);
                if let Some(r) = insert_before_close(
                    &rels,
                    "Relationships",
                    &format!("<Relationship Id=\"{id}\" Type=\"{REL_NUMBERING}\" Target=\"numbering.xml\"/>"),
                ) {
                    pkg.set(&rels_part, r);
                }
                if let Some(types) = pkg.text_opt("[Content_Types].xml") {
                    pkg.set(
                        "[Content_Types].xml",
                        add_override(&types, &part, CT_NUMBERING),
                    );
                }
                self.part = Some(part.clone());
                part
            }
        };
        pkg.set(&part, xml.clone());
        self.dirty = false;
    }
}

/// An empty document with styles for headings, lists and tables, on A4.
fn template() -> Package {
    let mut p = Package::new();
    p.set(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#,
    );
    p.set(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
    );
    p.set(
        "word/_rels/document.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
    );
    let heading = |id: &str, name: &str, level: u8, size: u32, before: u32| {
        format!(
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="{before}" w:after="80"/><w:outlineLvl w:val="{level}"/></w:pPr><w:rPr><w:b/><w:sz w:val="{size}"/></w:rPr></w:style>"#
        )
    };
    p.set(
        "word/styles.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{NS_W}"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="en-US"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="240"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:sz w:val="48"/></w:rPr></w:style>{}{}{}{}<w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="60"/><w:ind w:left="720"/><w:contextualSpacing/></w:pPr></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style></w:styles>"#,
            heading("Heading1", "heading 1", 0, 32, 360),
            heading("Heading2", "heading 2", 1, 28, 240),
            heading("Heading3", "heading 3", 2, 24, 200),
            heading("Heading4", "heading 4", 3, 22, 160),
        ),
    );
    p.set(
        "word/document.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{NS_W}" xmlns:r="{NS_R}"><w:body><w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
        ),
    );
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocks(bytes: &[u8]) -> Vec<Block> {
        match Docx.read(bytes).unwrap() {
            Content::Document(d) => d.blocks,
            _ => panic!(),
        }
    }

    #[test]
    fn creates_from_markdown_and_reads_it_back() {
        let md = "# Sales report\n\nRevenue grew **12%** in *Q1*.\n\n- North\n- South\n\n1. Hire\n2. Expand\n\n| Region | Q1 |\n|---|---|\n| North | 120 |";
        let bytes = Docx.create(&Content::Text(md.into())).unwrap();
        let b = blocks(&bytes);
        let md_back: Vec<String> = b.iter().map(Block::to_markdown).collect();
        assert_eq!(
            md_back,
            [
                "# Sales report",
                "Revenue grew **12%** in *Q1*.",
                "- North",
                "- South",
                "1. Hire",
                "1. Expand",
                "| Region | Q1 |\n| --- | --- |\n| North | 120 |"
            ]
        );
        let pkg = Package::read(&bytes).unwrap();
        assert!(pkg.has("word/numbering.xml"), "lists bring their numbering");
        assert!(pkg
            .text("[Content_Types].xml")
            .unwrap()
            .contains("/word/numbering.xml"));
        let doc = pkg.text("word/document.xml").unwrap();
        assert_eq!(
            doc.matches("<w:numId w:val=\"2\"/>").count(),
            2,
            "one numbered list: {doc}"
        );
    }

    /// A document as Word writes it: a localized heading style, a bookmark, an image, a
    /// tracked deletion and section properties. An edit changes one block and nothing else.
    #[test]
    fn an_edit_keeps_what_it_does_not_touch() {
        let mut p = template();
        p.set("word/styles.xml", format!(r#"<w:styles xmlns:w="{NS_W}"><w:style w:type="paragraph" w:styleId="Ttulo1"><w:name w:val="heading 1"/></w:style><w:style w:type="paragraph" w:styleId="Cita"><w:name w:val="Quote"/></w:style></w:styles>"#));
        let body = r#"<w:bookmarkStart w:id="0" w:name="top"/><w:p><w:pPr><w:pStyle w:val="Ttulo1"/><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>Informe</w:t></w:r></w:p><w:p w14:paraId="1"><w:pPr><w:pStyle w:val="Cita"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Georgia"/><w:b/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">Old </w:t></w:r><w:del><w:r><w:delText>gone</w:delText></w:r></w:del><w:r><w:t>text</w:t></w:r></w:p><w:p><w:r><w:drawing><wp:inline/></w:drawing></w:r></w:p><w:p/><w:p><w:r><w:t>End</w:t></w:r></w:p><w:bookmarkEnd w:id="0"/><w:sectPr><w:pgSz w:w="12240"/></w:sectPr>"#;
        p.set(
            "word/document.xml",
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body>{body}</w:body></w:document>"#),
        );
        let bytes = p.write().unwrap();
        let b = blocks(&bytes);
        let md: Vec<String> = b.iter().map(Block::to_markdown).collect();
        assert_eq!(md, ["# Informe", "**Old** text", "[image]", "", "End"]);

        let out = Docx.edit(&bytes, &Edit::parse_all(&json!([{"block": 2, "text": "New *text*"}, {"after": 5, "text": "# Next\n- a"}])).unwrap()).unwrap();
        let doc = Package::read(&out)
            .unwrap()
            .text("word/document.xml")
            .unwrap();
        assert!(doc.contains(r#"<w:bookmarkStart w:id="0" w:name="top"/><w:p><w:pPr><w:pStyle w:val="Ttulo1"/><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>Informe</w:t></w:r></w:p>"#), "{doc}");
        assert!(
            doc.contains(r#"<w:p><w:pPr><w:pStyle w:val="Cita"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Georgia"/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">New </w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Georgia"/><w:i/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">text</w:t></w:r></w:p>"#),
            "a changed paragraph keeps its style and font: {doc}"
        );
        assert!(doc.contains("<w:drawing><wp:inline/></w:drawing>"));
        assert!(
            doc.contains(
                r#"<w:pStyle w:val="Ttulo1"/></w:pPr><w:r><w:t xml:space="preserve">Next</w:t>"#
            ),
            "a new heading uses the document's own style: {doc}"
        );
        assert!(doc.ends_with(r#"<w:bookmarkEnd w:id="0"/><w:sectPr><w:pgSz w:w="12240"/></w:sectPr></w:body></w:document>"#), "{doc}");
        let md: Vec<String> = blocks(&out).iter().map(Block::to_markdown).collect();
        assert_eq!(
            md,
            [
                "# Informe",
                "New *text*",
                "[image]",
                "",
                "End",
                "# Next",
                "- a"
            ]
        );
        // Without a style for it, a heading is bold and large, in schema order.
        let out = Docx
            .edit(
                &bytes,
                &Edit::parse_all(&json!([{"after": 0, "text": "### Small"}])).unwrap(),
            )
            .unwrap();
        let doc = Package::read(&out)
            .unwrap()
            .text("word/document.xml")
            .unwrap();
        assert!(doc.contains(r#"<w:pPr><w:outlineLvl w:val="2"/></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="24"/></w:rPr>"#), "{doc}");
        assert_eq!(
            blocks(&out)[0].to_markdown(),
            "### Small",
            "reads back as written"
        );
    }

    /// TextEdit and Pages write no styles: a heading is large bold text, a list item starts
    /// with a typed bullet. Both read as what they look like, and a rewritten item keeps its
    /// typed bullet.
    #[test]
    fn hand_made_headings_and_bullets() {
        let mut p = template();
        let body = r#"<w:p><w:r><w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t>Report</w:t></w:r></w:p><w:p><w:pPr><w:ind w:left="720"/></w:pPr><w:r><w:t></w:t><w:tab/><w:t>•</w:t><w:tab/></w:r><w:r><w:t>North grew</w:t></w:r></w:p><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold but small</w:t></w:r></w:p>"#;
        p.set(
            "word/document.xml",
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body>{body}</w:body></w:document>"#),
        );
        let bytes = p.write().unwrap();
        let md: Vec<String> = blocks(&bytes).iter().map(Block::to_markdown).collect();
        assert_eq!(md, ["# Report", "- North grew", "**Bold but small**"]);
        let out = Docx.edit(&bytes, &Edit::parse_all(&json!([{"block": 2, "text": "- South fell"}, {"block": 1, "text": "# Q1 report"}])).unwrap()).unwrap();
        let doc = Package::read(&out)
            .unwrap()
            .text("word/document.xml")
            .unwrap();
        assert!(doc.contains(r#"<w:pPr><w:ind w:left="720"/></w:pPr><w:r><w:tab/><w:t xml:space="preserve">•</w:t><w:tab/><w:t xml:space="preserve">South fell</w:t></w:r>"#), "{doc}");
        assert!(doc.contains(r#"<w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t xml:space="preserve">Q1 report</w:t>"#), "a heading keeps its weight: {doc}");
    }

    #[test]
    fn document_layout_round_trips_without_erasing_direct_formatting() {
        let mut p = template();
        let doc = p.text("word/document.xml").unwrap().replace("<w:body>", "<w:body><w:p><w:pPr><w:spacing w:line=\"480\" w:after=\"40\"/></w:pPr><w:r><w:rPr><w:spacing w:val=\"10\"/></w:rPr><w:t>Direct</w:t></w:r></w:p>");
        p.set("word/document.xml", doc);
        p.set("word/header1.xml", b"header stays exact".to_vec());
        let bytes = p.write().unwrap();
        let edits = Edit::parse_all(&json!([{"layout":{"line_spacing":1.5,"paragraph_before":6,"paragraph_after":12,"character_spacing":0.5,"margin_top":0.75,"margin_right":1.25,"margin_bottom":0.8,"margin_left":1.1}}])).unwrap();
        let out = Docx.edit(&bytes, &edits).unwrap();
        let pkg = Package::read(&out).unwrap();
        assert_eq!(
            pkg.get("word/header1.xml"),
            Some(b"header stays exact".as_slice())
        );
        let xml = pkg.text("word/document.xml").unwrap();
        assert!(
            xml.contains(r#"<w:pPr><w:spacing w:line="480" w:after="40"/></w:pPr>"#),
            "{xml}"
        );
        assert!(xml.contains(r#"w:top="1080""#), "{xml}");
        assert!(xml.contains(r#"w:right="1800""#), "{xml}");
        let Content::Document(read) = Docx.read(&out).unwrap() else {
            panic!()
        };
        assert_eq!(read.layout.line_spacing, 1.5);
        assert_eq!(read.layout.paragraph_before, 6.0);
        assert_eq!(read.layout.paragraph_after, 12.0);
        assert_eq!(read.layout.character_spacing, 0.5);
        assert_eq!(read.layout.margin_left, Some(1.1));
        assert_eq!(read.blocks[0].layout.line_spacing, 2.0);
        assert!(read.blocks[0].layout.line_override);
        assert_eq!(read.blocks[0].layout.character_spacing, 0.5);
        assert!(read.blocks[0].layout.character_override);
    }

    #[test]
    fn layout_creates_missing_word_defaults_and_margins() {
        let mut p = Package::new();
        p.set(
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        );
        p.set(
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        );
        p.set(
            "word/document.xml",
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body><w:p><w:r><w:t>Plain</w:t></w:r></w:p></w:body></w:document>"#),
        );
        p.set("word/media/image1.png", b"image exact".to_vec());
        p.set("word/comments.xml", b"comments exact".to_vec());
        let input = p.write().unwrap();
        let edits = Edit::parse_all(&json!([{
            "layout": {"line_spacing": 1.4, "character_spacing": 0.3, "margin_left": 1.25}
        }]))
        .unwrap();
        let output = Docx.edit(&input, &edits).unwrap();
        let package = Package::read(&output).unwrap();
        let styles = package.text("word/styles.xml").unwrap();
        assert!(styles.contains(r#"w:line="336""#), "{styles}");
        assert!(styles.contains(r#"w:lineRule="auto""#), "{styles}");
        assert!(styles.contains(r#"w:val="6""#), "{styles}");
        assert!(package
            .text("word/_rels/document.xml.rels")
            .unwrap()
            .contains(REL_STYLES));
        assert!(package
            .text("[Content_Types].xml")
            .unwrap()
            .contains("/word/styles.xml"));
        let document = package.text("word/document.xml").unwrap();
        assert!(document.contains(r#"w:left="1800""#), "{document}");
        assert_eq!(
            package.get("word/media/image1.png"),
            Some(b"image exact".as_slice())
        );
        assert_eq!(
            package.get("word/comments.xml"),
            Some(b"comments exact".as_slice())
        );
    }

    #[test]
    fn margin_only_preserves_spacing_and_mixed_sections_can_be_restored() {
        let mut p = template();
        p.set(
            "word/styles.xml",
            format!(r#"<w:styles xmlns:w="{NS_W}"><w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:line="480" w:lineRule="exact" w:beforeAutospacing="1"/></w:pPr></w:pPrDefault></w:docDefaults></w:styles>"#),
        );
        p.set(
            "word/document.xml",
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body><w:p><w:pPr><w:sectPr><w:pgMar w:top="720" w:right="1440" w:bottom="1440" w:left="720"/></w:sectPr></w:pPr><w:r><w:t>One</w:t></w:r></w:p><w:sectPr><w:pgMar w:top="2160" w:right="1440" w:bottom="1440" w:left="2160"/></w:sectPr></w:body></w:document>"#),
        );
        let input = p.write().unwrap();
        let Content::Document(original) = Docx.read(&input).unwrap() else {
            panic!()
        };
        assert_eq!(original.layout.margin_top, None);
        assert_eq!(original.layout.section_margins.len(), 2);

        let reset = Docx
            .edit(
                &input,
                &Edit::parse_all(&json!([{"layout":{"margin_top":1,"margin_right":1,"margin_bottom":1,"margin_left":1}}])).unwrap(),
            )
            .unwrap();
        let reset_package = Package::read(&reset).unwrap();
        assert_eq!(
            reset_package.text("word/styles.xml").unwrap(),
            p.text("word/styles.xml").unwrap(),
            "a margin edit must not normalize paragraph spacing"
        );
        let sections: Vec<_> = original
            .layout
            .section_margins
            .iter()
            .map(|m| {
                json!({
                    "top": m.top, "right": m.right, "bottom": m.bottom, "left": m.left
                })
            })
            .collect();
        let restored = Docx
            .edit(
                &reset,
                &Edit::parse_all(&json!([{"layout":{"section_margins":sections}}])).unwrap(),
            )
            .unwrap();
        let Content::Document(restored) = Docx.read(&restored).unwrap() else {
            panic!()
        };
        assert_eq!(
            restored.layout.section_margins,
            original.layout.section_margins
        );
        assert_eq!(restored.layout.margin_top, None);
    }

    #[test]
    fn named_styles_inherit_spacing() {
        let mut p = template();
        p.set(
            "word/styles.xml",
            format!(r#"<w:styles xmlns:w="{NS_W}"><w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:line="276" w:after="160"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:styleId="Base"><w:name w:val="Base"/><w:pPr><w:spacing w:line="360"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Child"><w:name w:val="Child"/><w:basedOn w:val="Base"/><w:pPr><w:spacing w:after="200"/></w:pPr></w:style></w:styles>"#),
        );
        p.set(
            "word/document.xml",
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body><w:p><w:pPr><w:pStyle w:val="Child"/></w:pPr><w:r><w:t>Inherited</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#),
        );
        let Content::Document(document) = Docx.read(&p.write().unwrap()).unwrap() else {
            panic!()
        };
        assert_eq!(document.blocks[0].layout.line_spacing, 1.5);
        assert_eq!(document.blocks[0].layout.paragraph_after, 10.0);
        assert!(document.blocks[0].layout.line_override);
        assert!(document.blocks[0].layout.after_override);
    }
}
