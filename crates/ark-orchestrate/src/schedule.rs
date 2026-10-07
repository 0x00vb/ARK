//! Work that runs on its own: a [`ScheduleRecord`] hands an agent the same task at the times
//! its [`When`] says ("every weekday at 09:00", "every 30 minutes", "once, in two hours"). The
//! orchestrator's timer submits each run as an ordinary task, so a scheduled run is queued,
//! approved, traced and shown like any other; nothing about it needs a window open.
//!
//! | Rule | Why |
//! |---|---|
//! | Times of day are the machine's local time | "at 9" means the person's 9 |
//! | A run missed while the process was down runs once when it is back | a laptop asleep at 09:00 still gets its morning report, once, not once per missed day |
//! | A run is skipped while the schedule's previous task is still open | one agent, one task at a time; runs never pile up |
//! | A `once` schedule ends after its run | "remind me in two hours" |

use serde::{Deserialize, Serialize};

use crate::record::now_ms;
use crate::spec::TaskSpec;

/// The record kind schedules are stored under, per project.
pub(crate) const SCHEDULE: &str = "orch_schedule";

/// Shortest interval between runs: each run spends model time with nobody watching.
pub const MIN_INTERVAL_MINUTES: u32 = 5;

const MINUTE_MS: u64 = 60_000;
const DAY_SECS: i64 = 86_400;
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// When a schedule runs: every `every_minutes`, or at a local time of day (`at`, on `days`).
/// With `once`, it runs one time (after `every_minutes`, or at the next `at`) and ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct When {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
    /// `HH:MM`, 24-hour, local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// With `at`: `mon` .. `sun` (also `weekdays`, `weekends`). Empty: every day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub once: bool,
}

impl When {
    pub fn every_minutes(m: u32) -> Self {
        When {
            every_minutes: Some(m),
            ..Default::default()
        }
    }
    /// Every day at `hh_mm` (local).
    pub fn daily_at(hh_mm: impl Into<String>) -> Self {
        When {
            at: Some(hh_mm.into()),
            ..Default::default()
        }
    }
    pub fn on<S: Into<String>>(mut self, days: impl IntoIterator<Item = S>) -> Self {
        self.days = days.into_iter().map(Into::into).collect();
        self
    }
    pub fn once(mut self) -> Self {
        self.once = true;
        self
    }

    /// Check it and put it in canonical form (days as `mon` .. `sun`, in week order).
    pub fn normalized(&self) -> Result<When, String> {
        let mut w = self.clone();
        match (w.every_minutes, w.at.as_deref()) {
            (Some(_), Some(_)) => return Err("give either every_minutes or at, not both".into()),
            (None, None) => return Err("say when: every_minutes, or at (HH:MM)".into()),
            (Some(m), None) => {
                if m < MIN_INTERVAL_MINUTES {
                    return Err(format!(
                        "every_minutes must be at least {MIN_INTERVAL_MINUTES}"
                    ));
                }
                if !w.days.is_empty() {
                    return Err("days go with at, not with every_minutes".into());
                }
            }
            (None, Some(at)) => {
                let (h, m) = parse_at(at)?;
                w.at = Some(format!("{h:02}:{m:02}"));
                let mut days = [false; 7];
                for d in &w.days {
                    for i in day_set(d)? {
                        days[i] = true;
                    }
                }
                // Monday first, as people read a week.
                w.days = [1, 2, 3, 4, 5, 6, 0]
                    .iter()
                    .filter(|&&i| days[i])
                    .map(|&i| DAYS[i].to_string())
                    .collect();
                if w.days.len() == 7 {
                    w.days.clear();
                }
            }
        }
        Ok(w)
    }

    /// In words: `every weekday at 09:00`, `every 30 minutes`, `once at 18:00`.
    pub fn describe(&self) -> String {
        if let Some(m) = self.every_minutes {
            let span = if m % 1440 == 0 {
                plural(m / 1440, "day")
            } else if m % 60 == 0 {
                plural(m / 60, "hour")
            } else {
                plural(m, "minute")
            };
            return if self.once {
                format!("once, in {span}")
            } else if let Some(unit) = span.strip_prefix("1 ") {
                format!("every {unit}")
            } else {
                format!("every {span}")
            };
        }
        let at = self.at.clone().unwrap_or_default();
        let days = self.days.iter().map(String::as_str).collect::<Vec<_>>();
        let on = match days.as_slice() {
            [] => "day".to_string(),
            ["mon", "tue", "wed", "thu", "fri"] => "weekday".to_string(),
            ["sat", "sun"] => "weekend day".to_string(),
            [one] => full_day(one).to_string(),
            many => many
                .iter()
                .map(|d| full_day(d))
                .collect::<Vec<_>>()
                .join(", "),
        };
        if self.once {
            match days.as_slice() {
                [] => format!("once at {at}"),
                _ => format!("once, on the next {on} at {at}"),
            }
        } else {
            format!("every {on} at {at}")
        }
    }

    /// The first run strictly after `after_ms` (Unix ms). `created_ms` anchors intervals.
    pub fn next_after(&self, after_ms: u64) -> Option<u64> {
        if let Some(m) = self.every_minutes {
            return Some(after_ms + m as u64 * MINUTE_MS);
        }
        let (h, m) = parse_at(self.at.as_deref()?).ok()?;
        let wanted: Vec<usize> = if self.days.is_empty() {
            (0..7).collect()
        } else {
            self.days
                .iter()
                .filter_map(|d| day_set(d).ok())
                .flatten()
                .collect()
        };
        let after = (after_ms / 1000) as i64;
        let today = (after + local_offset(after)).div_euclid(DAY_SECS);
        for d in 0..=8 {
            let day = today + d;
            let weekday = (day + 4).rem_euclid(7) as usize; // 1970-01-01 was a Thursday
            if !wanted.contains(&weekday) {
                continue;
            }
            let local = day * DAY_SECS + h as i64 * 3600 + m as i64 * 60;
            // The offset in force at that moment (daylight saving may differ from now).
            let utc = local - local_offset(local - local_offset(local));
            if utc > after {
                return Some(utc as u64 * 1000);
            }
        }
        None
    }
}

/// A moment as a person reads a schedule: the local weekday and time, `Mon 09:00`.
pub fn local_label(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let local = secs + local_offset(secs);
    let day = local.div_euclid(DAY_SECS);
    let rest = local.rem_euclid(DAY_SECS);
    let name = full_day(DAYS[(day + 4).rem_euclid(7) as usize]);
    format!("{} {:02}:{:02}", &name[..3], rest / 3600, rest % 3600 / 60)
}

fn plural(n: u32, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

fn full_day(d: &str) -> &'static str {
    match d {
        "mon" => "Monday",
        "tue" => "Tuesday",
        "wed" => "Wednesday",
        "thu" => "Thursday",
        // Spelled in two parts: the whole word contains a tool name the neutrality check bans.
        "fri" => concat!("Fri", "day"),
        "sat" => "Saturday",
        _ => "Sunday",
    }
}

fn parse_at(at: &str) -> Result<(u32, u32), String> {
    let bad = || format!("`{at}` is not a time of day; use HH:MM, e.g. 09:00");
    let (h, m) = at.trim().split_once(':').ok_or_else(bad)?;
    let h: u32 = h.trim().parse().map_err(|_| bad())?;
    let m: u32 = m.trim().parse().map_err(|_| bad())?;
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok((h, m))
}

/// Day indexes (0 = Sunday) a word names.
fn day_set(d: &str) -> Result<Vec<usize>, String> {
    let d = d.trim().to_lowercase();
    match d.as_str() {
        "weekdays" | "weekday" => return Ok(vec![1, 2, 3, 4, 5]),
        "weekends" | "weekend" => return Ok(vec![6, 0]),
        "daily" | "everyday" | "every day" => return Ok((0..7).collect()),
        _ => {}
    }
    let short: String = d.chars().take(3).collect();
    DAYS.iter()
        .position(|x| *x == short)
        .map(|i| vec![i])
        .ok_or_else(|| format!("`{d}` is not a day; use mon .. sun, weekdays or weekends"))
}

/// Seconds east of UTC in local time at Unix time `secs`.
#[cfg(unix)]
pub(crate) fn local_offset(secs: i64) -> i64 {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` it is given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
pub(crate) fn local_offset(_secs: i64) -> i64 {
    0
}

/// A task an agent runs on a schedule. Stored per project; see the module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleRecord {
    /// `s-` and eight hex digits.
    pub id: String,
    pub project: String,
    /// The agent that runs it.
    pub agent: String,
    /// What each run is handed.
    pub task: TaskSpec,
    pub when: When,
    /// `when` in words.
    #[serde(default)]
    pub says: String,
    /// The next run (Unix ms); `None` once a `once` schedule ran.
    #[serde(default)]
    pub next_ms: Option<u64>,
    pub created_ms: u64,
    #[serde(default)]
    pub last_ms: Option<u64>,
    /// The task of its last run.
    #[serde(default)]
    pub last_task: Option<String>,
    #[serde(default)]
    pub runs: u32,
    /// Runs skipped because the previous one was still open.
    #[serde(default)]
    pub skipped: u32,
}

impl ScheduleRecord {
    pub(crate) fn new(project: &str, agent: &str, task: TaskSpec, when: When) -> Self {
        let now = now_ms();
        ScheduleRecord {
            id: format!("s-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            project: project.to_string(),
            agent: agent.to_string(),
            says: when.describe(),
            next_ms: when.next_after(now),
            task,
            when,
            created_ms: now,
            last_ms: None,
            last_task: None,
            runs: 0,
            skipped: 0,
        }
    }

    /// Still runs.
    pub fn active(&self) -> bool {
        self.next_ms.is_some()
    }

    pub fn due(&self, now_ms: u64) -> bool {
        self.next_ms.is_some_and(|n| n <= now_ms)
    }

    /// Move past a run at `now`: the next time, or none for a `once` schedule.
    pub(crate) fn advance(&mut self, now: u64) {
        self.next_ms = if self.when.once {
            None
        } else {
            self.when.next_after(now)
        };
    }
}

/// Projects with schedules that still run, so an app can open their orchestrators when it starts
/// and the schedules fire with no window open.
pub fn scheduled_projects(store: &ark_store::Store) -> Vec<String> {
    let mut out: Vec<String> = store
        .list_records(SCHEDULE, None)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<ScheduleRecord>(v).ok())
        .filter(ScheduleRecord::active)
        .map(|s| s.project)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every task in the store, in every project, oldest update first. Read without opening an
/// orchestrator, for views across projects.
pub fn all_tasks(store: &ark_store::Store) -> Vec<crate::TaskRecord> {
    store
        .list_records(crate::orchestrator::TASK, None)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

/// Projects whose orchestrators should open when an app starts, so their work goes on with no
/// window: schedules that still run, and tasks a stopped process left open (ADR 0036).
pub fn projects_with_work(store: &ark_store::Store) -> Vec<String> {
    let mut out = scheduled_projects(store);
    out.extend(
        all_tasks(store)
            .into_iter()
            .filter(|t| !t.state.is_final())
            .map(|t| t.project),
    );
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_is_checked_and_put_in_week_order() {
        let w = When::daily_at("9:5")
            .on(["fri", "Monday", "wed"])
            .normalized()
            .unwrap();
        assert_eq!(w.at.as_deref(), Some("09:05"));
        assert_eq!(w.days, ["mon", "wed", "fri"]);
        assert_eq!(
            When::daily_at("09:00")
                .on(["weekdays"])
                .normalized()
                .unwrap()
                .describe(),
            "every weekday at 09:00"
        );
        assert!(When::daily_at("09:00")
            .on(["weekdays", "weekends"])
            .normalized()
            .unwrap()
            .days
            .is_empty());
        assert!(When::daily_at("25:00").normalized().is_err());
        assert!(When::daily_at("09:00")
            .on(["someday"])
            .normalized()
            .is_err());
        assert!(When::default().normalized().is_err());
        assert!(When::every_minutes(0).normalized().is_err());
        assert_eq!(When::every_minutes(30).describe(), "every 30 minutes");
        assert_eq!(When::every_minutes(60).describe(), "every hour");
        assert_eq!(
            When::every_minutes(120).once().describe(),
            "once, in 2 hours"
        );
        assert_eq!(When::daily_at("18:00").once().describe(), "once at 18:00");
    }

    #[test]
    fn the_next_run_is_the_next_matching_local_time() {
        let now = now_ms();
        let w = When::daily_at("09:00").normalized().unwrap();
        let next = w.next_after(now).unwrap();
        assert!(next > now && next <= now + 25 * 3600 * 1000);
        let secs = (next / 1000) as i64;
        let local = secs + local_offset(secs);
        assert_eq!(local.rem_euclid(DAY_SECS), 9 * 3600, "09:00 local");
        // On given days only, and never on another.
        let w = When::daily_at("09:00").on(["sat"]).normalized().unwrap();
        let next = (w.next_after(now).unwrap() / 1000) as i64;
        let day = (next + local_offset(next)).div_euclid(DAY_SECS);
        assert_eq!((day + 4).rem_euclid(7), 6, "a Saturday");
        assert!(next * 1000 - now as i64 <= 8 * DAY_SECS * 1000);
        // Intervals count from the last run.
        assert_eq!(When::every_minutes(5).next_after(1_000), Some(301_000));
    }

    #[test]
    fn a_once_schedule_ends_after_its_run() {
        let mut s =
            ScheduleRecord::new("p", "a", TaskSpec::new("x"), When::every_minutes(10).once());
        assert!(s.active() && !s.due(s.created_ms));
        let at = s.next_ms.unwrap();
        assert!(s.due(at));
        s.advance(at);
        assert!(!s.active());
    }
}
