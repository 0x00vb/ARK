//! How much of a subscription's usage limits is spent, as its CLI reports it. Claude Code
//! sends a `rate_limit_event` with each reply; Codex answers `account/rateLimits/read` and
//! sends `account/rateLimits/updated`. Both say the same thing: a few windows (five hours, a
//! week), the share used in each, and when each resets.
//!
//! A CLI signs in as the person on this machine, so its usage belongs to the account, not to
//! one model or session: the latest report is kept per account for the whole process.
//! [`subscription_usage`] reads it. Nothing here is estimated: an account no CLI has reported
//! on yet has no entry.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The latest usage report of one subscription account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionUsage {
    /// The account (`claude`, `codex`).
    pub account: String,
    /// The plan, when the CLI says (`plus`, `pro`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The usage windows, shortest first.
    pub windows: Vec<UsageWindow>,
    /// A limit is reached: calls fail until the window resets.
    pub limited: bool,
    /// When the CLI reported it, in Unix milliseconds.
    pub reported_ms: u64,
}

/// One usage window of a subscription.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// How long the window is, in minutes (300 for five hours, 10080 for a week), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
    /// The share used, in whole percent (0 to 100).
    pub used_percent: u32,
    /// When it resets, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

static LATEST: Mutex<BTreeMap<String, SubscriptionUsage>> = Mutex::new(BTreeMap::new());

/// The latest usage report of every subscription account a CLI has reported on, by account.
pub fn subscription_usage() -> Vec<SubscriptionUsage> {
    LATEST.lock().unwrap().values().cloned().collect()
}

/// The latest usage report of `account`, if its CLI has sent one.
pub fn usage_of(account: &str) -> Option<SubscriptionUsage> {
    LATEST.lock().unwrap().get(account).cloned()
}

pub(crate) fn record(u: SubscriptionUsage) {
    LATEST.lock().unwrap().insert(u.account.clone(), u);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn percent(p: f64) -> u32 {
    p.round().clamp(0.0, 100.0) as u32
}

/// Claude Code's `rate_limit_info` (from a `rate_limit_event`).
pub(crate) fn from_claude(info: &Value) -> Option<SubscriptionUsage> {
    let mut windows: Vec<UsageWindow> = info["unifiedWindows"]
        .as_object()?
        .iter()
        .map(|(name, w)| UsageWindow {
            minutes: match name.as_str() {
                "five_hour" => Some(300),
                "seven_day" => Some(10080),
                _ => None,
            },
            // A fraction (0.35).
            used_percent: percent(w["utilization"].as_f64().unwrap_or(0.0) * 100.0),
            resets_at: w["resetsAt"].as_u64(),
        })
        .collect();
    windows.sort_by_key(|w| w.minutes.unwrap_or(u32::MAX));
    Some(SubscriptionUsage {
        account: "claude".into(),
        plan: None,
        windows,
        limited: info["status"].as_str().is_some_and(|s| s != "allowed"),
        reported_ms: now_ms(),
    })
}

/// Codex's `rateLimits` (from `account/rateLimits/read` or `account/rateLimits/updated`).
pub(crate) fn from_codex(limits: &Value) -> Option<SubscriptionUsage> {
    if !limits.is_object() {
        return None;
    }
    let windows = ["primary", "secondary"]
        .iter()
        .map(|k| &limits[*k])
        .filter(|w| w.is_object())
        .map(|w| UsageWindow {
            minutes: w["windowDurationMins"].as_u64().map(|m| m as u32),
            used_percent: percent(w["usedPercent"].as_f64().unwrap_or(0.0)),
            resets_at: w["resetsAt"].as_u64(),
        })
        .collect();
    Some(SubscriptionUsage {
        account: "codex".into(),
        plan: limits["planType"].as_str().map(str::to_string),
        windows,
        limited: !limits["rateLimitReachedType"].is_null()
            || limits["spendControlReached"].as_bool() == Some(true),
        reported_ms: now_ms(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Both payloads are from real runs (Claude Code 2.1.289, Codex 0.153.4, 2026-10-04).

    #[test]
    fn claude_reports_its_windows() {
        let info = json!({"status":"allowed","resetsAt":1791099600,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.35,"resetsAt":1791099600},"seven_day":{"utilization":0.26,"resetsAt":1791183600}}});
        let u = from_claude(&info).unwrap();
        assert_eq!(u.account, "claude");
        assert!(!u.limited);
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].minutes, Some(300));
        assert_eq!(u.windows[0].used_percent, 35);
        assert_eq!(u.windows[1].resets_at, Some(1791183600));
        let mut over = info.clone();
        over["status"] = json!("rejected");
        assert!(from_claude(&over).unwrap().limited);
        assert!(from_claude(&json!({"status":"allowed"})).is_none());
    }

    #[test]
    fn codex_reports_its_windows_and_plan() {
        let limits = json!({"limitId":"codex","limitName":null,"primary":{"usedPercent":8,"windowDurationMins":300,"resetsAt":1791101511},"secondary":{"usedPercent":15,"windowDurationMins":10080,"resetsAt":1791584132},"credits":{"hasCredits":false,"unlimited":false,"balance":"0"},"individualLimit":null,"spendControlReached":false,"planType":"plus","rateLimitReachedType":null});
        let u = from_codex(&limits).unwrap();
        assert_eq!(u.plan.as_deref(), Some("plus"));
        assert!(!u.limited);
        assert_eq!(
            u.windows,
            vec![
                UsageWindow {
                    minutes: Some(300),
                    used_percent: 8,
                    resets_at: Some(1791101511)
                },
                UsageWindow {
                    minutes: Some(10080),
                    used_percent: 15,
                    resets_at: Some(1791584132)
                },
            ]
        );
        let mut hit = limits.clone();
        hit["rateLimitReachedType"] = json!("primary");
        assert!(from_codex(&hit).unwrap().limited);
        assert!(from_codex(&Value::Null).is_none());
    }

    #[test]
    fn the_latest_report_is_kept_per_account() {
        record(SubscriptionUsage {
            account: "test-usage".into(),
            plan: None,
            windows: vec![],
            limited: false,
            reported_ms: 1,
        });
        record(SubscriptionUsage {
            account: "test-usage".into(),
            plan: None,
            windows: vec![],
            limited: true,
            reported_ms: 2,
        });
        assert!(usage_of("test-usage").unwrap().limited);
        assert_eq!(
            subscription_usage()
                .iter()
                .filter(|u| u.account == "test-usage")
                .count(),
            1
        );
    }
}
