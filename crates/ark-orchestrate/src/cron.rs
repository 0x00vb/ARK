//! Five-field calendar expressions (`minute hour day-of-month month day-of-week`), for schedules
//! that `at` and `days` cannot say: "the 1st of each month at 09:00" (`0 9 1 * *`), "every two
//! hours from 9 to 17 on weekdays" (`0 9-17/2 * * mon-fri`).
//!
//! | Field | Values | Also |
//! |---|---|---|
//! | minute | 0-59 | `*`, lists `1,15`, ranges `9-17`, steps `*/15`, `9-17/2` |
//! | hour | 0-23 | as above |
//! | day of month | 1-31 | as above |
//! | month | 1-12 | `jan` .. `dec` |
//! | day of week | 0-7 (0 and 7 are Sunday) | `sun` .. `sat` |
//!
//! When both day fields are restricted, a day matching either runs (the usual cron rule).

/// A parsed expression: one bit per allowed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cron {
    pub minutes: u64,
    pub hours: u32,
    /// Bit 1..=31.
    pub month_days: u32,
    /// Bit 1..=12.
    pub months: u16,
    /// Bit 0..=6, 0 is Sunday.
    pub week_days: u8,
    pub any_month_day: bool,
    pub any_week_day: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEK: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "`{expr}` needs 5 fields: minute hour day-of-month month day-of-week, e.g. `0 9 1 * *`"
            ));
        }
        let minutes = field(fields[0], 0, 59, &[])?;
        let hours = field(fields[1], 0, 23, &[])?;
        let month_days = field(fields[2], 1, 31, &[])?;
        let months = field(fields[3], 1, 12, &MONTHS)?;
        let mut week = field(fields[4], 0, 7, &WEEK)?;
        if week & (1 << 7) != 0 {
            week = (week & !(1 << 7)) | 1;
        }
        Ok(Cron {
            minutes,
            hours: hours as u32,
            month_days: month_days as u32,
            months: months as u16,
            week_days: week as u8,
            // A field that starts with `*` (also `*/2`) counts as every day for the either-day
            // rule, as cron reads it.
            any_month_day: fields[2].starts_with('*'),
            any_week_day: fields[4].starts_with('*'),
        })
    }

    /// Whether a day runs: its month, and its day of month or of week (`week_day` 0 = Sunday).
    pub fn day_matches(&self, month: u32, month_day: u32, week_day: u32) -> bool {
        if self.months & (1 << month) == 0 {
            return false;
        }
        let dom = self.month_days & (1 << month_day) != 0;
        let dow = self.week_days & (1 << week_day) != 0;
        // Cron's rule: a day field written with `*` (also `*/2`) narrows with AND; two
        // restricted day fields widen with OR. A plain `*` has every bit, so AND keeps the other.
        if self.any_month_day || self.any_week_day {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// The times of a day it runs at, earliest first, as (hour, minute).
    pub fn times(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (0..24u32)
            .filter(move |h| self.hours & (1 << h) != 0)
            .flat_map(move |h| {
                (0..60u32)
                    .filter(move |m| self.minutes & (1 << m) != 0)
                    .map(move |m| (h, m))
            })
    }

    /// In words, when the shape is a common one; `None` otherwise.
    pub fn describe(&self) -> Option<String> {
        let times: Vec<(u32, u32)> = self.times().collect();
        let at = match times.as_slice() {
            [] => return None,
            few if few.len() <= 4 => {
                let list: Vec<String> = few.iter().map(|(h, m)| format!("{h:02}:{m:02}")).collect();
                format!("at {}", join_and(&list))
            }
            _ => {
                let mins: Vec<u32> = (0..60).filter(|m| self.minutes & (1 << m) != 0).collect();
                let step = step_of(&mins, 60)?;
                if self.hours == (1 << 24) - 1 {
                    if mins == [0] {
                        "every hour".to_string()
                    } else if step > 1 && mins[0] == 0 {
                        format!("every {step} minutes")
                    } else {
                        return None;
                    }
                } else {
                    let hours: Vec<u32> = (0..24).filter(|h| self.hours & (1 << h) != 0).collect();
                    let (first, last) = (hours[0], *hours.last().unwrap());
                    let every = match (mins.as_slice(), step_of(&hours, 24)) {
                        ([m], Some(1)) => format!("every hour at :{m:02}"),
                        ([m], Some(n)) => format!("every {n} hours at :{m:02}"),
                        (_, Some(1)) if step > 1 && mins[0] == 0 => {
                            format!("every {step} minutes")
                        }
                        _ => return None,
                    };
                    format!(
                        "{every} from {first:02}:{:02} to {last:02}:{:02}",
                        mins[0],
                        mins.last()?
                    )
                }
            }
        };
        let mut on = Vec::new();
        if !self.any_month_day {
            let days: Vec<String> = (1..=31)
                .filter(|d| self.month_days & (1 << d) != 0)
                .map(ordinal)
                .collect();
            on.push(format!("on the {}", join_and(&days)));
        }
        if !self.any_week_day {
            let days: Vec<u32> = (0..7).filter(|d| self.week_days & (1 << d) != 0).collect();
            let words = match days.as_slice() {
                [1, 2, 3, 4, 5] => "weekdays".to_string(),
                [0, 6] => "weekends".to_string(),
                _ => join_and(
                    &days
                        .iter()
                        .map(|d| crate::schedule::day_name(*d as usize).to_string())
                        .collect::<Vec<_>>(),
                ),
            };
            on.push(format!("on {words}"));
        }
        let every_month = self.months == 0b1_1111_1111_1110;
        let months = if every_month {
            String::new()
        } else {
            let names: Vec<String> = (1..=12)
                .filter(|m| self.months & (1 << m) != 0)
                .map(|m| month_name(m).to_string())
                .collect();
            format!(" in {}", join_and(&names))
        };
        let tail = if !self.any_month_day && every_month {
            " of each month"
        } else {
            ""
        };
        let days = match on.len() {
            0 if at.starts_with("every") && months.is_empty() => return Some(at),
            0 => "every day".to_string(),
            _ => format!("{}{tail}", on.join(" or ")),
        };
        Some(format!("{days}{months} {at}"))
    }
}

/// The step between sorted values when they are evenly spaced and wrap around `span`.
fn step_of(v: &[u32], span: u32) -> Option<u32> {
    match v {
        [] => None,
        [_] => Some(span),
        _ => {
            let step = v[1] - v[0];
            v.windows(2).all(|w| w[1] - w[0] == step).then_some(step)
        }
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

pub(crate) fn month_name(m: u32) -> &'static str {
    [
        "",
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ][m as usize]
}

/// One field as a bit set over `lo..=hi`.
fn field(text: &str, lo: u32, hi: u32, names: &[&str]) -> Result<u64, String> {
    let bad = |why: &str| format!("`{text}`: {why}");
    let value = |s: &str| -> Result<u32, String> {
        let s = s.trim().to_lowercase();
        if let Some(i) = names.iter().position(|n| s.starts_with(n) && s.len() >= 3) {
            // Months count from 1, week days from 0.
            return Ok(i as u32 + u32::from(names == MONTHS));
        }
        let n: u32 = s.parse().map_err(|_| bad("not a number"))?;
        if n < lo || n > hi {
            return Err(bad(&format!("must be {lo}-{hi}")));
        }
        Ok(n)
    };
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let s: u32 = s.parse().map_err(|_| bad("the step is not a number"))?;
                if s == 0 {
                    return Err(bad("the step must be at least 1"));
                }
                (r, s)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            // `5/15` means from 5 to the end, every 15.
            (v, if step > 1 { hi } else { v })
        };
        // A week may wrap past Sunday (`fri-sun`, `sat-mon`); other ranges go low to high.
        let to = if from > to && names == WEEK {
            to + 7
        } else {
            to
        };
        if from > to {
            return Err(bad("a range must go from low to high"));
        }
        let mut v = from;
        while v <= to {
            bits |= 1 << (if names == WEEK && v > 7 { v - 7 } else { v });
            v += step;
        }
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_parse_with_lists_ranges_steps_and_names() {
        let c = Cron::parse("*/15 9-17/2 1,15 jan-mar mon-fri").unwrap();
        assert_eq!(c.minutes, 1 | 1 << 15 | 1 << 30 | 1 << 45);
        assert_eq!(c.hours, 1 << 9 | 1 << 11 | 1 << 13 | 1 << 15 | 1 << 17);
        assert_eq!(c.month_days, 1 << 1 | 1 << 15);
        assert_eq!(c.months, 1 << 1 | 1 << 2 | 1 << 3);
        assert_eq!(c.week_days, 0b0011_1110);
        assert_eq!(
            Cron::parse("0 0 * * 7").unwrap().week_days,
            1,
            "7 is Sunday"
        );
        assert_eq!(Cron::parse("0 0 * * sun").unwrap().week_days, 1);
        assert_eq!(Cron::parse("0 0 * dec *").unwrap().months, 1 << 12);
        // Sunday is 0: a week range may wrap through it.
        assert_eq!(
            Cron::parse("0 0 * * fri-sun").unwrap().week_days,
            1 | 1 << 5 | 1 << 6
        );
        assert_eq!(Cron::parse("0 0 * * mon-sun").unwrap().week_days, 0x7f);
        // `*/2` in a day field narrows with the other field (AND), not either (OR).
        let c = Cron::parse("0 9 */2 * mon").unwrap();
        assert!(c.any_month_day);
        assert!(c.day_matches(10, 5, 1), "an odd day, a Monday");
        assert!(!c.day_matches(10, 12, 1), "an even day");
        assert!(!c.day_matches(10, 7, 3), "not a Monday");
        let every_other = Cron::parse("0 9 */2 * *").unwrap();
        assert!(every_other.day_matches(10, 1, 4) && !every_other.day_matches(10, 2, 5));
        for bad in [
            "0 9 * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn either_day_field_matches_when_both_are_given() {
        // The 1st, or any Monday.
        let c = Cron::parse("0 9 1 * mon").unwrap();
        assert!(c.day_matches(10, 1, 4));
        assert!(c.day_matches(10, 5, 1));
        assert!(!c.day_matches(10, 6, 2));
        // Only the day of month restricted: the day of week is ignored.
        let c = Cron::parse("0 9 1 * *").unwrap();
        assert!(c.day_matches(10, 1, 4) && !c.day_matches(10, 2, 1));
    }

    #[test]
    fn common_shapes_read_as_words() {
        let d = |e: &str| Cron::parse(e).unwrap().describe().unwrap_or_default();
        assert_eq!(d("0 9 1 * *"), "on the 1st of each month at 09:00");
        assert_eq!(d("30 2 * * *"), "every day at 02:30");
        assert_eq!(d("0 9,18 * * mon-fri"), "on weekdays at 09:00 and 18:00");
        assert_eq!(
            d("0 9 1,15 * *"),
            "on the 1st and 15th of each month at 09:00"
        );
        assert_eq!(d("*/15 * * * *"), "every 15 minutes");
        assert_eq!(d("0 * * * *"), "every hour");
        assert_eq!(
            d("0 9-17/2 * * mon-fri"),
            "on weekdays every 2 hours at :00 from 09:00 to 17:00"
        );
        assert_eq!(
            d("0 8 * jan,jul *"),
            "every day in January and July at 08:00"
        );
        assert_eq!(
            d("30 8-18/2 * * *"),
            "every 2 hours at :30 from 08:30 to 18:30"
        );
    }
}
