//! Diagrams as artifacts. An agent describes structure (a sequence, the classes of a model,
//! the states of a process, how components or agents connect) as a diagram it can store,
//! version, hand to another agent and have rendered elsewhere, instead of prose.
//!
//! The format is [Mermaid](https://mermaid.js.org) text: portable, diffable, and rendered by
//! common tools (Markdown viewers, documentation sites, the Mermaid CLI). ARK does not render
//! it. [`validate`] checks the structure ARK can check without a renderer (the header matches
//! the kind, blocks are closed, brackets and quotes balance, each line has a shape the
//! diagram type accepts) and reports problems by line, so an agent can fix its own output.
//! It is a structural check, not the full Mermaid grammar: a diagram that passes can still
//! use a construct a given renderer does not support.
//!
//! A stored diagram is an artifact of kind [`DIAGRAM_KIND`]. Its source starts with a
//! Mermaid comment that records what ARK knows about it ([`Diagram::header`]), so the file
//! stays self-describing outside ARK.

use serde::{Deserialize, Serialize};

/// The artifact kind tag of a stored diagram.
pub const DIAGRAM_KIND: &str = "diagram";

/// What a diagram shows. Several kinds share a Mermaid type: a component, dependency,
/// architecture or orchestration diagram is a Mermaid flowchart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagramKind {
    Flowchart,
    Sequence,
    Class,
    State,
    /// Entities and relationships.
    Er,
    Component,
    Dependency,
    Architecture,
    /// Agents, tasks and delegation.
    Orchestration,
}

impl DiagramKind {
    pub const ALL: &'static [DiagramKind] = &[
        DiagramKind::Flowchart,
        DiagramKind::Sequence,
        DiagramKind::Class,
        DiagramKind::State,
        DiagramKind::Er,
        DiagramKind::Component,
        DiagramKind::Dependency,
        DiagramKind::Architecture,
        DiagramKind::Orchestration,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DiagramKind::Flowchart => "flowchart",
            DiagramKind::Sequence => "sequence",
            DiagramKind::Class => "class",
            DiagramKind::State => "state",
            DiagramKind::Er => "er",
            DiagramKind::Component => "component",
            DiagramKind::Dependency => "dependency",
            DiagramKind::Architecture => "architecture",
            DiagramKind::Orchestration => "orchestration",
        }
    }

    pub fn parse(s: &str) -> Option<DiagramKind> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.trim_end_matches(" diagram");
        DiagramKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// The Mermaid diagram type this kind is written in.
    pub fn mermaid(self) -> MermaidType {
        match self {
            DiagramKind::Sequence => MermaidType::Sequence,
            DiagramKind::Class => MermaidType::Class,
            DiagramKind::State => MermaidType::State,
            DiagramKind::Er => MermaidType::Er,
            _ => MermaidType::Flowchart,
        }
    }
}

/// The Mermaid diagram types ARK validates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MermaidType {
    Flowchart,
    Sequence,
    Class,
    State,
    Er,
}

impl MermaidType {
    fn of_header(line: &str) -> Option<MermaidType> {
        let word = line.split_whitespace().next().unwrap_or("");
        match word {
            "flowchart" | "graph" => Some(MermaidType::Flowchart),
            "sequenceDiagram" => Some(MermaidType::Sequence),
            "classDiagram" | "classDiagram-v2" => Some(MermaidType::Class),
            "stateDiagram" | "stateDiagram-v2" => Some(MermaidType::State),
            "erDiagram" => Some(MermaidType::Er),
            _ => None,
        }
    }

    fn header(self) -> &'static str {
        match self {
            MermaidType::Flowchart => "flowchart",
            MermaidType::Sequence => "sequenceDiagram",
            MermaidType::Class => "classDiagram",
            MermaidType::State => "stateDiagram-v2",
            MermaidType::Er => "erDiagram",
        }
    }
}

/// A problem found by [`validate`]. `line` is 1-based in the source as given (0: the whole
/// diagram).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramIssue {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for DiagramIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

/// A diagram: its kind, a title, and Mermaid source (without ARK's header comment).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagram {
    pub kind: DiagramKind,
    pub title: String,
    pub source: String,
    /// The artifact this one replaces, when it is a revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revises: Option<String>,
    /// 1 for a new diagram, +1 per revision.
    #[serde(default = "one")]
    pub version: u32,
}

fn one() -> u32 {
    1
}

/// The largest diagram source accepted, in bytes.
pub const MAX_SOURCE: usize = 64 * 1024;

const HEADER_PREFIX: &str = "%% ark-diagram ";

impl Diagram {
    pub fn new(kind: DiagramKind, title: impl Into<String>, source: impl Into<String>) -> Self {
        Diagram {
            kind,
            title: title.into(),
            source: source.into(),
            revises: None,
            version: 1,
        }
    }

    /// The comment line that records kind, title, version and lineage in the stored file.
    pub fn header(&self) -> String {
        let meta = serde_json::json!({
            "kind": self.kind,
            "title": self.title,
            "version": self.version,
            "revises": self.revises,
        });
        format!("{HEADER_PREFIX}{meta}")
    }

    /// The stored text: the header comment, then the source.
    pub fn to_text(&self) -> String {
        format!("{}\n{}\n", self.header(), self.source.trim_end())
    }

    /// Read a stored diagram back. Text without ARK's header is accepted when its Mermaid
    /// header names a type (the kind is then the type's own: flowchart, sequence, ...).
    pub fn from_text(text: &str) -> Option<Diagram> {
        let mut lines = text.lines();
        let first = lines.clone().next()?.trim();
        if let Some(meta) = first.strip_prefix(HEADER_PREFIX) {
            let v: serde_json::Value = serde_json::from_str(meta).ok()?;
            lines.next();
            return Some(Diagram {
                kind: serde_json::from_value(v["kind"].clone()).ok()?,
                title: v["title"].as_str().unwrap_or("").to_string(),
                source: lines.collect::<Vec<_>>().join("\n"),
                revises: v["revises"].as_str().map(str::to_string),
                version: v["version"].as_u64().unwrap_or(1) as u32,
            });
        }
        let header = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
        let kind = match MermaidType::of_header(header)? {
            MermaidType::Flowchart => DiagramKind::Flowchart,
            MermaidType::Sequence => DiagramKind::Sequence,
            MermaidType::Class => DiagramKind::Class,
            MermaidType::State => DiagramKind::State,
            MermaidType::Er => DiagramKind::Er,
        };
        Some(Diagram::new(kind, "", text.trim_end()))
    }

    pub fn validate(&self) -> Result<(), Vec<DiagramIssue>> {
        validate(self.kind, &self.source)
    }
}

/// Check a diagram's Mermaid source against its kind. `Ok` when no problem was found.
pub fn validate(kind: DiagramKind, source: &str) -> Result<(), Vec<DiagramIssue>> {
    let mut issues = Vec::new();
    let issue = |issues: &mut Vec<DiagramIssue>, line: usize, m: String| {
        issues.push(DiagramIssue { line, message: m })
    };
    if source.len() > MAX_SOURCE {
        issue(
            &mut issues,
            0,
            format!(
                "the source is {} bytes; the limit is {MAX_SOURCE}",
                source.len()
            ),
        );
        return Err(issues);
    }
    let lines: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with("%%"))
        .collect();
    let Some(&(hline, header)) = lines.first() else {
        issue(&mut issues, 0, "the diagram is empty".into());
        return Err(issues);
    };
    let want = kind.mermaid();
    match MermaidType::of_header(header) {
        None => issue(
            &mut issues,
            hline,
            format!(
                "the first line must declare the diagram type (`{}` for a {} diagram)",
                want.header(),
                kind.as_str()
            ),
        ),
        Some(t) if t != want => issue(
            &mut issues,
            hline,
            format!(
                "a {} diagram is written as `{}`, not `{}`",
                kind.as_str(),
                want.header(),
                header.split_whitespace().next().unwrap_or("")
            ),
        ),
        Some(_) => {}
    }
    if want == MermaidType::Flowchart {
        let dir = header.split_whitespace().nth(1);
        if let Some(d) = dir {
            if !["TB", "TD", "BT", "RL", "LR"].contains(&d) {
                issue(
                    &mut issues,
                    hline,
                    format!("unknown direction `{d}` (use TB, TD, BT, RL or LR)"),
                );
            }
        }
    }
    if lines.len() < 2 {
        issue(
            &mut issues,
            0,
            "the diagram has a header and nothing else".into(),
        );
    }
    let mut blocks: Vec<(usize, &str)> = Vec::new();
    for &(n, line) in lines.iter().skip(1) {
        // An ER relationship's cardinality (`||--o{`) uses braces that are not blocks.
        let er_relation =
            want == MermaidType::Er && line.split_whitespace().nth(1).is_some_and(is_er_relation);
        // A lone `}` closes a block; the block checks below match it.
        if let Some(m) = unbalanced(line).filter(|_| !er_relation && line != "}") {
            issue(&mut issues, n, m);
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("javascript:")
            || (lower.starts_with("click ") && lower.contains(" call "))
        {
            issue(
                &mut issues,
                n,
                "interactive callbacks are not allowed in a stored diagram".into(),
            );
            continue;
        }
        let r = match want {
            MermaidType::Flowchart => flowchart_line(line, &mut blocks, n),
            MermaidType::Sequence => sequence_line(line, &mut blocks, n),
            MermaidType::Class => class_line(line, &mut blocks, n),
            MermaidType::State => state_line(line, &mut blocks, n),
            MermaidType::Er => er_line(line, &mut blocks, n),
        };
        if let Err(m) = r {
            issue(&mut issues, n, m);
        }
    }
    for (n, what) in blocks {
        issue(&mut issues, n, format!("`{what}` is never closed"));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Brackets and double quotes on one line. Text inside quotes is not counted.
fn unbalanced(line: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut quoted = false;
    for c in line.chars() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                let open = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.pop() != Some(open) {
                    return Some(format!("unexpected `{c}`"));
                }
            }
            _ => {}
        }
    }
    if quoted {
        return Some("a quote is not closed".into());
    }
    // An open brace at the end starts a block (class, state, entity); anything else open is
    // an error.
    match stack.as_slice() {
        [] | ['{'] if line.ends_with('{') => None,
        [] => None,
        rest => Some(format!("`{}` is not closed", rest[rest.len() - 1])),
    }
}

fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '$' | '*' | '~')
        })
}

/// The identifier a node reference starts with: `A`, `A[label]`, `A((x))`, `A:::cls`.
fn node_id(part: &str) -> &str {
    let part = part.trim();
    let end = part
        .find(['[', '(', '{', '>', ' ', '"'])
        .unwrap_or(part.len());
    let id = &part[..end];
    id.split(":::").next().unwrap_or(id)
}

const FLOW_ARROWS: &[&str] = &[
    "<-->", "<-.->", "<==>", "-.->", "-.-", "--->", "-->", "---", "==>", "===", "--o", "--x",
    "o--o", "x--x", "~~~",
];

/// Remove the text of quoted strings and bracketed labels so arrows inside labels do not
/// count.
fn strip_labels(line: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    for c in line.chars() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match c {
            '[' | '(' | '{' => {
                depth += 1;
                if depth == 1 {
                    out.push(c);
                }
            }
            ']' | ')' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push(c);
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn flowchart_line<'a>(
    line: &'a str,
    blocks: &mut Vec<(usize, &'a str)>,
    n: usize,
) -> Result<(), String> {
    match first_word(line) {
        "subgraph" => {
            if line.split_whitespace().nth(1).is_none() {
                return Err("`subgraph` needs an id or a title".into());
            }
            blocks.push((n, "subgraph"));
            return Ok(());
        }
        "end" => {
            return match blocks.pop() {
                Some(_) => Ok(()),
                None => Err("`end` without a `subgraph`".into()),
            };
        }
        "direction" | "classDef" | "class" | "style" | "linkStyle" | "click" | "accTitle:"
        | "accDescr:" | "title" => return Ok(()),
        _ => {}
    }
    let bare = strip_labels(line);
    // `A -- text --> B` and `A -->|text| B`: drop the edge text before splitting.
    let bare = drop_pipe_text(&bare);
    let mut parts: Vec<String> = vec![bare.clone()];
    for arrow in FLOW_ARROWS {
        parts = parts
            .iter()
            .flat_map(|p| p.split(arrow).map(str::to_string).collect::<Vec<_>>())
            .collect();
    }
    let edge = parts.len() > 1;
    if !edge {
        let id = node_id(&bare);
        return if is_ident(id) {
            Ok(())
        } else {
            Err(format!(
                "expected a node, an edge or a statement, found `{line}`"
            ))
        };
    }
    let parts: Vec<String> = parts
        .into_iter()
        .map(|p| strip_edge_text(&p).trim().to_string())
        .collect();
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() {
            return Err(if i == 0 {
                "an edge has no node on its left".into()
            } else {
                "an edge has no node on its right".into()
            });
        }
        for node in p.split('&') {
            let id = node_id(node);
            if !is_ident(id) {
                return Err(format!("`{}` is not a node id", node.trim()));
            }
        }
    }
    Ok(())
}

fn drop_pipe_text(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        if c == '|' {
            inside = !inside;
            continue;
        }
        if !inside {
            out.push(c);
        }
    }
    out
}

/// `B -- text` (the text half of `A -- text --> B`): keep the node, drop the text.
fn strip_edge_text(p: &str) -> &str {
    match p.find(" -- ").or_else(|| p.find(" == ")) {
        Some(i) => &p[..i],
        None => p,
    }
}

const SEQ_ARROWS: &[&str] = &[
    "<<-->>", "<<->>", "-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)",
];

fn sequence_line<'a>(
    line: &'a str,
    blocks: &mut Vec<(usize, &'a str)>,
    n: usize,
) -> Result<(), String> {
    let w = first_word(line);
    match w {
        "participant" | "actor" => {
            let rest = line[w.len()..].trim();
            let name = rest.split(" as ").next().unwrap_or("").trim();
            return if name.is_empty() {
                Err(format!("`{w}` needs a name"))
            } else {
                Ok(())
            };
        }
        "loop" | "alt" | "opt" | "par" | "critical" | "break" | "rect" | "box" => {
            blocks.push((n, w));
            return Ok(());
        }
        "else" | "and" | "option" => {
            return if blocks.is_empty() {
                Err(format!("`{w}` outside a block"))
            } else {
                Ok(())
            };
        }
        "end" => {
            return match blocks.pop() {
                Some(_) => Ok(()),
                None => Err("`end` without a block".into()),
            };
        }
        "activate" | "deactivate" | "destroy" | "create" => {
            return if line.split_whitespace().nth(1).is_some() {
                Ok(())
            } else {
                Err(format!("`{w}` needs a participant"))
            };
        }
        "autonumber" | "title" | "accTitle:" | "accDescr:" => return Ok(()),
        _ => {}
    }
    if w.eq_ignore_ascii_case("note") {
        let rest = line[w.len()..].trim_start().to_ascii_lowercase();
        if !(rest.starts_with("left of")
            || rest.starts_with("right of")
            || rest.starts_with("over"))
        {
            return Err("a note is `Note left of|right of|over <participant>: text`".into());
        }
        return if line.contains(':') {
            Ok(())
        } else {
            Err("a note needs `: text`".into())
        };
    }
    let (head, _) = line.split_once(':').unwrap_or((line, ""));
    let Some(arrow) = SEQ_ARROWS.iter().find(|a| head.contains(**a)) else {
        return Err(format!(
            "expected a message (`A->>B: text`), a participant or a block, found `{line}`"
        ));
    };
    if !line.contains(':') {
        return Err("a message needs `: text` after the receiver".into());
    }
    let (from, to) = head.split_once(arrow).unwrap_or(("", ""));
    let to = to.trim().trim_start_matches(['+', '-']);
    if from.trim().is_empty() || to.trim().is_empty() {
        return Err("a message needs a sender and a receiver".into());
    }
    Ok(())
}

const CLASS_RELATIONS: &[&str] = &[
    "<|--", "--|>", "*--", "--*", "o--", "--o", "<--", "-->", "..|>", "<|..", "..>", "<..", "--",
    "..",
];

fn class_line<'a>(
    line: &'a str,
    blocks: &mut Vec<(usize, &'a str)>,
    n: usize,
) -> Result<(), String> {
    if line == "}" {
        return match blocks.pop() {
            Some(_) => Ok(()),
            None => Err("`}` without an open class".into()),
        };
    }
    if !blocks.is_empty() {
        // A member inside `class X { ... }`.
        return Ok(());
    }
    let w = first_word(line);
    match w {
        "class" => {
            let name = line.split_whitespace().nth(1).unwrap_or("");
            if name.is_empty() || name == "{" {
                return Err("`class` needs a name".into());
            }
            if line.ends_with('{') {
                blocks.push((n, "class"));
            }
            return Ok(());
        }
        "direction" | "note" | "classDef" | "style" | "cssClass" | "title" | "namespace" => {
            if w == "namespace" && line.ends_with('{') {
                blocks.push((n, "namespace"));
            }
            return Ok(());
        }
        _ => {}
    }
    if line.starts_with("<<") {
        return Ok(());
    }
    let head = line.split_once(" : ").map(|(h, _)| h).unwrap_or(line);
    if let Some(rel) = CLASS_RELATIONS.iter().find(|r| head.contains(**r)) {
        let (a, b) = head.split_once(rel).unwrap_or(("", ""));
        let clean = |s: &str| -> String {
            s.split('"')
                .enumerate()
                .filter(|(i, _)| i % 2 == 0)
                .map(|(_, p)| p)
                .collect::<String>()
                .trim()
                .to_string()
        };
        if clean(a).is_empty() || clean(b).is_empty() {
            return Err("a relationship needs a class on each side".into());
        }
        return Ok(());
    }
    // `Name : +member`
    if let Some((name, _)) = line.split_once(':') {
        if is_ident(name.trim()) {
            return Ok(());
        }
    }
    Err(format!(
        "expected a class, a member (`Name : member`) or a relationship, found `{line}`"
    ))
}

fn state_line<'a>(
    line: &'a str,
    blocks: &mut Vec<(usize, &'a str)>,
    n: usize,
) -> Result<(), String> {
    if line == "}" {
        return match blocks.pop() {
            Some(_) => Ok(()),
            None => Err("`}` without an open state".into()),
        };
    }
    let w = first_word(line);
    match w {
        "state" => {
            if line.ends_with('{') {
                blocks.push((n, "state"));
            }
            return if line.split_whitespace().nth(1).is_some() {
                Ok(())
            } else {
                Err("`state` needs a name".into())
            };
        }
        // `note right of X` ... `end note` spans lines; both ends are accepted.
        "direction" | "note" | "end" | "classDef" | "class" | "title" | "--" => return Ok(()),
        _ => {}
    }
    let head = line.split_once(':').map(|(h, _)| h).unwrap_or(line);
    if let Some((a, b)) = head.split_once("-->") {
        if a.trim().is_empty() || b.trim().is_empty() {
            return Err("a transition needs a state on each side".into());
        }
        return Ok(());
    }
    if is_ident(head.trim()) {
        return Ok(());
    }
    Err(format!(
        "expected a transition (`A --> B`), a state or a block, found `{line}`"
    ))
}

fn er_line<'a>(line: &'a str, blocks: &mut Vec<(usize, &'a str)>, n: usize) -> Result<(), String> {
    if line == "}" {
        return match blocks.pop() {
            Some(_) => Ok(()),
            None => Err("`}` without an open entity".into()),
        };
    }
    if !blocks.is_empty() {
        // An attribute: `type name [PK|FK|UK] ["comment"]`.
        return if line.split_whitespace().count() >= 2 {
            Ok(())
        } else {
            Err("an attribute is `type name`".into())
        };
    }
    if line.ends_with('{') {
        blocks.push((n, "entity"));
        return Ok(());
    }
    if matches!(
        first_word(line),
        "title" | "direction" | "style" | "classDef"
    ) {
        return Ok(());
    }
    let head = line.split_once(':').map(|(h, _)| h.trim()).unwrap_or(line);
    let mut words = head.split_whitespace();
    let (Some(a), Some(rel), Some(b)) = (words.next(), words.next(), words.next()) else {
        return if is_ident(head) {
            Ok(())
        } else {
            Err(format!(
                "expected an entity or a relationship, found `{line}`"
            ))
        };
    };
    if !is_er_relation(rel) {
        return Err(format!(
            "`{rel}` is not a relationship (for example `||--o{{` or `}}|..|{{`)"
        ));
    }
    if !line.contains(':') {
        return Err("a relationship needs `: label`".into());
    }
    if a.is_empty() || b.is_empty() {
        return Err("a relationship needs an entity on each side".into());
    }
    Ok(())
}

fn is_er_relation(rel: &str) -> bool {
    let (l, r) = match rel.split_once("--").or_else(|| rel.split_once("..")) {
        Some(p) => p,
        None => return false,
    };
    ["|o", "||", "}o", "}|"].contains(&l) && ["o|", "||", "o{", "|{"].contains(&r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(kind: DiagramKind, src: &str) {
        if let Err(e) = validate(kind, src) {
            panic!("expected valid, got {e:?}\n{src}");
        }
    }

    fn bad(kind: DiagramKind, src: &str) -> Vec<DiagramIssue> {
        validate(kind, src).expect_err("expected issues")
    }

    #[test]
    fn valid_diagrams_of_every_type_pass() {
        ok(
            DiagramKind::Flowchart,
            "flowchart LR\n  A[Start] --> B{Ready?}\n  B -->|yes| C((Done))\n  B -- no --> A\n  subgraph S [Group]\n    D & E --> F\n  end\n  classDef hot fill:#f00\n",
        );
        ok(
            DiagramKind::Sequence,
            "sequenceDiagram\n  participant C as Coordinator\n  actor U\n  U->>C: ask\n  loop each worker\n    C->>+W: delegate\n    W-->>-C: result\n  end\n  Note over C: synthesize\n  alt ok\n    C-->>U: answer\n  else failed\n    C--xU: error\n  end\n",
        );
        ok(
            DiagramKind::Class,
            "classDiagram\n  class Agent {\n    +String name\n    +run(task) Result\n  }\n  <<interface>> Provider\n  Agent --> Provider : uses\n  Agent \"1\" *-- \"many\" Task\n  Task : +objective\n",
        );
        ok(
            DiagramKind::State,
            "stateDiagram-v2\n  [*] --> Pending\n  Pending --> Running : start\n  state Running {\n    [*] --> Working\n  }\n  Running --> Done\n  Done --> [*]\n",
        );
        ok(
            DiagramKind::Er,
            "erDiagram\n  AGENT ||--o{ TASK : owns\n  TASK }|..|{ ARTIFACT : produces\n  TASK {\n    string id PK\n    string objective\n  }\n",
        );
        ok(
            DiagramKind::Component,
            "graph TD\n  api[API] --> rt[Runtime]\n  rt --> store[(Store)]\n",
        );
    }

    #[test]
    fn the_header_must_match_the_kind() {
        let e = bad(DiagramKind::Sequence, "flowchart LR\n A --> B\n");
        assert!(e[0].message.contains("sequenceDiagram"), "{e:?}");
        let e = bad(DiagramKind::Flowchart, "A --> B\n");
        assert_eq!(e[0].line, 1);
        let e = bad(DiagramKind::Flowchart, "flowchart XY\n A --> B\n");
        assert!(e[0].message.contains("direction"));
    }

    #[test]
    fn structural_problems_are_reported_by_line() {
        let e = bad(DiagramKind::Flowchart, "flowchart TD\n A[open --> B\n");
        assert_eq!(e[0].line, 2);
        let e = bad(DiagramKind::Flowchart, "flowchart TD\n A -->\n");
        assert!(e[0].message.contains("right"), "{e:?}");
        let e = bad(
            DiagramKind::Flowchart,
            "flowchart TD\n subgraph X\n A --> B\n",
        );
        assert!(e[0].message.contains("never closed"));
        let e = bad(DiagramKind::Sequence, "sequenceDiagram\n A->>B\n");
        assert!(e[0].message.contains(": text"), "{e:?}");
        let e = bad(
            DiagramKind::Sequence,
            "sequenceDiagram\n loop x\n A->>B: hi\n",
        );
        assert!(e[0].message.contains("loop"));
        let e = bad(DiagramKind::Sequence, "sequenceDiagram\n hello there\n");
        assert!(e[0].message.contains("expected a message"));
        let e = bad(DiagramKind::Er, "erDiagram\n A <-> B : x\n");
        assert!(e[0].message.contains("relationship"));
        let e = bad(DiagramKind::State, "stateDiagram-v2\n --> B\n");
        assert!(e[0].message.contains("each side"));
        let e = bad(DiagramKind::Class, "classDiagram\n class A {\n  +x\n");
        assert!(e[0].message.contains("never closed"));
    }

    #[test]
    fn callbacks_are_refused() {
        let e = bad(
            DiagramKind::Flowchart,
            "flowchart TD\n A --> B\n click A href \"javascript:alert(1)\"\n",
        );
        assert!(e[0].message.contains("callbacks"));
    }

    #[test]
    fn stored_text_round_trips() {
        let mut d = Diagram::new(
            DiagramKind::Sequence,
            "Delegation",
            "sequenceDiagram\n A->>B: hi",
        );
        d.version = 2;
        d.revises = Some("abc".into());
        let back = Diagram::from_text(&d.to_text()).unwrap();
        assert_eq!(back, d);
        assert!(back.validate().is_ok());
        let plain = Diagram::from_text("%% a comment\nclassDiagram\n A --> B\n").unwrap();
        assert_eq!(plain.kind, DiagramKind::Class);
        assert_eq!(
            DiagramKind::parse("Sequence diagram"),
            Some(DiagramKind::Sequence)
        );
    }
}
