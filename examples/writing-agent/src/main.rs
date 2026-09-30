//! # writer
//!
//! A developmental editor for novelists, built on the same ARK runtime and the same ARK
//! terminal shell as the security workstation. Nothing in ARK knows it is a writing tool:
//! the manuscript tools, the editor persona, the `/chapter` and `/style` commands, the
//! magenta theme and the activity labels are all supplied here.
//!
//! ```text
//! cargo run -p writing-agent              # interactive: the writer's shell
//! writer --resume                         # continue the last session
//! writer -p "how long is chapter 4?"      # one-shot, for scripts
//! ```
//!
//! The manuscript is the current directory when it holds `.md`/`.txt` chapters; otherwise a
//! sample novel is seeded into the project workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_cli::{
    Accent, ActivityLabels, ArkCli, Command, CommandContext, CommandOutcome, Theme, ToolRenderer,
};
use ark_sdk::prelude::*;

// ---- manuscript tools ------------------------------------------------------

/// Read/inspect chapters that live as files in the project workspace.
struct Manuscript;

#[async_trait::async_trait]
impl Adapter for Manuscript {
    fn name(&self) -> &str {
        "manuscript"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                "manuscript.list",
                "list the chapter files with their word counts",
                RiskTier::Read,
            )
            .with_capabilities([Capability::new("structure-analysis")]),
            ActionDescriptor::new(
                "manuscript.read",
                "read a chapter's full text",
                RiskTier::Read,
            )
            .with_capabilities([Capability::new("revision")])
            .with_input_schema(file_schema("the chapter filename, e.g. chapter-04.md")),
            ActionDescriptor::new(
                "manuscript.wordcount",
                "count words and paragraphs in a chapter",
                RiskTier::Read,
            )
            .with_capabilities([Capability::new("structure-analysis")])
            .with_input_schema(file_schema("the chapter filename")),
            ActionDescriptor::new(
                "manuscript.save",
                "write text to a chapter (creates or overwrites)",
                RiskTier::Mutate,
            )
            .with_capabilities([Capability::new("drafting")])
            .with_input_schema(serde_json::json!({
                "type": "object", "required": ["file", "text"],
                "properties": {
                    "file": { "type": "string" },
                    "text": { "type": "string" }
                }
            })),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let ws = ctx.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        match action {
            "manuscript.list" => {
                let chapters: Vec<serde_json::Value> = chapter_files(&ws)
                    .into_iter()
                    .map(|p| {
                        let text = std::fs::read_to_string(&p).unwrap_or_default();
                        serde_json::json!({ "file": file_name(&p), "words": word_count(&text), "title": title(&text) })
                    })
                    .collect();
                Ok(ActionOutput::json(
                    serde_json::json!({ "count": chapters.len(), "chapters": chapters }),
                ))
            }
            "manuscript.read" => {
                let file = req_str(&input, "file")?;
                let text = read_chapter(&ws, &file)?;
                let body: String = text.chars().take(12_000).collect();
                Ok(ActionOutput::json(serde_json::json!({
                    "path": file, "title": title(&text), "lines": text.lines().count(), "words": word_count(&text),
                    "truncated": body.len() < text.len(), "text": body
                })))
            }
            "manuscript.wordcount" => {
                let file = req_str(&input, "file")?;
                let text = read_chapter(&ws, &file)?;
                let words = word_count(&text);
                let paras = text.split("\n\n").filter(|p| !p.trim().is_empty()).count();
                Ok(ActionOutput::json(serde_json::json!({
                    "path": file, "words": words, "paragraphs": paras, "lines": text.lines().count()
                })))
            }
            "manuscript.save" => {
                let file = req_str(&input, "file")?;
                let text = req_str(&input, "text")?;
                confine(&file)?;
                let path = ws.join(&file);
                std::fs::write(&path, text.as_bytes())
                    .map_err(|e| ark_tools::AdapterError::Tool(format!("write {file}: {e}")))?;
                Ok(ActionOutput::json(
                    serde_json::json!({ "path": file, "words": word_count(&text) }),
                )
                .with_command(vec!["write".into(), file]))
            }
            other => Err(ark_tools::AdapterError::UnknownAction(other.into())),
        }
    }
}

/// Continuity and voice: how a term recurs, and what the narration sounds like.
struct Craft;

#[async_trait::async_trait]
impl Adapter for Craft {
    fn name(&self) -> &str {
        "craft"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("craft.continuity", "find every mention of a term across all chapters", RiskTier::Read)
                .with_capabilities([Capability::new("continuity-check")])
                .with_input_schema(serde_json::json!({
                    "type": "object", "required": ["term"],
                    "properties": { "term": { "type": "string", "description": "a character name, place, or object" } }
                })),
            ActionDescriptor::new(
                "craft.voice",
                "measure the narrative voice of a chapter or the whole manuscript (person, tense cues, sentence length, dialogue share)",
                RiskTier::Read,
            )
            .with_capabilities([Capability::new("voice-analysis")])
            .with_input_schema(serde_json::json!({
                "type": "object",
                "properties": { "file": { "type": "string", "description": "a chapter; omit for the whole manuscript" } }
            })),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let ws = ctx.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        match action {
            "craft.continuity" => {
                let term = req_str(&input, "term")?.to_lowercase();
                let mut matches = Vec::new();
                for path in chapter_files(&ws) {
                    let name = file_name(&path);
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        for (i, line) in text.lines().enumerate() {
                            if line.to_lowercase().contains(&term) {
                                let excerpt: String = line.chars().take(120).collect();
                                matches.push(serde_json::json!({ "path": name, "line": i + 1, "excerpt": excerpt }));
                            }
                        }
                    }
                }
                Ok(ActionOutput::json(
                    serde_json::json!({ "query": term, "count": matches.len(), "matches": matches }),
                ))
            }
            "craft.voice" => {
                let text = match input
                    .get("file")
                    .and_then(|f| f.as_str())
                    .filter(|f| !f.is_empty())
                {
                    Some(f) => read_chapter(&ws, f)?,
                    None => chapter_files(&ws)
                        .iter()
                        .filter_map(|p| std::fs::read_to_string(p).ok())
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                };
                Ok(ActionOutput::json(voice_profile(&text)))
            }
            other => Err(ark_tools::AdapterError::UnknownAction(other.into())),
        }
    }
}

/// A deterministic sketch of narrative voice. Numbers, not opinions: the model interprets them.
fn voice_profile(text: &str) -> serde_json::Value {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let count = |set: &[&str]| words.iter().filter(|w| set.contains(&w.as_str())).count();
    let first = count(&["i", "me", "my", "mine", "myself", "we", "our"]);
    let third = count(&["he", "she", "him", "her", "his", "hers", "they", "them"]);
    let sentences: Vec<&str> = text
        .split(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| s.split_whitespace().count() > 1)
        .collect();
    let avg = if sentences.is_empty() {
        0.0
    } else {
        sentences
            .iter()
            .map(|s| s.split_whitespace().count())
            .sum::<usize>() as f64
            / sentences.len() as f64
    };
    let quoted = text.matches('"').count() / 2 + text.matches('“').count();
    let paragraphs = text
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .count()
        .max(1);
    let past = count(&["was", "had", "were", "did", "said"]);
    let present = count(&["is", "has", "are", "does", "says"]);
    serde_json::json!({
        "words": words.len(),
        "person": if first > third { "first" } else { "third" },
        "first_person_pronouns": first,
        "third_person_pronouns": third,
        "tense_lean": if past >= present { "past" } else { "present" },
        "avg_sentence_words": (avg * 10.0).round() / 10.0,
        "dialogue_lines": quoted,
        "dialogue_per_paragraph": ((quoted as f64 / paragraphs as f64) * 100.0).round() / 100.0,
    })
}

// ---- a domain distiller ----------------------------------------------------

/// Writer-facing evidence summaries. Anything unrecognized falls through to ARK's common
/// distiller and generic fallback.
fn writing_distiller() -> DistillerSet {
    let d: Distiller = Arc::new(|action: &str, out: &serde_json::Value| {
        let n = |k: &str| out.get(k).and_then(|v| v.as_u64());
        let s = |k: &str| out.get(k).and_then(|v| v.as_str()).map(str::to_string);
        match action {
            "manuscript.wordcount" => {
                let words = n("words").unwrap_or(0);
                let paras = n("paragraphs").unwrap_or(0);
                let file = s("path").unwrap_or_default();
                Some((
                    format!("{file}: {words} words in {paras} paragraph(s)"),
                    vec![format!("words={words}")],
                ))
            }
            "manuscript.read" => {
                let file = s("path").unwrap_or_default();
                let words = n("words").unwrap_or(0);
                Some((
                    format!("read {file} \"{}\" ({words} words); full text via get_evidence(path=\"text\")", s("title").unwrap_or_default()),
                    vec![file],
                ))
            }
            "manuscript.list" => {
                let files: Vec<String> = out["chapters"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|c| {
                                format!("{} ({}w)", c["file"].as_str().unwrap_or(""), c["words"])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some((
                    format!("{} chapters: {}", files.len(), files.join(", ")),
                    files,
                ))
            }
            "manuscript.save" => Some((
                format!(
                    "saved {} ({} words)",
                    s("path").unwrap_or_default(),
                    n("words").unwrap_or(0)
                ),
                vec![s("path").unwrap_or_default()],
            )),
            "craft.continuity" => {
                let hits: Vec<String> = out["matches"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|m| format!("{}:{}", m["path"].as_str().unwrap_or(""), m["line"]))
                            .collect()
                    })
                    .unwrap_or_default();
                let files: std::collections::BTreeSet<&str> =
                    hits.iter().filter_map(|h| h.split(':').next()).collect();
                let term = s("query").unwrap_or_default();
                Some((
                    format!(
                        "\"{term}\": {} mention(s) in {}",
                        hits.len(),
                        files.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                    hits,
                ))
            }
            "craft.voice" => Some((
                format!(
                    "{}-person, {} tense lean, {} words/sentence, {} dialogue lines per paragraph",
                    s("person").unwrap_or_default(),
                    s("tense_lean").unwrap_or_default(),
                    out["avg_sentence_words"],
                    out["dialogue_per_paragraph"]
                ),
                vec![],
            )),
            _ => None,
        }
    });
    DistillerSet::with_common().with(d)
}

// ---- wiring ----------------------------------------------------------------

pub fn writer_profile() -> AgentProfile {
    AgentProfile::builder()
        .instructions(
            "You are a sharp, encouraging developmental editor helping an author with their novel. You \
             work from the manuscript itself, never from assumptions: list and read chapters, measure \
             their length, trace how characters and objects recur, and measure the narrative voice \
             before you judge pacing, continuity or style. Tool results are summaries; call \
             get_evidence(invocation, path=\"text\") when you need a chapter's actual words. Be concrete \
             and cite the chapter and line. Distinguish what the text says (a fact) from your editorial \
             opinion (an inference). Respect the author's voice: when you rewrite, keep the narrator's \
             person, tense, rhythm and diction, and only save a chapter when the author asks you to.",
        )
        .capabilities(CapabilityCatalog::new([
            CapabilitySpec::new("structure-analysis", "measure a chapter's length and shape").with_synonyms(["pacing", "length", "wordcount"]),
            CapabilitySpec::new("continuity-check", "track how a character, place or object recurs").with_synonyms(["continuity", "consistency"]),
            CapabilitySpec::new("voice-analysis", "measure narrative person, tense, rhythm and dialogue").with_synonyms(["voice", "style", "tone"]),
            CapabilitySpec::new("revision", "read and assess a passage for revision").with_synonyms(["edit", "review", "read"]),
            CapabilitySpec::new("drafting", "write or save new prose").with_synonyms(["write", "compose", "draft", "rewrite"]),
        ]))
        .investigation_markers(["why", "how", "compare", "foreshadow", "continuity", "consistent", "pacing", "rewrite", "voice", " and "])
        .distillers(writing_distiller())
        .core_actions(["manuscript.list", "manuscript.read", "manuscript.wordcount", "craft.continuity", "craft.voice"])
        .build()
}

fn build_ark(
    store_root: impl Into<PathBuf>,
    provider: Option<Arc<dyn ark_model::Provider>>,
) -> Result<Ark, ArkError> {
    let mut b = Ark::builder()
        .store_root(store_root)
        .tool(Arc::new(Manuscript))
        .tool(Arc::new(Craft))
        .profile(writer_profile());
    if let Some(p) = provider {
        b = b.provider(p);
    }
    b.build()
}

/// The project for a manuscript: the current directory when it holds chapters, else the sample.
fn manuscript_project(ark: &Ark) -> Result<Project, ArkError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if !chapter_files(&cwd).is_empty() {
        let name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "manuscript".into());
        if let Some(p) = ark
            .engine()
            .store
            .list_projects()
            .map_err(ark_runtime::AgentError::from)?
            .into_iter()
            .find(|p| p.workspace.as_deref() == Some(&*cwd.to_string_lossy()))
        {
            return Ok(p);
        }
        let p = Project::new(name, "a novel in progress").with_workspace(cwd.to_string_lossy());
        ark.engine()
            .store
            .put_project(&p)
            .map_err(ark_runtime::AgentError::from)?;
        return Ok(p);
    }
    let p = ark.find_or_create_project("the-locket", "a debut novel (sample manuscript)")?;
    let ws = ark.engine().workspace_dir(&p.id)?;
    if chapter_files(&ws).is_empty() {
        seed_manuscript(&ws).map_err(ark_runtime::AgentError::from)?;
    }
    Ok(p)
}

/// A small sample novel with a distinct first-person, past-tense narrator.
fn seed_manuscript(ws: &Path) -> std::io::Result<()> {
    let chapters = [
        ("chapter-01.md", "# Chapter 1: Varga Station\n\nI turned the brass locket over in my palm, its hinge stiff with age. The train pulled out of Varga station as the last light failed.\n\nI had promised my grandmother I would never open it. I was already regretting the promise.\n"),
        ("chapter-02.md", "# Chapter 2: The Coast Road\n\nThe locket stayed cold against my collarbone all the way to the coast. Varga was three days behind me now, and the sea smelled of iron.\n\n\"You're Mara Venn,\" the ferryman said. It was not a question.\n\n\"Depends who's asking,\" I said.\n"),
        ("chapter-03.md", "# Chapter 3: Salt\n\nThe island had one road, one bell, and one woman who remembered my grandmother. Her name was Ilse. She did not like me.\n\nShe looked at the chain at my throat for a long time before she let me in.\n"),
        ("chapter-04.md", "# Chapter 4: The Hinge\n\nIt was raining when I finally opened it. It was raining and it was late and Ilse was asleep and I was tired of promises, so I opened it, and the hinge gave with a sound like a small bone breaking.\n\nThere was no picture inside. There was a key, and a strip of paper, and on the paper in my grandmother's cramped hand were the words: not yet.\n\nI sat on the floor for a long time. Then I closed it again, which is the kind of thing I do.\n"),
    ];
    for (name, text) in chapters {
        std::fs::write(ws.join(name), text)?;
    }
    Ok(())
}

// ---- writer-specific shell pieces ---------------------------------------

/// `/manuscript`: the chapters at a glance, straight from the files.
struct ManuscriptCmd;
#[async_trait::async_trait]
impl Command for ManuscriptCmd {
    fn name(&self) -> &str {
        "manuscript"
    }
    fn summary(&self) -> &str {
        "List chapters with titles and word counts"
    }
    fn group(&self) -> &str {
        "Writing"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let ws = ctx.engine().workspace_dir(&ctx.project().id)?;
        let files = chapter_files(&ws);
        ctx.heading(&format!(
            "{} · {} chapters",
            ctx.project().name,
            files.len()
        ));
        let mut total = 0;
        for p in &files {
            let text = std::fs::read_to_string(p).unwrap_or_default();
            let w = word_count(&text);
            total += w;
            let row = format!(
                "{:<18}{:>7} words  {}",
                file_name(p),
                w,
                ctx.theme().dim(&title(&text))
            );
            ctx.line(format!("  {row}"));
        }
        ctx.line(
            ctx.theme()
                .dim(&format!("  {total} words in {}", ws.display())),
        );
        Ok(CommandOutcome::Continue)
    }
}

/// `/chapter <n>`: ask the editor about one chapter.
struct ChapterCmd;
#[async_trait::async_trait]
impl Command for ChapterCmd {
    fn name(&self) -> &str {
        "chapter"
    }
    fn summary(&self) -> &str {
        "Have the editor read and assess a chapter"
    }
    fn usage(&self) -> &str {
        "<n>"
    }
    fn group(&self) -> &str {
        "Writing"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let ws = ctx.engine().workspace_dir(&ctx.project().id)?;
        let Some(file) = find_chapter(&ws, args) else {
            ctx.line(
                ctx.theme()
                    .yellow("Which chapter? e.g. /chapter 4 (see /manuscript)"),
            );
            return Ok(CommandOutcome::Continue);
        };
        Ok(CommandOutcome::Submit(format!(
            "Read {file} and give me a short developmental assessment: what the chapter does, how the pacing and voice work, and the one change that would help most."
        )))
    }
}

/// `/style [n]`: the voice profile, measured directly (no model call).
struct StyleCmd;
#[async_trait::async_trait]
impl Command for StyleCmd {
    fn name(&self) -> &str {
        "style"
    }
    fn summary(&self) -> &str {
        "Measure the narrative voice of a chapter or the whole book"
    }
    fn usage(&self) -> &str {
        "[n]"
    }
    fn group(&self) -> &str {
        "Writing"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let ws = ctx.engine().workspace_dir(&ctx.project().id)?;
        let input = match find_chapter(&ws, args) {
            Some(f) => serde_json::json!({ "file": f }),
            None => serde_json::json!({}),
        };
        let inv = ctx.run_action("craft.voice", input, false).await?;
        let o = &inv.output;
        ctx.heading(if args.is_empty() {
            "Voice · whole manuscript"
        } else {
            "Voice"
        });
        for key in [
            "person",
            "tense_lean",
            "avg_sentence_words",
            "dialogue_per_paragraph",
            "words",
        ] {
            ctx.row(&key.replace('_', " "), o[key].to_string().trim_matches('"'));
        }
        Ok(CommandOutcome::Continue)
    }
}

/// Show a save as the file it touches and how big the new text is.
struct SaveRenderer;
impl ToolRenderer for SaveRenderer {
    fn approval(&self, r: &ApprovalRequest) -> Option<String> {
        (r.action == "manuscript.save").then(|| {
            let words = word_count(r.input["text"].as_str().unwrap_or(""));
            format!(
                "manuscript.save(\"{}\")  · {words} words",
                r.input["file"].as_str().unwrap_or("?")
            )
        })
    }
}

fn labels() -> ActivityLabels {
    let file = |i: &serde_json::Value| i["file"].as_str().unwrap_or("a chapter").to_string();
    ActivityLabels::new()
        .label("manuscript.list", |_| {
            "Reading the table of contents".into()
        })
        .label("manuscript.read", move |i| format!("Reading {}", file(i)))
        .label("manuscript.wordcount", move |i| {
            format!("Measuring {}", file(i))
        })
        .label("manuscript.save", move |i| format!("Saving {}", file(i)))
        .label("craft.continuity", |i| {
            format!(
                "Tracing \"{}\" through the manuscript",
                i["term"].as_str().unwrap_or("?")
            )
        })
        .label("craft.voice", |i| match i["file"].as_str() {
            Some(f) => format!("Listening to the narrator in {f}"),
            None => "Listening to the narrator".into(),
        })
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let root = std::env::var_os("WRITER_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".writer")
        });
    let ark = match build_ark(root, None) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("writer: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    ArkCli::builder()
        .name("Writer")
        .tagline("a developmental editor that reads your manuscript before it opines")
        .ark(ark)
        .project(manuscript_project)
        .theme(Theme::default().accent(Accent::Magenta).prompt("✎ "))
        .help("Ask about your manuscript in plain words: pacing, continuity, voice, a rewrite.\nThe editor reads the chapters first and asks before saving anything.")
        .command(Arc::new(ManuscriptCmd))
        .command(Arc::new(ChapterCmd))
        .command(Arc::new(StyleCmd))
        .labels(labels())
        .renderer(Arc::new(SaveRenderer))
        .status_section("Manuscript", |ark, project| {
            let ws = ark.engine().workspace_dir(&project.id).ok()?;
            let files = chapter_files(&ws);
            let words: u64 = files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).map(|t| word_count(&t)).sum();
            Some(format!("{} chapters · {words} words · {}", files.len(), ws.display()))
        })
        .run()
        .await
}

// ---- helpers ---------------------------------------------------------------

fn chapter_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| e == "md" || e == "txt")
                .unwrap_or(false)
        })
        .filter(|p| !file_name(p).eq_ignore_ascii_case("readme.md"))
        .collect();
    files.sort();
    files
}

/// `4` → `chapter-04.md`; also accepts a filename.
fn find_chapter(dir: &Path, arg: &str) -> Option<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    let files: Vec<String> = chapter_files(dir).iter().map(|p| file_name(p)).collect();
    if files.iter().any(|f| f == arg) {
        return Some(arg.to_string());
    }
    let n: u32 = arg.parse().ok()?;
    files.into_iter().find(|f| {
        f.chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u32>()
            .ok()
            == Some(n)
    })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn title(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

fn file_schema(desc: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object", "required": ["file"],
        "properties": { "file": { "type": "string", "description": desc } }
    })
}

fn req_str(input: &serde_json::Value, key: &str) -> ark_tools::Result<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            ark_tools::AdapterError::bad_input(
                "manuscript",
                format!("missing string field `{key}`"),
            )
        })
}

fn confine(file: &str) -> ark_tools::Result<()> {
    if file.contains("..") || file.starts_with('/') || file.contains('\\') {
        return Err(ark_tools::AdapterError::bad_input(
            "manuscript",
            "file must be a plain name inside the manuscript",
        ));
    }
    Ok(())
}

fn read_chapter(ws: &Path, file: &str) -> ark_tools::Result<String> {
    confine(file)?;
    std::fs::read_to_string(ws.join(file))
        .map_err(|e| ark_tools::AdapterError::Tool(format!("read {file}: {e}")))
}

fn word_count(text: &str) -> u64 {
    text.split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Completion, MockProvider, ToolCall, Usage};

    fn mock(script: Vec<Completion>) -> Option<Arc<dyn ark_model::Provider>> {
        Some(Arc::new(MockProvider::new("mock", script)))
    }

    #[tokio::test]
    async fn editor_agent_counts_a_chapter_and_checks_continuity() {
        let dir = tempfile::tempdir().unwrap();
        let c1 = ToolCall {
            id: "c1".into(),
            name: "manuscript.wordcount".into(),
            arguments: serde_json::json!({ "file": "chapter-01.md" }),
        };
        let c2 = ToolCall {
            id: "c2".into(),
            name: "craft.continuity".into(),
            arguments: serde_json::json!({ "term": "locket" }),
        };
        let script = vec![
            // The planner runs first (the task has an " and " marker → an investigation).
            Completion {
                content: "measure chapter 1 length\ncheck whether the locket recurs".into(),
                tool_calls: vec![],
                usage: Usage::measured(40, 10),
            },
            Completion {
                content: String::new(),
                tool_calls: vec![c1],
                usage: Usage::measured(200, 12),
            },
            Completion {
                content: String::new(),
                tool_calls: vec![c2],
                usage: Usage::measured(220, 12),
            },
            Completion {
                content:
                    "Chapter 1 runs about 50 words [inv:1]; the locket recurs in chapter 2 [inv:2]."
                        .into(),
                tool_calls: vec![],
                usage: Usage::measured(240, 30),
            },
        ];
        let ark = build_ark(dir.path(), mock(script)).unwrap();
        let project = ark.create_project("the-locket", "a debut novel").unwrap();
        seed_manuscript(&ark.engine().workspace_dir(&project.id).unwrap()).unwrap();

        let outcome = ark
            .ask(
                &project.id,
                "how long is chapter 1 and does the locket recur?",
            )
            .await
            .unwrap();

        assert_eq!(outcome.metrics.tool_calls, 2, "both manuscript tools ran");
        assert!(
            outcome.answer.contains("locket"),
            "answer: {}",
            outcome.answer
        );
        let cards = ark.engine().store.list_evidence_cards(&project.id).unwrap();
        assert!(
            cards.iter().any(|c| c.summary.contains("words in")),
            "distiller summaries: {:?}",
            cards.iter().map(|c| &c.summary).collect::<Vec<_>>()
        );
    }

    #[test]
    fn word_count_ignores_whitespace_noise() {
        assert_eq!(word_count("one two   three\n\nfour"), 4);
    }

    #[test]
    fn voice_profile_reads_the_sample_narrator() {
        let dir = tempfile::tempdir().unwrap();
        seed_manuscript(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("chapter-04.md")).unwrap();
        let v = voice_profile(&text);
        assert_eq!(v["person"], "first");
        assert_eq!(v["tense_lean"], "past");
        assert_eq!(
            find_chapter(dir.path(), "4").as_deref(),
            Some("chapter-04.md")
        );
    }

    #[tokio::test]
    async fn save_is_gated_behind_approval() {
        let dir = tempfile::tempdir().unwrap();
        let save = ToolCall {
            id: "c1".into(),
            name: "manuscript.save".into(),
            arguments: serde_json::json!({ "file": "new.md", "text": "hello" }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![save],
                usage: Usage::measured(80, 8),
            },
            Completion {
                content: "I need approval to write that file.".into(),
                tool_calls: vec![],
                usage: Usage::measured(90, 9),
            },
        ];
        let ark = build_ark(dir.path(), mock(script)).unwrap();
        let project = ark.create_project("t", "").unwrap();
        let _ = ark.ask(&project.id, "save a new chapter").await.unwrap();
        let ws = ark.engine().workspace_dir(&project.id).unwrap();
        assert!(
            !ws.join("new.md").exists(),
            "save must be gated behind human approval"
        );
    }

    #[tokio::test]
    async fn approved_save_in_a_session_writes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let save = ToolCall {
            id: "c1".into(),
            name: "manuscript.save".into(),
            arguments: serde_json::json!({ "file": "chapter-05.md", "text": "# Chapter 5\n\nI went back." }),
        };
        let script = vec![
            Completion {
                content: String::new(),
                tool_calls: vec![save],
                usage: Usage::measured(80, 8),
            },
            Completion {
                content: "Saved chapter 5.".into(),
                tool_calls: vec![],
                usage: Usage::measured(90, 9),
            },
        ];
        let ark = build_ark(dir.path(), mock(script)).unwrap();
        let project = ark.create_project("t", "").unwrap();
        let mut session = ark.start_session(&project.id).unwrap();
        let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
            ApprovalDecision::AllowOnce,
        ))));
        let opts = TurnOptions {
            approvals: Some(gate),
            ..Default::default()
        };
        let out = ark
            .ask_session(&mut session, "write chapter 5", opts, |_| {})
            .await
            .unwrap();
        assert_eq!(out.answer, "Saved chapter 5.");
        let ws = ark.engine().workspace_dir(&project.id).unwrap();
        assert!(ws.join("chapter-05.md").exists());
        assert_eq!(session.turn_count(), 1);
    }
}
