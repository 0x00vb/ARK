//! The generic workload suite. Each workload exercises one harness behavior; together they
//! cover trivial work, tool discovery, dependent steps, large outputs, long sessions, failure
//! recovery, repetition, runaway loops, resume across a restart, retrieval from a corpus,
//! approvals, independent slow calls and an answer that names only files that were written. Nothing here is specific to an application domain.

mod claims;
pub mod tools;
mod web;

pub use claims::{claimed_files, ClaimsSpec};
pub use web::{web, WebSpec};

use std::sync::Arc;

use ark_sdk::prelude::*;
use serde_json::json;

use crate::sim::{cite_all, competent, word_after, Goal, Policy, Reply, SimRequest};
use crate::workload::{Env, Mode, RunOutput, TurnSpec, Verdict, Workload};

type Turns = Arc<dyn Fn(Mode) -> Vec<TurnSpec> + Send + Sync>;
type Prepare = Arc<dyn Fn(&Ark) -> anyhow::Result<ProjectId> + Send + Sync>;
type Check = Arc<dyn Fn(&RunOutput) -> Verdict + Send + Sync>;

/// A workload assembled from closures.
pub struct Spec {
    pub name: &'static str,
    pub class: &'static str,
    pub description: &'static str,
    turns: Turns,
    policy: Policy,
    prepare: Option<Prepare>,
    check: Check,
}

impl Workload for Spec {
    fn name(&self) -> &str {
        self.name
    }
    fn class(&self) -> &str {
        self.class
    }
    fn description(&self) -> &str {
        self.description
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        (self.turns)(mode)
    }
    fn policy(&self) -> Policy {
        self.policy.clone()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        Ok(builder(env).build()?)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        match &self.prepare {
            Some(p) => p(ark),
            None => Ok(ark.find_or_create_project("bench", "")?.id),
        }
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        (self.check)(out)
    }
}

/// The suite's agent: every simulated tool, `calc.eval` preloaded.
fn builder(env: &Env) -> ArkBuilder {
    let mut registry = Registry::new();
    for a in tools::all() {
        registry.register(a);
    }
    env.builder()
        .registry(registry)
        .profile(AgentProfile::builder().core_actions(["calc.eval"]).build())
}

fn one(input: &'static str) -> Turns {
    Arc::new(move |_| vec![TurnSpec::new(input)])
}

fn answer(f: impl Fn(&SimRequest) -> String + Send + Sync + 'static) -> crate::sim::Answer {
    Arc::new(f)
}

/// Replies every hand-written policy shares: the planner, the critic and a forced answer.
fn housekeeping(r: &SimRequest) -> Option<Reply> {
    if r.is_planner() {
        return Some(Reply::Text("- find the tool\n- run it\n- answer".into()));
    }
    if r.is_critic() {
        return Some(Reply::Text(r.draft()));
    }
    if r.tools.is_empty() {
        return Some(Reply::Text(format!(
            "Best answer from what I have. {}",
            cite_all(r)
        )));
    }
    None
}

fn has(s: &str, needle: &str) -> bool {
    s.to_lowercase().contains(&needle.to_lowercase())
}

/// Every workload in the generic suite. `long_turns` sets the long-session length.
pub fn all(long_turns: usize) -> Vec<Box<dyn Workload>> {
    vec![
        Box::new(simple()),
        Box::new(tool_lookup()),
        Box::new(multi_step()),
        Box::new(large_output()),
        Box::new(long_session(long_turns)),
        Box::new(failure_recovery()),
        Box::new(duplicate()),
        Box::new(runaway()),
        Box::new(resume()),
        Box::new(retrieval()),
        Box::new(approval()),
        Box::new(parallel()),
        Box::new(stale_read()),
        Box::new(team()),
        Box::new(skill()),
        Box::new(web()),
        Box::new(claimed_files()),
    ]
}

/// A workload run by several agents: [`Workload::team`] lists them, the first gets each turn
/// as a task.
pub struct TeamSpec {
    pub spec: Spec,
    pub agents: Vec<ark_orchestrate::AgentSpec>,
}

impl Workload for TeamSpec {
    fn name(&self) -> &str {
        self.spec.name()
    }
    fn class(&self) -> &str {
        self.spec.class()
    }
    fn description(&self) -> &str {
        self.spec.description()
    }
    fn turns(&self, mode: Mode) -> Vec<TurnSpec> {
        self.spec.turns(mode)
    }
    fn policy(&self) -> Policy {
        self.spec.policy()
    }
    fn build(&self, env: &Env) -> anyhow::Result<Ark> {
        self.spec.build(env)
    }
    fn prepare(&self, ark: &Ark) -> anyhow::Result<ProjectId> {
        self.spec.prepare(ark)
    }
    fn check(&self, out: &RunOutput) -> Verdict {
        self.spec.check(out)
    }
    fn team(&self) -> Vec<ark_orchestrate::AgentSpec> {
        self.agents.clone()
    }
}

/// Trivial work: no tool is needed. Tests whether the harness overthinks.
pub fn simple() -> Spec {
    Spec {
        name: "simple",
        class: "simple",
        description: "say hello in French; no tool needed",
        turns: one("Say hello in French."),
        policy: competent(|_| vec![], answer(|_| "Bonjour !".into())),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "bonjour"), "answer is not 'bonjour'"),
                (
                    o.executed("calc.eval") == 0,
                    "ran a tool for a trivial task",
                ),
            ])
        }),
    }
}

/// One tool that is not preloaded: the agent has to find and load it.
pub fn tool_lookup() -> Spec {
    Spec {
        name: "tool-lookup",
        class: "tool-lookup",
        description:
            "convert currency with a tool found among 25 actions; only the tool knows the rate",
        turns: one("Convert 250 US dollars to euros at the converter's current rate."),
        policy: competent(
            |_| {
                vec![Goal::call(
                    "units.convert",
                    json!({ "value": 250, "from": "usd", "to": "eur" }),
                )]
            },
            answer(|r| {
                match r
                    .result_of("units.convert")
                    .and_then(|t| word_after(t, "result="))
                {
                    Some(v) => format!("250 USD is {v} EUR {}", cite_all(r)),
                    None => "I could not convert it; the converter was unavailable.".into(),
                }
            }),
        ),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (
                    has(o.answer(), "230"),
                    "wrong or missing result (expected 230 EUR)",
                ),
                (o.executed("units.convert") >= 1, "never ran units.convert"),
            ])
        }),
    }
}

/// The format of the answer is in a stored skill: the agent sees one line about it, reads its
/// instructions with `skills.read`, and follows them.
pub fn skill() -> Spec {
    Spec {
        name: "skill",
        class: "skill",
        description: "write release notes in the format a stored skill sets; only the skill says what it is",
        turns: one("Write release notes for these changes: added an export button, fixed the login timeout."),
        policy: competent(
            |r| {
                if r.mentions("== SKILLS ==") {
                    vec![Goal::call("skills.read", json!({ "name": "release-notes" }))]
                } else {
                    Vec::new()
                }
            },
            answer(|r| match r.result_of("skills.read") {
                Some(t) if t.contains("format R7") => {
                    "Release notes, format R7\nAdded: an export button\nFixed: the login timeout".into()
                }
                _ => "Release notes: added an export button; fixed the login timeout.".into(),
            }),
        ),
        prepare: Some(Arc::new(|ark: &Ark| {
            let dir = ark.engine().store.root().join("skills").join("release-notes");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("SKILL.md"),
                "---\nname: release-notes\ndescription: Write release notes from a list of changes. Use for any release notes.\n---\n\
Begin with the line `Release notes, format R7`.\n\
Then one line per change, starting with `Added:`, `Fixed:` or `Changed:`.\n",
            )?;
            Ok(ark.find_or_create_project("bench", "")?.id)
        })),
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "format R7"), "did not follow the skill's format"),
                (o.executed("skills.read") >= 1, "never read the skill"),
            ])
        }),
    }
}

/// Several dependent calls: each needs a value from the previous result.
pub fn multi_step() -> Spec {
    Spec {
        name: "multi-step",
        class: "multi-step",
        description: "find a customer, list her orders, add the amounts",
        turns: one("Find the customer named Ada Lovelace, then add up the amounts of all her orders and tell me the total."),
        policy: competent(
            order_total_goals,
            answer(|r| match r.result_of("calc.eval").and_then(|t| word_after(t, "value=")) {
                Some(v) => format!("Ada Lovelace's orders total {v} {}", cite_all(r)),
                None => "I could not total the orders: the amounts were not available.".into(),
            }),
        ),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "565.74"), "wrong or missing total (expected 565.74)"),
                (o.executed("crm.find_customer") == 1, "looked the customer up more than once"),
            ])
        }),
    }
}

/// Find Ada Lovelace, list her orders, and add the amounts: each step needs the last result.
fn order_total_goals(r: &SimRequest) -> Vec<Goal> {
    let mut g = vec![Goal::call(
        "crm.find_customer",
        json!({ "name": "Ada Lovelace" }),
    )];
    let Some(id) = r
        .result_of("crm.find_customer")
        .and_then(|t| word_after(t, "id="))
    else {
        return g;
    };
    g.push(Goal::call("crm.orders", json!({ "customer_id": id })));
    g.push(Goal::find("\"amount\"", "crm.orders", "amount"));
    let amounts: Vec<String> = r
        .results()
        .iter()
        .flat_map(|(_, t)| t.split("\"amount\":").skip(1))
        .filter_map(|rest| {
            let n: String = rest
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!n.is_empty()).then_some(n)
        })
        .collect();
    if !amounts.is_empty() {
        g.push(Goal::call(
            "calc.eval",
            json!({ "expr": amounts.join("+") }),
        ));
    }
    g
}

/// A 3,000-line output where one line answers the question.
pub fn large_output() -> Spec {
    Spec {
        name: "large-output",
        class: "large-output",
        description: "find the one error code in 3,000 log lines",
        turns: one("Which error code did the payments service log today?"),
        policy: competent(
            |_| {
                vec![
                    Goal::call("logs.fetch", json!({ "service": "payments" })),
                    Goal::find("code=E4721", "logs.fetch", "ERROR"),
                ]
            },
            answer(|r| {
                if r.results().iter().any(|(_, t)| t.contains("code=E4721")) {
                    format!(
                        "The payments service logged error code E4721 {}",
                        cite_all(r)
                    )
                } else {
                    "Unknown: the log was too large to find the error line.".into()
                }
            }),
        ),
        prepare: None,
        check: Arc::new(|o| Verdict::all(&[(has(o.answer(), "E4721"), "did not find E4721")])),
    }
}

/// Many turns in one session: the prompt must stay flat while the transcript grows.
pub fn long_session(turns: usize) -> Spec {
    Spec {
        name: "long-session",
        class: "long-session",
        description: "many short turns, each with one tool call; context must not grow",
        turns: Arc::new(move |mode| {
            let n = if mode == Mode::Live {
                turns.min(12)
            } else {
                turns
            };
            (1..=n)
                .map(|i| {
                    TurnSpec::new(format!(
                        "turn {i}: what is {i} times 7 plus 3? keep going after this"
                    ))
                })
                .collect()
        }),
        policy: competent(
            |r| {
                let task = r.task();
                let i: String = task
                    .trim_start_matches("turn ")
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                vec![Goal::call(
                    "calc.eval",
                    json!({ "expr": format!("{i}*7+3") }),
                )]
            },
            answer(|r| {
                match r
                    .result_of("calc.eval")
                    .and_then(|t| word_after(t, "value="))
                {
                    Some(v) => format!("It is {v}. {} Ready for the next one.", cite_all(r)),
                    None => "I could not compute it.".into(),
                }
            }),
        ),
        prepare: None,
        check: Arc::new(|o| {
            let wrong = o
                .turns
                .iter()
                .enumerate()
                .filter(|(i, t)| {
                    let v = ((i + 1) * 7 + 3) as f64;
                    !t.answer.contains(&format!("{v}"))
                })
                .count();
            Verdict::all(&[(wrong == 0, "some turns answered wrong")])
        }),
    }
}

/// The first tool fails every time; the agent must switch, not loop.
pub fn failure_recovery() -> Spec {
    Spec {
        name: "failure-recovery",
        class: "failure-recovery",
        description: "primary mirror is down; retry at most once, then use the backup",
        turns: one(
            "Fetch release.txt from the mirror (try the primary first; there is a backup) and tell me the version it lists.",
        ),
        policy: Arc::new(|r: &SimRequest| {
            if let Some(reply) = housekeeping(r) {
                return reply;
            }
            let failed = r
                .results()
                .iter()
                .filter(|(a, t)| *a == "mirror.primary" && t.contains("failed"))
                .count();
            // Try the primary, retry it once the way a model does after an error, then switch.
            let next = if failed < 2 {
                Some(("mirror.primary", r.count_calls("mirror.primary")))
            } else if r.result_of("mirror.backup").is_none() {
                Some(("mirror.backup", r.count_calls("mirror.backup")))
            } else {
                None
            };
            match next {
                Some((action, _)) if !r.has_tool(action) => {
                    Reply::call("load_action", json!({ "name": action }))
                }
                Some((action, _)) => Reply::call(action, json!({ "path": "release.txt" })),
                None => match r
                    .result_of("mirror.backup")
                    .and_then(|t| word_after(t, "version="))
                {
                    Some(v) => Reply::Text(format!("The release is version {v} {}", cite_all(r))),
                    None => Reply::Text(
                        "The primary mirror is down; I could not fetch the file.".into(),
                    ),
                },
            }
        }),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "4.2.1"), "did not recover the version"),
                (
                    o.executed("mirror.primary") <= 3,
                    "kept retrying the failing mirror",
                ),
            ])
        }),
    }
}

/// The model asks for the same call three times; the harness should run it once.
pub fn duplicate() -> Spec {
    Spec {
        name: "duplicate",
        class: "repetition",
        description: "the model repeats an identical lookup three times",
        turns: one("Look up the customer named Ada Lovelace and tell me her tier."),
        policy: Arc::new(|r: &SimRequest| {
            if r.is_planner() {
                return Reply::Text("- look up the customer".into());
            }
            if r.tools.is_empty() {
                return Reply::Text(r.draft());
            }
            let call = json!({ "name": "Ada Lovelace" });
            if !r.has_tool("crm.find_customer") {
                return Reply::call("load_action", json!({ "name": "crm.find_customer" }));
            }
            if r.count_calls("crm.find_customer") < 3 {
                return Reply::call("crm.find_customer", call);
            }
            match r
                .result_of("crm.find_customer")
                .and_then(|t| word_after(t, "tier="))
            {
                Some(t) => Reply::Text(format!("Her tier is {t} {}", cite_all(r))),
                None => Reply::Text("Unknown tier.".into()),
            }
        }),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "gold"), "wrong tier"),
                (
                    o.executed("crm.find_customer") <= 1,
                    "ran the identical lookup more than once",
                ),
            ])
        }),
    }
}

/// The model never stops: it repeats the same search forever. The loop must end by itself.
pub fn runaway() -> Spec {
    Spec {
        name: "runaway",
        class: "repetition",
        description: "the model repeats one search forever; the stall guard must stop it",
        turns: one("Search the stored evidence for anything about the Q3 budget and summarize it."),
        policy: Arc::new(|r: &SimRequest| {
            if r.is_planner() {
                return Reply::Text("- search the evidence".into());
            }
            if r.tools.is_empty() {
                return Reply::Text("Nothing about the Q3 budget is stored; unknown.".into());
            }
            Reply::call("search_evidence", json!({ "query": "Q3 budget" }))
        }),
        prepare: None,
        check: Arc::new(|o| {
            let steps = o.last().steps;
            Verdict::all(&[
                (!o.answer().is_empty(), "no answer at all"),
                (steps <= 6, "the loop ran too many steps before stopping"),
            ])
        }),
    }
}

/// A fact found in turn 1 is needed in turn 2, after a process restart.
pub fn resume() -> Spec {
    Spec {
        name: "resume",
        class: "resume",
        description: "look up a customer, restart, then ask about the same customer",
        turns: Arc::new(|_| {
            vec![
                TurnSpec::new(
                    "Look up the customer named Ada Lovelace and tell me her customer id.",
                ),
                TurnSpec::after_restart("Which tier is that customer in?"),
            ]
        }),
        policy: competent(
            |r| {
                if r.task().contains("tier")
                    && word_after(&r.visible(), "tier=").as_deref() == Some("gold")
                {
                    return vec![];
                }
                vec![Goal::call(
                    "crm.find_customer",
                    json!({ "name": "Ada Lovelace" }),
                )]
            },
            answer(|r| {
                let t = r.visible();
                if r.task().contains("tier") {
                    match word_after(&t, "tier=") {
                        Some(tier) => format!("Ada Lovelace is in the {tier} tier {}", cite_all(r)),
                        None => "I do not know her tier.".into(),
                    }
                } else {
                    match word_after(&t, "id=") {
                        Some(id) => format!("Her customer id is {id} {}", cite_all(r)),
                        None => "I could not find her.".into(),
                    }
                }
            }),
        ),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(&o.turns[0].answer, "C-1042"), "turn 1 missed the id"),
                (
                    o.turns.len() == 2 && has(&o.turns[1].answer, "gold"),
                    "turn 2 missed the tier",
                ),
            ])
        }),
    }
}

/// One relevant document among 120: find it by search, then read it.
pub fn retrieval() -> Spec {
    Spec {
        name: "retrieval",
        class: "retrieval",
        description: "find a warranty period in one of 120 stored product documents",
        turns: one("What is the warranty period for the Model K7 kettle? Check the stored product documents."),
        policy: competent(
            |r| {
                let mut g = vec![Goal::call("search_evidence", json!({ "query": "K7 kettle warranty" }))];
                // The matching lines may already answer; then there is nothing to open.
                if r.results().iter().any(|(_, t)| t.contains("18 months")) {
                    return g;
                }
                let Some(hit) = r
                    .result_of("search_evidence")
                    .and_then(|t| word_after(t, "art:"))
                else {
                    return g;
                };
                g.push(Goal::call("docs.open", json!({ "artifact": format!("art:{hit}") })));
                g.push(Goal::find("18 months", "docs.open", "warranty"));
                g
            },
            answer(|r| {
                if r.results().iter().any(|(_, t)| t.contains("18 months")) {
                    format!("The Model K7 kettle has an 18 months warranty {}", cite_all(r))
                } else {
                    "Unknown: I could not find the warranty in the documents.".into()
                }
            }),
        ),
        prepare: Some(Arc::new(|ark: &Ark| {
            let p = ark.find_or_create_project("bench", "")?;
            for i in 0..120 {
                let (name, body) = if i == 77 {
                    (
                        "manual-k7-kettle.txt".to_string(),
                        "Model K7 kettle. Capacity 1.7 litres. Boils in 3 minutes. The K7 kettle warranty: 18 months from purchase, parts and labour. Descale monthly.".to_string(),
                    )
                } else {
                    (
                        format!("manual-{i:03}.txt"),
                        format!("Model X{i} toaster oven. Capacity {} litres. Warranty: {} months. Clean the crumb tray weekly. Do not immerse in water.", 10 + i % 7, 12 + (i % 3) * 12),
                    )
                };
                ark.engine().ingest_bytes(
                    &p.id,
                    &name,
                    ArtifactKind::Text,
                    body.as_bytes(),
                    Provenance::Ingested { path: name.clone() },
                )?;
            }
            Ok(p.id)
        })),
        check: Arc::new(|o| Verdict::all(&[(has(o.answer(), "18 months"), "wrong or missing warranty")])),
    }
}

/// A state-changing call that needs approval.
pub fn approval() -> Spec {
    Spec {
        name: "approval",
        class: "approval",
        description: "save a note; the write goes through the approval gate",
        turns: one("Save a note that says: ship the release on Thursday."),
        policy: competent(
            |_| {
                vec![Goal::call(
                    "notes.write",
                    json!({ "text": "ship the release on Thursday" }),
                )]
            },
            answer(|r| match r.result_of("notes.write") {
                Some(t) if !t.contains("failed") => format!("Saved the note {}", cite_all(r)),
                _ => "I could not save the note.".into(),
            }),
        ),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (
                    o.executed("notes.write") == 1,
                    "the note was not written exactly once",
                ),
                (has(o.answer(), "saved"), "answer does not confirm the save"),
            ])
        }),
    }
}

/// Three independent slow calls the model asks for in one step.
pub fn parallel() -> Spec {
    Spec {
        name: "parallel",
        class: "parallel",
        description: "three independent slow lookups requested together",
        turns: one("What is the current temperature in Paris, Tokyo and Lima?"),
        policy: Arc::new(|r: &SimRequest| {
            if r.is_planner() {
                return Reply::Text("- look up each city".into());
            }
            if r.is_critic() {
                return Reply::Text(r.draft());
            }
            let cities = ["Paris", "Tokyo", "Lima"];
            if !r.tools.is_empty() {
                if !r.has_tool("weather.current") {
                    return Reply::call("load_action", json!({ "name": "weather.current" }));
                }
                if r.count_calls("weather.current") == 0 {
                    return Reply::Calls(
                        cities
                            .iter()
                            .map(|c| ("weather.current".to_string(), json!({ "city": c })))
                            .collect(),
                    );
                }
            }
            let temps: Vec<String> = r
                .results()
                .iter()
                .filter(|(a, _)| *a == "weather.current")
                .filter_map(|(_, t)| {
                    Some(format!(
                        "{} {}°C",
                        word_after(t, "city=")?,
                        word_after(t, "temp_c=")?
                    ))
                })
                .collect();
            Reply::Text(format!("{} {}", temps.join(", "), cite_all(r)))
        }),
        prepare: None,
        check: Arc::new(|o| {
            let a = o.answer();
            Verdict::all(&[(
                has(a, "14") && has(a, "22") && has(a, "18"),
                "missing a city's temperature",
            )])
        }),
    }
}

/// Read, change, read again: the second read must see the change, not a reused result.
pub fn stale_read() -> Spec {
    Spec {
        name: "stale-read",
        class: "reuse",
        description: "read a setting, change it, read it again",
        turns: one("Read the `mode` setting, set it to `fast`, then read it again and tell me its value now."),
        policy: Arc::new(|r: &SimRequest| {
            if let Some(reply) = housekeeping(r) {
                return reply;
            }
            let key = json!({ "key": "mode" });
            let gets = r.count_calls("settings.get");
            let sets = r.count_calls("settings.set");
            let next = if gets == 0 {
                Some(("settings.get", key))
            } else if sets == 0 {
                Some(("settings.set", json!({ "key": "mode", "value": "fast" })))
            } else if gets == 1 {
                Some(("settings.get", key))
            } else {
                None
            };
            match next {
                Some((a, _)) if !r.has_tool(a) => Reply::call("load_action", json!({ "name": a })),
                Some((a, args)) => Reply::call(a, args),
                None => {
                    let v = r
                        .results()
                        .iter()
                        .rev()
                        .filter(|(a, _)| *a == "settings.get")
                        .find_map(|(_, t)| word_after(t, "value="))
                        .unwrap_or_else(|| "unknown".into());
                    Reply::Text(format!("The mode is now {v} {}", cite_all(r)))
                }
            }
        }),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "fast"), "reported a stale value"),
                (o.executed("settings.set") == 1, "the change was not made exactly once"),
            ])
        }),
    }
}

/// Two independent pieces of work: a lead hands one to each of two workers (they run in
/// parallel, each in its own context with only its own tools) and combines what they report.
/// The naive harness does it alone with every tool.
pub fn team() -> TeamSpec {
    use ark_orchestrate::AgentSpec;
    const A: &str = "Find the customer Ada Lovelace: her tier, and the total of all her orders.";
    const B: &str = "Get the current temperature in Paris and in Tokyo.";
    let weather = |_: &SimRequest| {
        vec![
            Goal::call("weather.current", json!({ "city": "Paris" })),
            Goal::call("weather.current", json!({ "city": "Tokyo" })),
        ]
    };
    let customer_answer = |r: &SimRequest| {
        let tier = r
            .result_of("crm.find_customer")
            .and_then(|t| word_after(t, "tier="))
            .unwrap_or_else(|| "unknown".into());
        let total = r
            .result_of("calc.eval")
            .and_then(|t| word_after(t, "value="))
            .unwrap_or_else(|| "unknown".into());
        format!("Ada Lovelace is {tier}; her orders total {total}.")
    };
    let weather_answer = |r: &SimRequest| {
        let temps: Vec<String> = r
            .results()
            .iter()
            .filter(|(a, _)| *a == "weather.current")
            .filter_map(|(_, t)| {
                Some(format!(
                    "{} {}°C",
                    word_after(t, "city=")?,
                    word_after(t, "temp_c=")?
                ))
            })
            .collect();
        temps.join(", ")
    };
    let worker_a = competent(
        order_total_goals,
        answer(move |r| format!("{} {}", customer_answer(r), cite_all(r))),
    );
    let worker_b = competent(
        weather,
        answer(move |r| format!("{} {}", weather_answer(r), cite_all(r))),
    );
    let solo = competent(
        move |r| {
            let mut g = order_total_goals(r);
            g.extend(weather(r));
            g
        },
        answer(move |r| {
            format!(
                "{} {} {}",
                customer_answer(r),
                weather_answer(r),
                cite_all(r)
            )
        }),
    );
    let lead = |r: &SimRequest| {
        if let Some(reply) = housekeeping_lead(r) {
            return reply;
        }
        if r.count_calls("agents.delegate") == 0 {
            return Reply::Calls(vec![
                (
                    "agents.delegate".into(),
                    json!({ "to": "worker-a", "objective": A }),
                ),
                (
                    "agents.delegate".into(),
                    json!({ "to": "worker-b", "objective": B }),
                ),
            ]);
        }
        Reply::Text(team_answer(r))
    };
    Spec {
        name: "team",
        class: "orchestration",
        description: "a lead delegates two independent lookups to two workers and combines them",
        turns: one("Two things: Ada Lovelace's customer tier and the total of all her orders; and the current temperature in Paris and in Tokyo."),
        policy: Arc::new(move |r: &SimRequest| {
            if r.system.contains("agent \"worker-a\"") {
                worker_a(r)
            } else if r.system.contains("agent \"worker-b\"") {
                worker_b(r)
            } else if r.system.contains("agent \"lead\"") {
                lead(r)
            } else {
                solo(r)
            }
        }),
        prepare: None,
        check: Arc::new(|o| {
            Verdict::all(&[
                (has(o.answer(), "gold"), "no tier"),
                (has(o.answer(), "565.74"), "wrong or missing order total (expected 565.74)"),
                (has(o.answer(), "14"), "no temperature for Paris"),
                (has(o.answer(), "22"), "no temperature for Tokyo"),
            ])
        }),
    }
    .with_team(vec![
        AgentSpec::new("lead")
            .role("splits the work, delegates it and combines the results")
            .tools(Vec::<String>::new())
            .delegates_to(["worker-a", "worker-b"]),
        AgentSpec::new("worker-a")
            .role("looks up customers and their orders")
            .tools(["crm.*", "calc.*"])
            .parent("lead"),
        AgentSpec::new("worker-b")
            .role("looks up the weather")
            .tools(["weather.*"])
            .parent("lead"),
    ])
}

impl Spec {
    /// Run this workload with several agents (see [`TeamSpec`]).
    pub fn with_team(self, agents: Vec<ark_orchestrate::AgentSpec>) -> TeamSpec {
        TeamSpec { spec: self, agents }
    }
}

/// The lead's planner, critic and forced answer.
fn housekeeping_lead(r: &SimRequest) -> Option<Reply> {
    if r.is_planner() {
        return Some(Reply::Text(
            "- delegate the two lookups\n- combine the results".into(),
        ));
    }
    if r.is_critic() {
        return Some(Reply::Text(r.draft()));
    }
    if r.tools.is_empty() {
        return Some(Reply::Text(team_answer(r)));
    }
    None
}

/// What the workers reported, together.
fn team_answer(r: &SimRequest) -> String {
    let parts: Vec<&str> = r
        .results()
        .iter()
        .filter(|(a, _)| *a == "agents.delegate")
        .map(|(_, t)| *t)
        .collect();
    format!("Combined: {}", parts.join(" | "))
}
