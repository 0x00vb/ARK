//! Which agent a name means. A model or a person names an agent the way it reads in the window
//! ("Censu - Engineer", "Engineer censu", "Censu -Enginner"), not by its stored name
//! (`censu---engineer`). ADR 0130.

/// The agent `query` means among `agents` (name, title): the exact name; else the one whose name
/// or title matches ignoring case, spaces, punctuation and word order; else one typo in each
/// six letters (none under six); else the one whose words contain all of the query's. `Err` holds the names that
/// tie, or nothing when none matched.
pub(crate) fn pick<'a>(
    query: &str,
    agents: &[(&'a str, &'a str)],
) -> Result<&'a str, Vec<&'a str>> {
    let query = query.trim();
    if let Some((name, _)) = agents.iter().find(|(name, _)| *name == query) {
        return Ok(name);
    }
    if let Ok(name) = one(agents
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(query))
        .map(|(n, _)| *n))
    {
        return Ok(name);
    }
    let q = Key::of(query);
    if q.letters.is_empty() {
        return Err(Vec::new());
    }
    let keys: Vec<(&str, [Key; 2])> = agents
        .iter()
        .map(|(name, title)| (*name, [Key::of(name), Key::of(title)]))
        .collect();
    // A typo is in a letter: "analyst 3" is not "analyst 2".
    let distance = |k: &Key| match edits(&q.letters, &k.letters).min(edits(&q.sorted, &k.sorted)) {
        _ if k.letters.is_empty() => usize::MAX,
        0 => 0,
        d if k.digits() == q.digits() => d,
        _ => usize::MAX,
    };
    let allowed = q.letters.chars().count() / 6;
    let best = keys
        .iter()
        .map(|(name, k)| (*name, k.iter().map(distance).min().unwrap_or(usize::MAX)))
        .filter(|(_, d)| *d <= allowed)
        .collect::<Vec<_>>();
    if let Some(min) = best.iter().map(|(_, d)| *d).min() {
        return one(best.iter().filter(|(_, d)| *d == min).map(|(n, _)| *n));
    }
    one(keys
        .iter()
        .filter(|(_, k)| {
            k.iter()
                .any(|k| q.words.iter().all(|w| k.words.contains(w)))
        })
        .map(|(n, _)| *n))
}

/// `pick` over owned (name, title) pairs, with the reason in words when no agent or several
/// match: the model reads it and names one of them next.
pub(crate) fn resolve(query: &str, agents: &[(String, String)]) -> Result<String, String> {
    let pairs: Vec<(&str, &str)> = agents
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    match pick(query, &pairs) {
        Ok(name) => Ok(name.to_string()),
        Err(tied) if !tied.is_empty() => Err(format!(
            "`{}` could be {}; use one of these names",
            query.trim(),
            listed(&pairs, |n| tied.contains(&n))
        )),
        Err(_) if pairs.is_empty() => Err("there are no other agents you can reach".into()),
        Err(_) => Err(format!(
            "no agent you can reach is called `{}`. Agents: {}",
            query.trim(),
            listed(&pairs, |_| true)
        )),
    }
}

/// `name (Title)` for each agent `keep` takes, the title only when it says something else.
pub(crate) fn listed(agents: &[(&str, &str)], keep: impl Fn(&str) -> bool) -> String {
    let mut out: Vec<String> = agents
        .iter()
        .filter(|(n, _)| keep(n))
        .map(|(n, t)| match t.trim() {
            "" => n.to_string(),
            t if t.eq_ignore_ascii_case(n) => n.to_string(),
            t => format!("{n} ({t})"),
        })
        .collect();
    out.sort();
    out.join(", ")
}

fn one<'a>(names: impl Iterator<Item = &'a str>) -> Result<&'a str, Vec<&'a str>> {
    let mut names: Vec<&str> = names.collect();
    names.dedup();
    match names.as_slice() {
        [name] => Ok(name),
        _ => Err(names),
    }
}

struct Key {
    words: Vec<String>,
    letters: String,
    sorted: String,
}

impl Key {
    fn digits(&self) -> String {
        self.letters.chars().filter(char::is_ascii_digit).collect()
    }

    fn of(s: &str) -> Self {
        let words: Vec<String> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let mut sorted = words.clone();
        sorted.sort();
        Self {
            letters: words.concat(),
            sorted: sorted.concat(),
            words,
        }
    }
}

/// Levenshtein distance by characters.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::pick;

    const AGENTS: &[(&str, &str)] = &[
        ("censu---engineer", "Censu - Engineer"),
        ("censu-ceo", "Censu CEO"),
        ("engineer", ""),
        ("qa", ""),
        ("ceo-redagro", "CEO Redagro"),
        ("cmo-redagro", "CMO Redagro"),
    ];

    #[test]
    fn the_names_from_the_owners_chat_find_the_engineer() {
        for q in [
            "Engineer censu",
            "Censu-Engineer",
            "Censu -Enginner",
            "Censu - Engineer",
            "censu engineer",
        ] {
            assert_eq!(pick(q, AGENTS), Ok("censu---engineer"), "{q}");
        }
    }

    #[test]
    fn an_exact_name_wins_over_a_close_one() {
        assert_eq!(pick("engineer", AGENTS), Ok("engineer"));
        assert_eq!(pick("qa", AGENTS), Ok("qa"));
    }

    #[test]
    fn a_number_is_not_read_as_a_typo() {
        assert_eq!(pick("Analyst 3", &[("analyst2", "")]), Err(vec![]));
        assert_eq!(
            pick("agent-14", &[("agent-1", ""), ("agent-4", "")]),
            Err(vec![])
        );
        assert_eq!(pick("Analist 2", &[("analyst2", "")]), Ok("analyst2"));
    }

    #[test]
    fn a_name_in_other_case_is_the_exact_name() {
        assert_eq!(
            pick("Engineer", &[("engineer", ""), ("eng", "Engineer")]),
            Ok("engineer")
        );
    }

    #[test]
    fn a_short_name_is_not_read_as_a_typo_of_another() {
        assert_eq!(pick("coo", &[("ceo", "")]), Err(vec![]));
        assert_eq!(pick("qb", AGENTS), Err(vec![]));
    }

    #[test]
    fn a_part_of_the_name_that_only_one_agent_has_finds_it() {
        assert_eq!(pick("cmo", AGENTS), Ok("cmo-redagro"));
    }

    #[test]
    fn a_name_two_agents_share_is_not_guessed() {
        assert_eq!(
            pick("redagro", AGENTS),
            Err(vec!["ceo-redagro", "cmo-redagro"])
        );
        assert_eq!(pick("ceo", AGENTS), Err(vec!["censu-ceo", "ceo-redagro"]));
    }

    #[test]
    fn a_wrong_name_is_answered_with_the_names_to_use() {
        let owned: Vec<(String, String)> = AGENTS
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let e = super::resolve("designer", &owned).unwrap_err();
        assert!(e.contains("censu---engineer (Censu - Engineer)"), "{e}");
        let e = super::resolve("redagro", &owned).unwrap_err();
        assert_eq!(e, "`redagro` could be ceo-redagro (CEO Redagro), cmo-redagro (CMO Redagro); use one of these names");
        assert_eq!(
            super::resolve("Censu -Enginner", &owned),
            Ok("censu---engineer".into())
        );
    }

    #[test]
    fn a_name_nobody_has_matches_nothing() {
        assert_eq!(pick("designer", AGENTS), Err(vec![]));
        assert_eq!(pick("  ", AGENTS), Err(vec![]));
    }
}
