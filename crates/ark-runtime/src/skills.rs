//! Skills: instructions for a kind of work, kept in folders and read only when a task needs
//! them. The format is the Agent Skills one: a folder with a `SKILL.md` whose front matter
//! names it and says when to use it, followed by the instructions, and any files they refer to.
//!
//! ```text
//! ---
//! name: release-notes
//! description: Write release notes from a list of changes. Use when asked for notes.
//! ---
//! # Release notes
//! 1. Group the changes ...
//! ```
//!
//! A turn never carries a skill's instructions up front:
//!
//! 1. [`catalog`] lists the skills a turn may use, one line each (name and description), under
//!    a token cap. The harness puts it after the rules (`== SKILLS ==`) and reports it as
//!    `skills` ([`crate::Harness::skills`]).
//! 2. [`SkillsTool`] reads one on demand: `skills.read` returns its instructions and the files
//!    beside them; `skills.file` reads one of those files. Both are read-only.
//!
//! Where skills live is the application's choice; [`dirs`] gives the usual two: the project
//! folder's `.agents/skills` and the store's `skills`. A name in the project hides the same
//! name in the store. Installing skills is not ARK's business (a person runs an installer, or
//! copies a folder).

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ark_core::{ActionDescriptor, ProjectId, RiskTier};
use ark_model::estimate_tokens;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::distill::Distiller;
use crate::Engine;

pub const READ: &str = "skills.read";
pub const FILE: &str = "skills.file";

/// Tokens the catalog may take in a turn; the skills past it stay readable by name.
pub const CATALOG_BUDGET: u32 = 600;
/// A skill's instructions reach the model whole up to this many characters.
const BODY_CHARS: usize = 24_000;
/// A file beside a skill, read with `skills.file`, up to this many bytes.
const FILE_BYTES: u64 = 64 * 1024;
/// Files listed with a skill's instructions.
const FILES_LISTED: usize = 40;
/// A description longer than this is clipped in the catalog (the whole one is in `skills.read`).
const DESCRIPTION_CHARS: usize = 300;

/// Where a skill was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    /// The project folder's `.agents/skills`.
    Project,
    /// The store's `skills`, shared by every project.
    Store,
}

/// One skill: its name, when to use it, and its folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub scope: SkillScope,
}

/// The usual places, most specific first: the project folder's `.agents/skills`, then the
/// store's `skills`.
pub fn dirs(engine: &Engine, project: &ProjectId) -> Vec<(PathBuf, SkillScope)> {
    let mut out = Vec::new();
    if let Ok(ws) = engine.workspace_dir(project) {
        out.push((ws.join(".agents").join("skills"), SkillScope::Project));
    }
    out.push((engine.store.root().join("skills"), SkillScope::Store));
    out
}

/// Every skill in `places`, by name. A name found in an earlier place hides the same name
/// later. A folder whose `SKILL.md` cannot be read or has no description is left out.
pub fn discover(places: &[(PathBuf, SkillScope)]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for (root, scope) in places {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<Skill> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let dir = e.path();
                let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
                let folder = e.file_name().to_string_lossy().into_owned();
                let (front, _) = split_front_matter(&text);
                let name = front.get("name").cloned().unwrap_or_else(|| folder.clone());
                let description = front.get("description").cloned().unwrap_or_default();
                if !valid_name(&name) || description.trim().is_empty() {
                    tracing::warn!(
                        "skill `{}` skipped: no valid name or description",
                        dir.display()
                    );
                    return None;
                }
                Some(Skill {
                    name,
                    description: description.trim().to_string(),
                    dir,
                    scope: *scope,
                })
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for s in found {
            if !out.iter().any(|o| o.name == s.name) {
                out.push(s);
            }
        }
    }
    out
}

/// The skills a pattern list allows: `*` for all, else names.
pub fn allowed(skills: &[Skill], patterns: &[String]) -> Vec<Skill> {
    skills
        .iter()
        .filter(|s| patterns.iter().any(|p| p == "*" || p == &s.name))
        .cloned()
        .collect()
}

/// The skills block of a turn and what it left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillCatalog {
    pub text: String,
    /// Skills listed.
    pub shown: usize,
    /// Skills past the cap, still readable by name.
    pub held_back: usize,
}

/// One line per skill, in name order, until `budget` tokens. `None` when there are none.
pub fn catalog(skills: &[Skill], budget: u32) -> Option<SkillCatalog> {
    if skills.is_empty() {
        return None;
    }
    let mut text = format!(
        "Instructions for kinds of work. When the task is one of these, call {READ} with its name first and follow what it says.\n"
    );
    let mut shown = 0;
    for s in skills {
        let line = format!(
            "- {}: {}\n",
            s.name,
            clip(&s.description, DESCRIPTION_CHARS)
        );
        if shown > 0 && estimate_tokens(&text) + estimate_tokens(&line) > budget {
            break;
        }
        text.push_str(&line);
        shown += 1;
    }
    let held_back = skills.len() - shown;
    if held_back > 0 {
        let _ = writeln!(
            text,
            "({held_back} more; {READ} with any name, or an unknown one to list them all.)"
        );
    }
    Some(SkillCatalog {
        text: text.trim_end().to_string(),
        shown,
        held_back,
    })
}

/// `skills.read` and `skills.file` over a fixed set of skills.
pub struct SkillsTool {
    skills: Vec<Skill>,
}

impl SkillsTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        SkillsTool { skills }
    }
}

#[async_trait::async_trait]
impl Adapter for SkillsTool {
    fn name(&self) -> &str {
        "skills"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                READ,
                "read a skill's instructions (and the files beside them) by its name",
                RiskTier::Read,
            )
            .reusable()
            .with_input_schema(json!({
                "type": "object", "required": ["name"],
                "properties": { "name": { "type": "string" } }
            })),
            ActionDescriptor::new(FILE, "read a file that a skill refers to", RiskTier::Read)
                .reusable()
                .with_input_schema(json!({
                    "type": "object", "required": ["name", "path"],
                    "properties": { "name": { "type": "string" }, "path": { "type": "string" } }
                })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let arg = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let Some(skill) = self.skills.iter().find(|s| s.name == arg("name")) else {
            let names: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
            return Err(AdapterError::Tool(format!(
                "no skill named `{}`; the skills are: {}",
                arg("name"),
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                }
            )));
        };
        match action {
            READ => {
                let text = std::fs::read_to_string(skill.dir.join("SKILL.md"))
                    .map_err(|e| AdapterError::Tool(format!("reading {}: {e}", skill.name)))?;
                let (_, body) = split_front_matter(&text);
                let chars = body.chars().count();
                let mut out = json!({
                    "skill": skill.name,
                    "description": skill.description,
                    "instructions": clip(body.trim(), BODY_CHARS),
                    "files": files(&skill.dir),
                });
                if chars > BODY_CHARS {
                    out["clipped"] = json!(format!("{BODY_CHARS} of {chars} characters"));
                }
                Ok(ActionOutput::json(out))
            }
            FILE => {
                let path = inside(&skill.dir, arg("path"))?;
                let meta = std::fs::metadata(&path)
                    .map_err(|e| AdapterError::Tool(format!("{}: {e}", arg("path"))))?;
                if meta.len() > FILE_BYTES {
                    return Err(AdapterError::Tool(format!(
                        "{} is {} bytes; files over {FILE_BYTES} are not read",
                        arg("path"),
                        meta.len()
                    )));
                }
                let bytes = std::fs::read(&path)
                    .map_err(|e| AdapterError::Tool(format!("{}: {e}", arg("path"))))?;
                if bytes.contains(&0) {
                    return Err(AdapterError::Tool(format!("{} is not text", arg("path"))));
                }
                Ok(ActionOutput::json(json!({
                    "skill": skill.name,
                    "path": arg("path"),
                    "text": String::from_utf8_lossy(&bytes),
                })))
            }
            other => Err(AdapterError::Tool(format!("unknown action {other}"))),
        }
    }
}

/// What the model sees of a skill: its instructions whole (they are the point), the files
/// beside them by name, and of a file its text.
pub fn distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let s = |k: &str| out.get(k).and_then(Value::as_str).unwrap_or("");
        match action {
            READ => {
                let mut text = format!("skill {}:\n{}", s("skill"), s("instructions"));
                if !s("clipped").is_empty() {
                    let _ = write!(text, "\n(clipped: {})", s("clipped"));
                }
                let files: Vec<&str> = out
                    .get("files")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if !files.is_empty() {
                    let _ = write!(text, "\nFiles ({FILE} reads one): {}", files.join(", "));
                }
                Some((text, vec![s("skill").to_string()]))
            }
            FILE => Some((
                format!("{} of skill {}:\n{}", s("path"), s("skill"), s("text")),
                vec![s("path").to_string()],
            )),
            _ => None,
        }
    })
}

/// Front matter (`key: value` lines between `---` fences, with `>` and `|` blocks and quoted
/// values) and the text after it. A file without front matter is all body.
fn split_front_matter(text: &str) -> (std::collections::HashMap<String, String>, &str) {
    let mut map = std::collections::HashMap::new();
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else {
        return (map, t);
    };
    let Some(end) = rest.find("\n---") else {
        return (map, t);
    };
    let head = &rest[..end];
    let body = rest[end + 4..]
        .trim_start_matches(['-'])
        .trim_start_matches('\n');
    let lines: Vec<&str> = head.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
            continue; // nested keys (metadata: ...) are not read
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let value = if v == ">" || v == "|" || v == ">-" || v == "|-" {
            let mut parts = Vec::new();
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].trim().is_empty()) {
                parts.push(lines[i].trim());
                i += 1;
            }
            let sep = if v.starts_with('>') { " " } else { "\n" };
            parts.join(sep).trim().to_string()
        } else {
            unquote(v)
        };
        map.insert(k.trim().to_string(), value);
    }
    (map, body)
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1].replace("\\\"", "\"").replace("''", "'")
    } else {
        v.to_string()
    }
}

/// Lower-case letters, digits and `-`, as the format asks; 64 at most.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The files of a skill's folder (not `SKILL.md`, nothing hidden), relative, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(dir) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if rel != "SKILL.md" {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    out.truncate(FILES_LISTED);
    out
}

/// `rel` inside `dir`, or an error: no `..`, no absolute path, no link pointing out.
fn inside(dir: &Path, rel: &str) -> ark_tools::Result<PathBuf> {
    let r = Path::new(rel);
    if rel.is_empty()
        || r.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(AdapterError::Tool(format!(
            "`{rel}` is outside the skill's folder"
        )));
    }
    let root = dir
        .canonicalize()
        .map_err(|e| AdapterError::Tool(format!("skill folder: {e}")))?;
    let real = root
        .join(r)
        .canonicalize()
        .map_err(|e| AdapterError::Tool(format!("{rel}: {e}")))?;
    if !real.starts_with(&root) {
        return Err(AdapterError::Tool(format!(
            "`{rel}` is outside the skill's folder"
        )));
    }
    Ok(real)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, folder: &str, text: &str) {
        let d = root.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn front_matter_reads_the_format() {
        let (m, body) = split_front_matter(
            "---\nname: release-notes\ndescription: >\n  Write notes\n  from changes.\nlicense: \"MIT\"\nmetadata:\n  version: 2\n---\n# Notes\nStep one.\n",
        );
        assert_eq!(m["name"], "release-notes");
        assert_eq!(m["description"], "Write notes from changes.");
        assert_eq!(m["license"], "MIT");
        assert!(!m.contains_key("version"), "nested keys are not read");
        assert_eq!(body, "# Notes\nStep one.\n");
        let (m, body) = split_front_matter("just text");
        assert!(m.is_empty() && body == "just text");
    }

    #[test]
    fn the_project_hides_the_store_and_bad_skills_are_left_out() {
        let project = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        skill(
            project.path(),
            "notes",
            "---\nname: notes\ndescription: the project's\n---\nP",
        );
        skill(
            store.path(),
            "notes",
            "---\nname: notes\ndescription: the store's\n---\nS",
        );
        skill(
            store.path(),
            "tidy",
            "---\ndescription: tidy a folder\n---\nT",
        );
        skill(
            store.path(),
            "Bad Name",
            "---\nname: Bad Name\ndescription: x\n---\n",
        );
        skill(
            store.path(),
            "silent",
            "---\nname: silent\n---\nno description",
        );
        let all = discover(&[
            (project.path().to_path_buf(), SkillScope::Project),
            (store.path().to_path_buf(), SkillScope::Store),
        ]);
        let got: Vec<(&str, &str, SkillScope)> = all
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str(), s.scope))
            .collect();
        assert_eq!(
            got,
            [
                ("notes", "the project's", SkillScope::Project),
                ("tidy", "tidy a folder", SkillScope::Store),
            ]
        );
        assert_eq!(allowed(&all, &["tidy".into()]).len(), 1);
        assert_eq!(allowed(&all, &["*".into()]).len(), 2);
        assert!(allowed(&all, &[]).is_empty());
    }

    #[test]
    fn the_catalog_stays_under_its_cap() {
        let many: Vec<Skill> = (0..60)
            .map(|i| Skill {
                name: format!("skill-{i:02}"),
                description: "does one particular kind of work, described in a sentence".into(),
                dir: PathBuf::new(),
                scope: SkillScope::Store,
            })
            .collect();
        let c = catalog(&many, 200).unwrap();
        assert!(
            estimate_tokens(&c.text) <= 230,
            "{}",
            estimate_tokens(&c.text)
        );
        assert_eq!(c.shown + c.held_back, 60);
        assert!(c.held_back > 0 && c.text.contains("more;"));
        assert!(catalog(&[], 200).is_none());
    }

    #[tokio::test]
    async fn read_and_file_stay_in_the_skill() {
        let root = tempfile::tempdir().unwrap();
        skill(
            root.path(),
            "tidy",
            "---\nname: tidy\ndescription: tidy\n---\nUse template.md.",
        );
        std::fs::write(root.path().join("tidy/template.md"), "# Template").unwrap();
        std::fs::write(root.path().join("secret"), "s").unwrap();
        struct NoBlobs;
        impl ark_tools::BlobResolver for NoBlobs {
            fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
                None
            }
            fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
                Err(AdapterError::ArtifactNotFound(id.to_string()))
            }
        }
        let tool = SkillsTool::new(discover(&[(root.path().to_path_buf(), SkillScope::Store)]));
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        let out = tool
            .run(READ, json!({ "name": "tidy" }), &ctx)
            .await
            .unwrap();
        let (text, _) = distiller()(READ, &out.output).unwrap();
        assert!(
            text.contains("Use template.md.") && text.contains("template.md"),
            "{text}"
        );
        let f = tool
            .run(FILE, json!({ "name": "tidy", "path": "template.md" }), &ctx)
            .await
            .unwrap();
        assert_eq!(f.output["text"], "# Template");
        for bad in ["../secret", "/etc/hosts", ""] {
            assert!(
                tool.run(FILE, json!({ "name": "tidy", "path": bad }), &ctx)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        let Err(e) = tool.run(READ, json!({ "name": "nope" }), &ctx).await else {
            panic!("an unknown skill is an error");
        };
        assert!(e.to_string().contains("the skills are: tidy"), "{e}");
    }
}
