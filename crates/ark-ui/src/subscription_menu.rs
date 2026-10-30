use ark_interact::ark_sdk::prelude::ark_model::{
    accounts::{AccountState, ModelAccount},
    SubscriptionUsage,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubscriptionEntry {
    pub id: String,
    pub label: String,
    pub lines: Vec<String>,
}

pub(crate) fn entries(
    accounts: &[ModelAccount],
    reports: &[SubscriptionUsage],
    defaults: &[(String, Option<String>)],
    now_ms: u64,
) -> Vec<SubscriptionEntry> {
    let mut entries = Vec::new();
    for provider in ["claude", "codex"] {
        let name = if provider == "claude" {
            "Claude"
        } else {
            "Codex"
        };
        if let Some((_, identity)) = defaults.iter().find(|(id, _)| id == provider) {
            entries.push(entry(
                provider,
                &format!(
                    "{name} · {}",
                    identity.as_deref().unwrap_or("This computer")
                ),
                AccountState::Active,
                reports,
                now_ms,
            ));
        }
        for account in accounts
            .iter()
            .filter(|a| a.provider == provider && !a.removed)
        {
            entries.push(entry(
                &account.id,
                &format!("{name} · {}", account.name),
                account.state,
                reports,
                now_ms,
            ));
        }
    }
    entries
}

fn entry(
    id: &str,
    label: &str,
    state: AccountState,
    reports: &[SubscriptionUsage],
    now_ms: u64,
) -> SubscriptionEntry {
    let report = reports.iter().find(|r| r.account == id);
    let mut lines = match state {
        AccountState::Reconnecting => vec!["Reconnect required".into()],
        AccountState::Removing => vec!["Removing account".into()],
        AccountState::Active => report.map_or_else(
            || vec!["Usage unavailable".into()],
            |r| {
                let mut lines = Vec::new();
                if r.limited {
                    lines.push("Limit reached".into());
                }
                for window in &r.windows {
                    let duration = match window.minutes {
                        Some(10080) => "Week".into(),
                        Some(m) if m >= 60 && m % 60 == 0 => format!("{}h", m / 60),
                        Some(m) => format!("{m}m"),
                        None => String::new(),
                    };
                    let bucket = window.bucket.as_deref().unwrap_or("");
                    let window_name = [bucket.to_string(), duration]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let reset = match window.resets_at {
                        Some(at) if at <= now_ms / 1000 => " · refresh needed".into(),
                        Some(at) => {
                            let minutes = (at - now_ms / 1000).div_ceil(60);
                            if minutes < 60 {
                                format!(" · resets in {minutes}m")
                            } else if minutes < 1440 {
                                format!(" · resets in {}h", minutes.div_ceil(60))
                            } else {
                                format!(" · resets in {}d", minutes.div_ceil(1440))
                            }
                        }
                        None => String::new(),
                    };
                    let prefix = if window_name.is_empty() {
                        String::new()
                    } else {
                        format!("{window_name} · ")
                    };
                    lines.push(format!(
                        "{prefix}{}% left{reset}",
                        100u32.saturating_sub(window.used_percent)
                    ));
                }
                if lines.is_empty() {
                    lines.push("Usage unavailable".into());
                }
                lines
            },
        ),
    };
    if state == AccountState::Active {
        if let Some(report) = report {
            let minutes = now_ms.saturating_sub(report.reported_ms) / 60000;
            if minutes >= 5 {
                lines.push(if minutes < 60 {
                    format!("Reported {minutes}m ago")
                } else {
                    format!("Reported {}h ago", minutes / 60)
                });
            }
        }
    }
    SubscriptionEntry {
        id: id.into(),
        label: label.chars().take(64).collect(),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_interact::ark_sdk::prelude::ark_model::UsageWindow;

    #[test]
    fn accounts_keep_separate_windows_and_expired_reports_do_not_refill() {
        let accounts = vec![ModelAccount {
            id: "claude~work".into(),
            provider: "claude".into(),
            name: "Work".into(),
            identity: None,
            revision: 1,
            state: AccountState::Active,
            removed: false,
        }];
        let reports = vec![
            SubscriptionUsage {
                account: "claude".into(),
                plan: None,
                windows: vec![UsageWindow {
                    bucket: None,
                    minutes: Some(300),
                    used_percent: 25,
                    resets_at: Some(2000),
                }],
                limited: false,
                reported_ms: 1000000,
            },
            SubscriptionUsage {
                account: "claude~work".into(),
                plan: None,
                windows: vec![UsageWindow {
                    bucket: Some("Reviews".into()),
                    minutes: Some(10080),
                    used_percent: 80,
                    resets_at: Some(900),
                }],
                limited: false,
                reported_ms: 100000,
            },
        ];
        let rows = entries(&accounts, &reports, &[("claude".into(), None)], 1000000);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Claude · This computer");
        assert_eq!(rows[0].lines, ["5h · 75% left · resets in 17m"]);
        assert_eq!(
            rows[1].lines,
            [
                "Reviews · Week · 20% left · refresh needed",
                "Reported 15m ago"
            ]
        );
    }

    #[test]
    fn missing_usage_and_pending_accounts_never_invent_percentages() {
        let mut account = ModelAccount {
            id: "codex~work".into(),
            provider: "codex".into(),
            name: "Work".into(),
            identity: None,
            revision: 1,
            state: AccountState::Reconnecting,
            removed: false,
        };
        assert_eq!(
            entries(&[account.clone()], &[], &[], 0)[0].lines,
            ["Reconnect required"]
        );
        account.state = AccountState::Active;
        assert_eq!(
            entries(&[account], &[], &[], 0)[0].lines,
            ["Usage unavailable"]
        );
        assert!(entries(&[], &[], &[], 0).is_empty());
    }
}
