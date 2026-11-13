//! Group chats: the records and the pure rules, the same for every front-end (ADR 0113).
//!
//! A group holds agents of one or more projects. A person's message goes to the members the
//! rules below pick; each answers in its own thread for the group, from a bounded brief.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub use ark_orchestrate::group::{Group, Member, SCOPE};

/// The most agent replies one person message may cause.
pub const MAX_REPLIES: usize = 6;
/// Messages the router sees, and the length of each.
const ROUTER_MESSAGES: usize = 6;
const ROUTER_CHARS: usize = 300;
/// Messages and characters one agent's brief holds; the newest are kept.
const BRIEF_MESSAGES: usize = 20;
const BRIEF_CHARS: usize = 6000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMessage {
    pub id: String,
    pub group: String,
    /// `None`: the person.
    pub from: Option<Member>,
    pub text: String,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

/// A live member as routing and prompts see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub member: Member,
    /// The agent's display name (its title, else its id).
    pub name: String,
    pub role: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Mention,
    Single,
    Router,
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Chosen(Vec<Member>, By),
    /// Stages 1 and 2 did not decide: ask the router, then call [`from_router`].
    AskRouter,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// An id that sorts by time: `<ms>-<counter>`.
pub fn sortable_id(prefix: &str, at_ms: u64) -> String {
    format!(
        "{prefix}{at_ms:013}-{:06}",
        SEQ.fetch_add(1, Ordering::Relaxed) % 1_000_000
    )
}

fn same(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Whether `name` is at `at` in `chars`, ignoring case, ending at a word boundary.
fn name_at(chars: &[char], at: usize, name: &str) -> Option<usize> {
    let want: Vec<char> = name.chars().collect();
    if want.is_empty() || at + want.len() > chars.len() {
        return None;
    }
    if !want.iter().zip(&chars[at..]).all(|(a, b)| same(*a, *b)) {
        return None;
    }
    let end = at + want.len();
    (end == chars.len() || !is_word(chars[end])).then_some(end - at)
}

fn names_of(c: &Candidate) -> [&str; 2] {
    [c.member.agent.as_str(), c.name.as_str()]
}

/// Members named in `text`, in order of first mention. With `at_only`, a name counts only
/// after `@`; otherwise anywhere at a word boundary (a router's answer).
fn named(text: &str, roster: &[Candidate], at_only: bool) -> Vec<Member> {
    let chars: Vec<char> = text.chars().collect();
    let mut found: Vec<Member> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start_ok = i == 0 || !is_word(chars[i - 1]);
        let (from, ok) = if at_only {
            (
                i + 1,
                chars[i] == '@' && (i == 0 || !chars[i - 1].is_alphanumeric()),
            )
        } else {
            (i, start_ok)
        };
        let mut hit: Option<(usize, &Candidate)> = None;
        if ok {
            for c in roster {
                for n in names_of(c) {
                    if let Some(len) = name_at(&chars, from, n) {
                        if hit.is_none_or(|(l, _)| len > l) {
                            hit = Some((len, c));
                        }
                    }
                }
            }
        }
        if let Some((len, c)) = hit {
            if !found.contains(&c.member) {
                found.push(c.member.clone());
            }
            i = from + len;
        } else {
            i += 1;
        }
    }
    found
}

/// Members the text mentions with `@Name` (agent id or display name), in order. Unknown
/// names are ignored.
pub fn mentions(text: &str, roster: &[Candidate]) -> Vec<Member> {
    named(text, roster, true)
}

/// Stages 1 and 2 of routing.
pub fn route(text: &str, roster: &[Candidate]) -> Route {
    let chosen = mentions(text, roster);
    if !chosen.is_empty() {
        return Route::Chosen(chosen, By::Mention);
    }
    if let [only] = roster {
        return Route::Chosen(vec![only.member.clone()], By::Single);
    }
    Route::AskRouter
}

/// Stages 3 and 4: the router's answer names members, else the last agent that spoke, else
/// the first member.
pub fn from_router(
    answer: &str,
    roster: &[Candidate],
    last_speaker: Option<&Member>,
) -> (Vec<Member>, By) {
    let chosen = named(answer, roster, false);
    if !chosen.is_empty() {
        return (chosen, By::Router);
    }
    let fallback = last_speaker
        .filter(|m| roster.iter().any(|c| &c.member == *m))
        .cloned()
        .or_else(|| roster.first().map(|c| c.member.clone()));
    (fallback.into_iter().collect(), By::Fallback)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("...");
    out
}

fn name_of<'a>(from: &'a Option<Member>, roster: &'a [Candidate]) -> &'a str {
    match from {
        None => "Person",
        Some(m) => roster
            .iter()
            .find(|c| &c.member == m)
            .map_or(m.agent.as_str(), |c| c.name.as_str()),
    }
}

/// The router's system prompt and user message: members with their role and the last
/// messages, bounded.
pub fn router_prompt(roster: &[Candidate], messages: &[GroupMessage]) -> (String, String) {
    let system = "You route a message in a group chat to the members who should answer. \
        Answer with the member names only, separated by commas. Pick one member unless \
        several are needed."
        .to_string();
    let mut user = String::from("Members:\n");
    for c in roster {
        let role = if c.role.is_empty() {
            "no role given"
        } else {
            &c.role
        };
        user.push_str(&format!("- {}: {}\n", c.name, clip(role, ROUTER_CHARS)));
    }
    user.push_str("\nRecent messages:\n");
    let skip = messages.len().saturating_sub(ROUTER_MESSAGES);
    for m in &messages[skip..] {
        user.push_str(&format!(
            "{}: {}\n",
            name_of(&m.from, roster),
            clip(&m.text, ROUTER_CHARS)
        ));
    }
    user.push_str("\nWho answers the last message?");
    (system, user)
}

/// The objective of a member's turn: the messages since its cursor as `Name: text` lines,
/// without its own, at most 20 messages and 6000 characters, the newest kept.
pub fn brief(
    roster: &[Candidate],
    messages: &[GroupMessage],
    member: &Member,
    cursor: Option<&str>,
) -> String {
    let unseen: Vec<&GroupMessage> = messages
        .iter()
        .filter(|m| cursor.is_none_or(|c| m.id.as_str() > c))
        .filter(|m| m.from.as_ref() != Some(member))
        .filter(|m| !m.failed)
        .collect();
    let skip = unseen.len().saturating_sub(BRIEF_MESSAGES);
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for m in unseen[skip..].iter().rev() {
        let line = format!("{}: {}", name_of(&m.from, roster), m.text);
        let len = line.chars().count() + 1;
        if used + len > BRIEF_CHARS {
            if lines.is_empty() {
                let keep = BRIEF_CHARS.saturating_sub(used);
                lines.push(
                    line.chars()
                        .skip(line.chars().count().saturating_sub(keep))
                        .collect(),
                );
            }
            break;
        }
        used += len;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

/// The context of a member's turn: who is in the group and how to answer.
pub fn context(roster: &[Candidate], member: &Member) -> String {
    let mut out = String::from("You are in a group chat with:\n");
    for c in roster {
        let you = if &c.member == member { " (you)" } else { "" };
        if c.role.is_empty() {
            out.push_str(&format!("- {}{you}\n", c.name));
        } else {
            out.push_str(&format!(
                "- {}{you}: {}\n",
                c.name,
                clip(&c.role, ROUTER_CHARS)
            ));
        }
    }
    out.push_str(
        "\nThe objective holds the messages you have not seen yet. Reply to the group only. \
         To bring another member in, write @Name.",
    );
    out
}

/// Members a reply hands on to: its `@Name` mentions, never the author.
pub fn hand_on(reply: &str, roster: &[Candidate], author: &Member) -> Vec<Member> {
    mentions(reply, roster)
        .into_iter()
        .filter(|m| m != author)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(project: &str, agent: &str, name: &str, role: &str) -> Candidate {
        Candidate {
            member: Member::new(project, agent),
            name: name.into(),
            role: role.into(),
        }
    }

    fn roster() -> Vec<Candidate> {
        vec![
            cand("p", "scout", "Scout", "finds things"),
            cand("p", "writer", "Code Writer", "writes code"),
            cand("q", "ana", "Ana", ""),
        ]
    }

    fn msg(id: &str, from: Option<&Member>, text: &str) -> GroupMessage {
        GroupMessage {
            id: id.into(),
            group: "g".into(),
            from: from.cloned(),
            text: text.into(),
            at_ms: 0,
            failed: false,
        }
    }

    #[test]
    fn mentions_match_id_or_title_in_order_without_duplicates() {
        let got = mentions(
            "hey @ana and @SCOUT, also @code writer; @ana again",
            &roster(),
        );
        let agents: Vec<&str> = got.iter().map(|m| m.agent.as_str()).collect();
        assert_eq!(agents, ["ana", "scout", "writer"]);
    }

    #[test]
    fn mentions_need_a_word_boundary_and_a_known_name() {
        assert!(mentions("@scouts and @nobody", &roster()).is_empty());
        assert!(mentions("mail me at x@scout.dev", &roster()).is_empty());
        assert_eq!(mentions("(@Ana)", &roster()).len(), 1);
        assert_eq!(mentions("@ana, ok", &roster()).len(), 1);
    }

    #[test]
    fn a_mention_decides_without_the_router() {
        assert_eq!(
            route("@Ana look", &roster()),
            Route::Chosen(vec![Member::new("q", "ana")], By::Mention)
        );
    }

    #[test]
    fn one_member_answers_alone_else_the_router_is_asked() {
        let one = &roster()[..1];
        assert_eq!(
            route("hi", one),
            Route::Chosen(vec![Member::new("p", "scout")], By::Single)
        );
        assert_eq!(route("hi", &roster()), Route::AskRouter);
    }

    #[test]
    fn the_router_answer_picks_members_else_the_fallback_applies() {
        let r = roster();
        let (got, by) = from_router("Scout, Ana", &r, None);
        assert_eq!((got.len(), by), (2, By::Router));
        let last = Member::new("q", "ana");
        let (got, by) = from_router("nobody knows", &r, Some(&last));
        assert_eq!((got, by), (vec![last.clone()], By::Fallback));
        let gone = Member::new("z", "gone");
        let (got, by) = from_router("", &r, Some(&gone));
        assert_eq!((got, by), (vec![Member::new("p", "scout")], By::Fallback));
    }

    #[test]
    fn the_router_prompt_is_bounded() {
        let long = "x".repeat(1000);
        let msgs: Vec<GroupMessage> = (0..10).map(|i| msg(&format!("{i}"), None, &long)).collect();
        let (_, user) = router_prompt(&roster(), &msgs);
        assert_eq!(user.matches("Person: ").count(), ROUTER_MESSAGES);
        assert!(user.chars().count() < 3000);
        assert!(user.contains("- Scout: finds things"));
    }

    #[test]
    fn a_brief_holds_unseen_messages_of_others_newest_kept() {
        let me = Member::new("p", "scout");
        let other = Member::new("q", "ana");
        let msgs = vec![
            msg("1", None, "old"),
            msg("2", Some(&other), "seen"),
            msg("3", Some(&me), "mine"),
            msg("4", None, "new question"),
            msg("5", Some(&other), "an answer"),
        ];
        let b = brief(&roster(), &msgs, &me, Some("2"));
        assert_eq!(b, "Person: new question\nAna: an answer");
        let all = brief(&roster(), &msgs, &me, None);
        assert!(all.starts_with("Person: old"));
    }

    #[test]
    fn a_brief_is_cut_to_20_messages_and_6000_characters() {
        let me = Member::new("p", "scout");
        let many: Vec<GroupMessage> = (0..30)
            .map(|i| msg(&format!("{i:02}"), None, &format!("m{i}")))
            .collect();
        let b = brief(&roster(), &many, &me, None);
        assert_eq!(b.lines().count(), 20);
        assert!(b.starts_with("Person: m10") && b.ends_with("Person: m29"));
        let big: Vec<GroupMessage> = (0..5)
            .map(|i| msg(&format!("{i}"), None, &"y".repeat(2500)))
            .collect();
        let b = brief(&roster(), &big, &me, None);
        assert!(b.chars().count() <= BRIEF_CHARS);
        assert_eq!(b.lines().count(), 2);
    }

    #[test]
    fn the_context_lists_the_roster_and_the_rule() {
        let c = context(&roster(), &Member::new("p", "scout"));
        assert!(c.contains("- Scout (you): finds things"));
        assert!(c.contains("- Ana\n"));
        assert!(c.contains("Reply to the group only"));
        assert!(c.contains("write @Name"));
    }

    #[test]
    fn a_reply_hands_on_to_others_never_to_its_author() {
        let me = Member::new("p", "scout");
        let got = hand_on("@Scout ok, @Ana please", &roster(), &me);
        assert_eq!(got, vec![Member::new("q", "ana")]);
    }

    #[test]
    fn ids_sort_by_time() {
        let a = sortable_id("m-", 99);
        let b = sortable_id("m-", 100);
        let c = sortable_id("m-", 100);
        assert!(a < b && b < c);
    }
}
