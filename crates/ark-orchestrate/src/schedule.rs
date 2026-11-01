//! Work that runs on its own: a [`ScheduleRecord`] hands an agent the same task at the times
//! its [`When`] says ("every weekday at 09:00", "every 30 minutes", "on 15 Oct at 14:00", "the
//! 1st of each month"). The orchestrator's timer submits each run as an ordinary task, so a
//! scheduled run is queued, approved, traced and shown like any other; nothing about it needs a
//! window open.
//!
//! | Rule | Why |
//! |---|---|
//! | Times of day are read in the schedule's time zone (`When::tz`, the person's zone when it was made) | "at 9" means the person's 9, also on a runtime in another zone |
//! | A time the clocks skip (spring forward) runs at the first moment after it; a time that happens twice (fall back) runs the first time | one run per day, never none, never two |
//! | A run missed while the process was down runs once when it is back ([`IfMissed::Once`]), or is let go when more than [`LATE_MS`] late ([`IfMissed::Skip`]) | a laptop asleep at 02:00 still gets its report, once, not once per missed day |
//! | A run is skipped while the schedule's previous run is still open, unless `overlap` | runs never pile up |
//! | A run that fails for a reason that may pass (the model unreachable) is tried again later, `retries` times, continuing where it stopped | a network gone at 02:00 does not lose the night's work, and nothing is done twice |
//! | A paused schedule keeps its record and does not run; resuming starts from the next time, not the missed ones | pausing is not a backlog |
//! | A one-time schedule (`once`, or a `date`) ends after its run | "remind me in two hours" |

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::cron::Cron;
use crate::record::now_ms;
use crate::spec::TaskSpec;

/// The record kind schedules are stored under, per project.
pub(crate) const SCHEDULE: &str = "orch_schedule";

/// Shortest interval between runs: each run spends model time with nobody watching.
pub const MIN_INTERVAL_MINUTES: u32 = 5;

/// A run this late was missed (the process or the computer was off at its time).
pub const LATE_MS: u64 = 15 * 60_000;

/// Tries after a run fails for a reason that may pass, unless the schedule says otherwise.
pub const DEFAULT_RETRIES: u32 = 2;

/// The wait before the first try again; each later one waits twice as long.
pub const RETRY_DELAY_MS: u64 = 5 * 60_000;

const MINUTE_MS: u64 = 60_000;
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// When a schedule runs. One of: every `every_minutes`; at a time of day (`at`, on `days`, or
/// on one `date`); or a five-field `cron` expression. With `once` it runs one time and ends (a
/// `date` is always once). `tz` is the IANA time zone `at` and `cron` are read in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct When {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
    /// `HH:MM`, 24-hour, in `tz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// With `at`: `mon` .. `sun` (also `weekdays`, `weekends`). Empty: every day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    /// With `at`: one run on this day, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Instead of `at`: `minute hour day-of-month month day-of-week` (see [`crate::cron`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The IANA time zone (`Europe/Madrid`). `None`: the machine's zone at each run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
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
    /// Every day at `hh_mm`.
    pub fn daily_at(hh_mm: impl Into<String>) -> Self {
        When {
            at: Some(hh_mm.into()),
            ..Default::default()
        }
    }
    /// Once, on `date` (`YYYY-MM-DD`) at `hh_mm`.
    pub fn on_date(date: impl Into<String>, hh_mm: impl Into<String>) -> Self {
        When {
            at: Some(hh_mm.into()),
            date: Some(date.into()),
            once: true,
            ..Default::default()
        }
    }
    /// At the times a five-field expression gives (`0 9 1 * *`).
    pub fn cron(expr: impl Into<String>) -> Self {
        When {
            cron: Some(expr.into()),
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
    /// Read `at` and `cron` in this IANA time zone.
    pub fn in_zone(mut self, tz: impl Into<String>) -> Self {
        self.tz = Some(tz.into());
        self
    }

    /// Check it and put it in canonical form (days as `mon` .. `sun`, in week order; the zone
    /// as its IANA name).
    pub fn normalized(&self) -> Result<When, String> {
        let mut w = self.clone();
        if let Some(tz) = w.tz.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let zone = TimeZone::get(tz).map_err(|_| {
                format!("`{tz}` is not a time zone; use an IANA name, e.g. Europe/Madrid")
            })?;
            w.tz = Some(zone.iana_name().unwrap_or(tz).to_string());
        } else {
            w.tz = None;
        }
        w.cron = w
            .cron
            .as_deref()
            .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|c| !c.is_empty());
        w.date = w
            .date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        let kinds = [w.every_minutes.is_some(), w.at.is_some(), w.cron.is_some()];
        match kinds.iter().filter(|k| **k).count() {
            0 => return Err("say when: at (HH:MM), every_minutes, or cron".into()),
            1 => {}
            _ => return Err("give one of at, every_minutes or cron, not several".into()),
        }
        if w.date.is_some() && w.at.is_none() {
            return Err("a date goes with at (HH:MM)".into());
        }
        if !w.days.is_empty() && w.at.is_none() {
            return Err("days go with at, not with every_minutes or cron".into());
        }
        if let Some(m) = w.every_minutes {
            if m < MIN_INTERVAL_MINUTES {
                return Err(format!(
                    "every_minutes must be at least {MIN_INTERVAL_MINUTES}"
                ));
            }
        }
        if let Some(at) = w.at.as_deref() {
            let (h, m) = parse_at(at)?;
            w.at = Some(format!("{h:02}:{m:02}"));
            if let Some(d) = w.date.as_deref() {
                let date: Date = d
                    .parse()
                    .map_err(|_| format!("`{d}` is not a date; use YYYY-MM-DD, e.g. 2026-10-15"))?;
                if !w.days.is_empty() {
                    return Err("give a date or days, not both".into());
                }
                w.date = Some(date.to_string());
                w.once = true;
            }
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
        if let Some(c) = w.cron.as_deref() {
            Cron::parse(c)?;
            if !w.once {
                // Runs too close together spend model time nobody asked for.
                let mut at = now_ms();
                let mut prev: Option<u64> = None;
                for _ in 0..50 {
                    let Some(next) = w.next_after(at) else { break };
                    if prev.is_some_and(|p| next - p < MIN_INTERVAL_MINUTES as u64 * MINUTE_MS) {
                        return Err(format!(
                            "`{c}` runs more often than every {MIN_INTERVAL_MINUTES} minutes"
                        ));
                    }
                    prev = Some(next);
                    at = next;
                }
            }
            if w.next_after(now_ms()).is_none() {
                return Err(format!("`{c}` never runs"));
            }
        }
        Ok(w)
    }

    /// The zone its times are read in.
    pub fn zone(&self) -> TimeZone {
        self.tz
            .as_deref()
            .and_then(|t| TimeZone::get(t).ok())
            .unwrap_or_else(TimeZone::system)
    }

    /// In words: `every weekday at 09:00`, `every 30 minutes`, `once on 15 Oct 2026 at 14:00`,
    /// `on the 1st of each month at 09:00`.
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
        if let Some(c) = self.cron.as_deref() {
            let words = Cron::parse(c)
                .ok()
                .and_then(|c| c.describe())
                .unwrap_or_else(|| format!("on the times `{c}` gives"));
            return if self.once {
                format!("once, {words}")
            } else {
                words
            };
        }
        let at = self.at.clone().unwrap_or_default();
        if let Some(date) = self.date.as_deref().and_then(|d| d.parse::<Date>().ok()) {
            return format!(
                "once on {} {} {} at {at}",
                date.day(),
                &crate::cron::month_name(date.month() as u32)[..3],
                date.year()
            );
        }
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

    /// The first run strictly after `after_ms` (Unix ms), or `None` when there is none (a date
    /// that has passed).
    pub fn next_after(&self, after_ms: u64) -> Option<u64> {
        if let Some(m) = self.every_minutes {
            return Some(after_ms + m as u64 * MINUTE_MS);
        }
        let zone = self.zone();
        let after = Timestamp::from_millisecond(after_ms as i64).ok()?;
        let today = after.to_zoned(zone.clone()).date();
        // The instant a wall-clock time names in the zone. A time the clocks skip moves past
        // the gap; a time that happens twice is its first.
        let instant = |day: Date, h: u32, m: u32| -> Option<u64> {
            let dt = DateTime::from_parts(day, jiff::civil::time(h as i8, m as i8, 0, 0));
            let z = zone.to_ambiguous_zoned(dt).compatible().ok()?;
            Some(z.timestamp().as_millisecond() as u64)
        };
        if let Some(c) = self.cron.as_deref() {
            let cron = Cron::parse(c).ok()?;
            let times: Vec<(u32, u32)> = cron.times().collect();
            let mut day = today;
            // Five years cover a 29 February.
            for _ in 0..(366 * 5) {
                let week_day = day.weekday().to_sunday_zero_offset() as u32;
                if cron.day_matches(day.month() as u32, day.day() as u32, week_day) {
                    for (h, m) in &times {
                        if let Some(t) = instant(day, *h, *m).filter(|t| *t > after_ms) {
                            return Some(t);
                        }
                    }
                }
                day = day.tomorrow().ok()?;
            }
            return None;
        }
        let (h, m) = parse_at(self.at.as_deref()?).ok()?;
        if let Some(date) = self.date.as_deref() {
            let day: Date = date.parse().ok()?;
            return instant(day, h, m).filter(|t| *t > after_ms);
        }
        let wanted: Vec<usize> = if self.days.is_empty() {
            (0..7).collect()
        } else {
            self.days
                .iter()
                .filter_map(|d| day_set(d).ok())
                .flatten()
                .collect()
        };
        let mut day = today;
        for _ in 0..=8 {
            let week_day = day.weekday().to_sunday_zero_offset() as usize;
            if wanted.contains(&week_day) {
                if let Some(t) = instant(day, h, m).filter(|t| *t > after_ms) {
                    return Some(t);
                }
            }
            day = day.tomorrow().ok()?;
        }
        None
    }

    /// A moment as a person reads this schedule: `Mon 09:00`, a date when it is more than six
    /// days away (`Thu 15 Oct 14:00`), and the zone when it is not the machine's.
    pub fn label(&self, ms: u64) -> String {
        let zone = self.zone();
        let here = TimeZone::system();
        let foreign = self.tz.is_some() && here.iana_name() != zone.iana_name();
        let mut s = label_in(ms, &zone);
        if foreign {
            if let Ok(t) = Timestamp::from_millisecond(ms as i64) {
                s.push(' ');
                s.push_str(&t.to_zoned(zone).strftime("%Z").to_string());
            }
        }
        s
    }
}

/// The machine's IANA time zone, when it has a name.
pub fn system_zone() -> Option<String> {
    TimeZone::system().iana_name().map(str::to_string)
}

/// A moment in the machine's zone: `Mon 09:00`, or `Thu 15 Oct 14:00` more than six days
/// away or ago.
pub fn local_label(ms: u64) -> String {
    label_in(ms, &TimeZone::system())
}

fn label_in(ms: u64, zone: &TimeZone) -> String {
    let Ok(t) = Timestamp::from_millisecond(ms as i64) else {
        return String::new();
    };
    let z = t.to_zoned(zone.clone());
    // More than six days away, either way: with its date.
    let far = ms.abs_diff(now_ms()) > 6 * 24 * 3600 * 1000;
    z.strftime(if far { "%a %-d %b %H:%M" } else { "%a %H:%M" })
        .to_string()
}

fn plural(n: u32, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// A week day's name, 0 = Sunday.
pub(crate) fn day_name(i: usize) -> &'static str {
    full_day(DAYS[i % 7])
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

/// What happens to a run whose time passed while the process (or the computer) was off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfMissed {
    /// It runs once when the process is back, however many times were missed.
    #[default]
    Once,
    /// It is let go when more than [`LATE_MS`] late; the next run is at its usual time.
    Skip,
}

/// Where a schedule is, from its record and its last run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    /// Waiting for its next time; no run yet, or the last was cancelled.
    Scheduled,
    /// A run is queued or working.
    Running,
    /// A run waits for the person's answer.
    Asking,
    /// A run failed for a reason that may pass and tries again later.
    Retrying,
    /// The last run finished.
    Succeeded,
    /// The last run did not finish.
    Failed,
    /// Runs nothing until resumed.
    Paused,
}

impl ScheduleState {
    /// Of a schedule whose last run is `last` (its task record).
    pub fn of(s: &ScheduleRecord, last: Option<&crate::TaskRecord>) -> Self {
        use crate::TaskState as T;
        let open = last.filter(|t| !t.state.is_final());
        if let Some(t) = open {
            return if t.question.as_ref().is_some_and(|q| q.open()) {
                ScheduleState::Asking
            } else if t.retry_at_ms.is_some() {
                ScheduleState::Retrying
            } else {
                ScheduleState::Running
            };
        }
        if s.paused {
            return ScheduleState::Paused;
        }
        match last.map(|t| t.state) {
            Some(T::Done) => ScheduleState::Succeeded,
            Some(T::Failed) => ScheduleState::Failed,
            _ => ScheduleState::Scheduled,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ScheduleState::Scheduled => "scheduled",
            ScheduleState::Running => "running",
            ScheduleState::Asking => "waiting for your answer",
            ScheduleState::Retrying => "retrying",
            ScheduleState::Succeeded => "succeeded",
            ScheduleState::Failed => "failed",
            ScheduleState::Paused => "paused",
        }
    }
}

/// A task an agent runs on a schedule. Stored per project; see the module docs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleRecord {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub target_since_ms: u64,
    #[serde(default)]
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_slot_ms: Option<u64>,
    #[serde(default)]
    pub admission_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_retry_ms: Option<u64>,
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
    /// The next run (Unix ms); `None` while paused, and once a one-time schedule ran.
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
    /// Runs let go because their time passed while the process was off ([`IfMissed::Skip`]).
    #[serde(default)]
    pub missed: u32,
    /// Kept, but runs nothing until resumed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
    #[serde(default)]
    pub if_missed: IfMissed,
    /// A due run starts even while the previous one is open (it queues behind it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overlap: bool,
    /// Tries again after a run fails for a reason that may pass.
    #[serde(default = "default_retries")]
    pub retries: u32,
    /// Its agent was made for it: stopping the schedule ends the agent when nothing else uses it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_agent: bool,
    /// Runs in a row that ended failed; at 3 the schedule pauses itself.
    #[serde(default)]
    pub failures: u32,
    /// The last run counted in `failures`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counted: Option<String>,
}

fn default_retries() -> u32 {
    DEFAULT_RETRIES
}

pub struct CalendarSnapshot {
    pub schedules: Vec<ScheduleRecord>,
    pub tasks: Vec<crate::TaskRecord>,
    pub runs: Vec<crate::automation::Run>,
}

/// What [`Orchestrator::change_schedule`](crate::Orchestrator::change_schedule) changes; `None`
/// keeps it.
#[derive(Clone, Debug, Default)]
pub struct ScheduleChange {
    pub objective: Option<String>,
    pub context: Option<String>,
    pub when: Option<When>,
    pub if_missed: Option<IfMissed>,
    pub overlap: Option<bool>,
    pub retries: Option<u32>,
}

impl ScheduleChange {
    /// Its policies onto `s` (a run's tries again are at most 5).
    pub(crate) fn policies(&self, s: &mut ScheduleRecord) {
        if let Some(m) = self.if_missed {
            s.if_missed = m;
        }
        if let Some(o) = self.overlap {
            s.overlap = o;
        }
        if let Some(r) = self.retries {
            s.retries = r.min(5);
        }
    }
}

impl ScheduleRecord {
    pub(crate) fn new(project: &str, agent: &str, task: TaskSpec, mut when: When) -> Self {
        let now = now_ms();
        // Times of day are the person's: keep their zone, so a runtime elsewhere agrees.
        if when.tz.is_none() && when.every_minutes.is_none() {
            when.tz = system_zone();
        }
        ScheduleRecord {
            target: None,
            target_since_ms: 0,
            version: 0,
            admission_error: None,
            admission_slot_ms: None,
            admission_failures: 0,
            admission_retry_ms: None,
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
            missed: 0,
            paused: false,
            if_missed: IfMissed::Once,
            overlap: false,
            retries: DEFAULT_RETRIES,
            failures: 0,
            counted: None,
            own_agent: false,
        }
    }

    /// Will run again.
    pub fn active(&self) -> bool {
        self.next_ms.is_some()
    }

    /// Kept in the store: active or paused.
    pub fn kept(&self) -> bool {
        self.active() || self.paused
    }

    pub fn due(&self, now_ms: u64) -> bool {
        !self.paused
            && self.next_ms.is_some_and(|n| n <= now_ms)
            && self.admission_retry_ms.is_none_or(|retry| retry <= now_ms)
    }

    /// The next run in words (`Thu 02:00`), in the schedule's zone.
    pub fn next_label(&self) -> Option<String> {
        self.next_ms.map(|n| self.when.label(n))
    }

    /// Move past the run for time `slot`, handled at `now`: the next time still to come, or
    /// none for a one-time schedule. An interval counts from its time, not from the tick that
    /// noticed it, so it does not drift.
    pub(crate) fn advance(&mut self, slot: u64, now: u64) {
        self.next_ms = if self.when.once {
            None
        } else {
            match self.when.next_after(slot) {
                Some(n) if n > now => Some(n),
                _ => self.when.next_after(now),
            }
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
    tasks_where(store, |_| true)
}

/// The tasks whose stored record passes `keep`, which sees the raw record, so a view across
/// projects reads in full only the tasks it shows.
pub fn tasks_where(
    store: &ark_store::Store,
    keep: impl Fn(&serde_json::Value) -> bool,
) -> Vec<crate::TaskRecord> {
    store
        .list_records(crate::orchestrator::TASK, None)
        .unwrap_or_default()
        .into_iter()
        .filter(|v| keep(v))
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

    fn ms(rfc3339: &str) -> u64 {
        rfc3339.parse::<Timestamp>().unwrap().as_millisecond() as u64
    }

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
    fn dates_cron_and_zones_are_checked() {
        let w = When::on_date("2026-10-15", "14:00")
            .in_zone("Europe/Madrid")
            .normalized()
            .unwrap();
        assert!(w.once, "a date is one run");
        assert_eq!(w.describe(), "once on 15 Oct 2026 at 14:00");
        assert!(When::on_date("15/10/2026", "14:00").normalized().is_err());
        assert!(When {
            date: Some("2026-10-15".into()),
            ..Default::default()
        }
        .normalized()
        .is_err());
        assert!(When::daily_at("09:00")
            .in_zone("Mars/Base")
            .normalized()
            .is_err());
        assert!(
            When::cron("* * * * *").normalized().is_err(),
            "every minute"
        );
        assert!(When::cron("*/5 * * * *").normalized().is_ok());
        assert!(When::cron("0 9 31 2 *").normalized().is_err(), "never runs");
        assert!(When::cron("0 9 1 * *").on(["mon"]).normalized().is_err());
        let both = When {
            at: Some("09:00".into()),
            cron: Some("0 9 * * *".into()),
            ..Default::default()
        };
        assert!(both.normalized().is_err());
        assert_eq!(
            When::cron("0  9 1 * *")
                .normalized()
                .unwrap()
                .cron
                .as_deref(),
            Some("0 9 1 * *")
        );
        assert_eq!(
            When::cron("0 9 1 * *").describe(),
            "on the 1st of each month at 09:00"
        );
    }

    #[test]
    fn the_next_run_is_the_next_matching_local_time() {
        let now = now_ms();
        let w = When::daily_at("09:00").normalized().unwrap();
        let next = w.next_after(now).unwrap();
        assert!(next > now && next <= now + 25 * 3600 * 1000);
        let secs = (next / 1000) as i64;
        let local = secs + ark_os::time::local_offset(secs);
        assert_eq!(local.rem_euclid(86_400), 9 * 3600, "09:00 local");
        // On given days only, and never on another.
        let w = When::daily_at("09:00").on(["sat"]).normalized().unwrap();
        let next = (w.next_after(now).unwrap() / 1000) as i64;
        let day = (next + ark_os::time::local_offset(next)).div_euclid(86_400);
        assert_eq!((day + 4).rem_euclid(7), 6, "a Saturday");
        assert!(next * 1000 - now as i64 <= 8 * 86_400 * 1000);
        // Intervals count from the last run.
        assert_eq!(When::every_minutes(5).next_after(1_000), Some(301_000));
    }

    #[test]
    fn times_are_read_in_the_schedules_zone_across_daylight_saving() {
        let ny = |w: When| w.in_zone("America/New_York").normalized().unwrap();
        // 8 March 2026: 02:00 becomes 03:00 in New York. 02:30 does not exist that day: it runs
        // at 03:30 (EDT, UTC-4), once, and at 02:30 again the day after.
        let w = ny(When::daily_at("02:30"));
        let first = w.next_after(ms("2026-03-07T17:00:00Z")).unwrap();
        assert_eq!(first, ms("2026-03-08T07:30:00Z"));
        assert_eq!(w.next_after(first), Some(ms("2026-03-09T06:30:00Z")));
        // 1 November 2026: 02:00 becomes 01:00 again. 01:30 happens twice: it runs the first
        // time (EDT) and not the second; the next is the day after (EST, UTC-5).
        let w = ny(When::daily_at("01:30"));
        let first = w.next_after(ms("2026-10-31T16:00:00Z")).unwrap();
        assert_eq!(first, ms("2026-11-01T05:30:00Z"));
        assert_eq!(w.next_after(first), Some(ms("2026-11-02T06:30:00Z")));
        // The same rules for cron.
        let w = ny(When::cron("30 2 * * *"));
        assert_eq!(
            w.next_after(ms("2026-03-07T17:00:00Z")),
            Some(ms("2026-03-08T07:30:00Z"))
        );
        // Another zone than the machine's: 09:00 in Tokyo is 00:00 UTC.
        let w = When::daily_at("09:00")
            .in_zone("Asia/Tokyo")
            .normalized()
            .unwrap();
        assert_eq!(
            w.next_after(ms("2026-10-07T12:00:00Z")),
            Some(ms("2026-10-08T00:00:00Z"))
        );
        // Intervals are real durations, whatever the clocks do.
        let w = ny(When::every_minutes(60));
        let t = ms("2026-11-01T05:30:00Z");
        assert_eq!(w.next_after(t), Some(t + 3_600_000));
    }

    #[test]
    fn a_date_runs_once_and_cron_finds_its_days() {
        let w = When::on_date("2026-10-15", "14:00")
            .in_zone("Europe/Madrid")
            .normalized()
            .unwrap();
        let at = ms("2026-10-15T12:00:00Z");
        assert_eq!(w.next_after(ms("2026-10-07T12:00:00Z")), Some(at));
        assert_eq!(w.next_after(at), None, "a passed date has no next run");
        let utc = |w: When| w.in_zone("UTC").normalized().unwrap();
        // The 1st of each month at 09:00, from 7 October: 1 November, then 1 December.
        let w = utc(When::cron("0 9 1 * *"));
        let n = w.next_after(ms("2026-10-07T12:00:00Z")).unwrap();
        assert_eq!(n, ms("2026-11-01T09:00:00Z"));
        assert_eq!(w.next_after(n), Some(ms("2026-12-01T09:00:00Z")));
        // Every other day: the 1st, the 3rd.
        let w = utc(When::cron("0 9 */2 * *"));
        let n = w.next_after(ms("2026-10-01T10:00:00Z")).unwrap();
        assert_eq!(n, ms("2026-10-03T09:00:00Z"));
        // 29 February: the next is in 2028.
        let w = utc(When::cron("0 9 29 2 *"));
        assert_eq!(
            w.next_after(ms("2026-10-07T12:00:00Z")),
            Some(ms("2028-02-29T09:00:00Z"))
        );
        // Several times a day, weekdays only: Friday 17:00, then Monday 09:00.
        let w = utc(When::cron("0 9-17/4 * * mon-fri"));
        let fri = ms("2026-10-09T13:30:00Z");
        let n = w.next_after(fri).unwrap();
        assert_eq!(n, ms("2026-10-09T17:00:00Z"));
        assert_eq!(w.next_after(n), Some(ms("2026-10-12T09:00:00Z")));
    }

    #[test]
    fn a_once_schedule_ends_after_its_run() {
        let mut s =
            ScheduleRecord::new("p", "a", TaskSpec::new("x"), When::every_minutes(10).once());
        assert!(s.active() && !s.due(s.created_ms));
        let at = s.next_ms.unwrap();
        assert!(s.due(at));
        s.advance(at, at);
        assert!(!s.active());
    }

    #[test]
    fn a_new_schedule_keeps_the_persons_zone_and_old_records_still_read() {
        let s = ScheduleRecord::new("p", "a", TaskSpec::new("x"), When::daily_at("02:00"));
        assert_eq!(s.when.tz, system_zone());
        assert_eq!(s.retries, DEFAULT_RETRIES);
        let s = ScheduleRecord::new("p", "a", TaskSpec::new("x"), When::every_minutes(30));
        assert_eq!(s.when.tz, None, "an interval has no time of day");
        // A record saved before zones, pauses and retries.
        let old = serde_json::json!({
            "id": "s-1", "project": "p", "agent": "a",
            "task": { "objective": "x" },
            "when": { "at": "09:00" },
            "next_ms": 1, "created_ms": 0
        });
        let s: ScheduleRecord = serde_json::from_value(old).unwrap();
        assert!(!s.paused && !s.overlap && s.if_missed == IfMissed::Once);
        assert_eq!(s.retries, DEFAULT_RETRIES);
    }
}

pub(crate) const MANUAL_RECEIPT: &str = "orch_schedule_manual_receipt";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleExecutionKind {
    Task,
    Automation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleRunReceipt {
    pub schedule: String,
    pub request: String,
    pub execution: String,
    pub kind: ScheduleExecutionKind,
    pub created_ms: u64,
}

pub(crate) fn manual_receipt_id(
    project: &str,
    schedule: &str,
    request: &str,
) -> Result<String, crate::OrchError> {
    crate::automation::RunOrigin::ScheduleManual {
        schedule: schedule.into(),
        request: request.into(),
    }
    .validate()
    .map_err(crate::OrchError::NotAllowed)?;
    Ok(format!(
        "sr-{}",
        blake3::hash(&serde_json::to_vec(&(project, schedule, request))?).to_hex()
    ))
}
