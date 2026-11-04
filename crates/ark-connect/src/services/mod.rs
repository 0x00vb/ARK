//! The tools a connected capability gives, by the service name its catalog entry names. Each
//! service is an [`Adapter`] with neutral action names; it asks [`Connections`] for a token on
//! every call, so a refreshed or revoked sign-in applies at once.

use std::sync::Arc;

use ark_tools::{Adapter, AdapterError};

use crate::Connections;

pub mod gmail;
pub mod google;
pub mod whatsapp;

/// The adapter for `service`, acting as `account`. `None`: no such service.
pub fn adapter(service: &str, conns: Connections, account: String) -> Option<Arc<dyn Adapter>> {
    let ctx = Ctx {
        service: service.to_string(),
        conns,
        account,
    };
    match service {
        "google.mail" => Some(Arc::new(google::Mail(ctx))),
        "google.calendar" => Some(Arc::new(google::Calendar(ctx))),
        "google.inbox" => Some(Arc::new(gmail::Inbox(ctx))),
        whatsapp::SERVICE => Some(Arc::new(whatsapp::Reply(ctx))),
        _ => None,
    }
}

/// The service delivers its events to a webhook ([`crate::Connections::webhook`]).
pub fn pushes(service: &str) -> bool {
    service == whatsapp::SERVICE
}

/// The provider connects with ids and tokens the person enters, not a sign-in.
pub fn by_credentials(provider: &str) -> bool {
    provider == whatsapp::PROVIDER
}

/// The poller of `service`'s events for `account` (`address`: who it is, as events name the
/// account). `None`: the service delivers no events.
pub fn feed(
    service: &str,
    conns: Connections,
    account: String,
    address: String,
) -> Option<Arc<dyn ark_tools::events::EventSource>> {
    let ctx = Ctx {
        service: service.to_string(),
        conns,
        account,
    };
    match service {
        "google.inbox" => Some(Arc::new(gmail::Feed::new(ctx, address))),
        _ => None,
    }
}

/// The evidence card of a service's result (the runtime's distiller shape). `None` for an
/// action no service here offers.
pub fn distill(action: &str, out: &serde_json::Value) -> Option<(String, Vec<String>)> {
    google::distill(action, out)
        .or_else(|| gmail::distill(action, out))
        .or_else(|| whatsapp::distill(action, out))
}

/// What a service needs to call its API as an account.
pub struct Ctx {
    service: String,
    conns: Connections,
    account: String,
}

impl Ctx {
    /// The API's base URL: `default`, or the one the connections were given for this service.
    pub fn base<'a>(&'a self, default: &'a str) -> &'a str {
        self.conns.base(&self.service).unwrap_or(default)
    }

    /// Send a request with the account's token. A refused token is dropped and the request
    /// sent once more with a new one.
    pub async fn send(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<serde_json::Value, AdapterError> {
        self.call(build)
            .await
            .map_err(|e| AdapterError::Tool(e.to_string()))
    }

    /// [`Ctx::send`] that keeps why it failed: the status, a lost sign-in, or the network.
    pub async fn call(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<serde_json::Value, CallError> {
        for attempt in 0..2 {
            let token = self
                .conns
                .access_token(&self.account)
                .await
                .map_err(|e| CallError::SignIn(e.to_string()))?;
            let resp = build(self.conns.http())
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| CallError::Network {
                    sent: !(e.is_connect() || e.is_builder()),
                    why: e.to_string(),
                })?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                self.conns.expire(&self.account);
                continue;
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after_seconds(v, time::OffsetDateTime::now_utc()));
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            if !status.is_success() {
                let why = body["error"]["message"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string();
                return Err(CallError::Status {
                    status: status.as_u16(),
                    why,
                    retry_after,
                });
            }
            return Ok(body);
        }
        Err(CallError::SignIn(
            "the account's sign-in was refused; connect it again".into(),
        ))
    }
}

fn retry_after_seconds(value: &str, now: time::OffsetDateTime) -> Option<u64> {
    use time::{macros::format_description, PrimitiveDateTime};

    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let imf = format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    let asctime = format_description!(
        "[weekday repr:short] [month repr:short] [day padding:space] [hour]:[minute]:[second] [year]"
    );
    let rfc850 = format_description!(
        "[weekday repr:long], [day]-[month repr:short]-[year repr:last_two] [hour]:[minute]:[second] GMT"
    );
    let mut parsed = [imf, asctime, rfc850].into_iter().find_map(|format| {
        let mut parsed = time::parsing::Parsed::new();
        parsed
            .parse_items(value.as_bytes(), format)
            .ok()?
            .is_empty()
            .then_some(parsed)
    })?;
    if parsed.year().is_none() {
        let mut year = (now.year() + 50).div_euclid(100) * 100 + i32::from(parsed.year_last_two()?);
        let candidate = (
            year,
            parsed.month()? as u8,
            parsed.day()?.get(),
            parsed.hour_24()?,
            parsed.minute()?,
            parsed.second()?,
            0,
        );
        let limit = (
            now.year() + 50,
            now.month() as u8,
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.nanosecond(),
        );
        if candidate > limit {
            year -= 100;
        }
        parsed.set_year(year)?;
    }
    let leap_second = parsed.second()? == 60;
    if leap_second {
        if parsed.hour_24()? != 23 || parsed.minute()? != 59 {
            return None;
        }
        parsed.set_second(59)?;
    }
    let date = PrimitiveDateTime::try_from(parsed)
        .ok()?
        .checked_add(time::Duration::seconds(i64::from(leap_second)))?
        .assume_utc();
    let nanos = (date - now).whole_nanoseconds();
    if nanos <= 0 {
        Some(0)
    } else {
        u64::try_from((nanos + 999_999_999) / 1_000_000_000).ok()
    }
}

/// Why a [`Ctx::call`] failed.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// No usable sign-in: not connected, revoked, or refused twice.
    #[error("{0}")]
    SignIn(String),
    /// The service answered with an error. `retry_after`: seconds it asked to wait.
    #[error("{status}: {why}")]
    Status {
        status: u16,
        why: String,
        retry_after: Option<u64>,
    },
    /// No answer. `sent`: the request may have reached the service.
    #[error("{why}")]
    Network { sent: bool, why: String },
}

#[cfg(test)]
mod tests {
    use super::retry_after_seconds;
    use time::macros::datetime;

    #[test]
    fn retry_after_preserves_numeric_seconds() {
        let now = datetime!(2026-10-17 12:00 UTC);
        for (value, expected) in [
            ("0", Some(0)),
            (" +42 ", Some(42)),
            ("00042", Some(42)),
            ("18446744073709551615", Some(u64::MAX)),
            ("18446744073709551616", None),
            ("-1", None),
            ("", None),
        ] {
            assert_eq!(retry_after_seconds(value, now), expected, "{value}");
        }
    }

    #[test]
    fn retry_after_accepts_the_three_http_date_forms() {
        let now = datetime!(1994-11-06 08:49:36.5 UTC);
        for value in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(retry_after_seconds(value, now), Some(1), "{value}");
            assert_eq!(
                retry_after_seconds(value, datetime!(1994-11-06 08:49:37 UTC)),
                Some(0),
                "{value}"
            );
            assert_eq!(
                retry_after_seconds(value, datetime!(1994-11-07 00:00 UTC)),
                Some(0),
                "{value}"
            );
        }
    }

    #[test]
    fn retry_after_accepts_leap_seconds_in_all_http_date_forms() {
        for value in [
            "Sat, 31 Dec 2016 23:59:60 GMT",
            "Saturday, 31-Dec-16 23:59:60 GMT",
            "Sat Dec 31 23:59:60 2016",
        ] {
            for (now, expected) in [
                (datetime!(2016-12-31 23:59:58.25 UTC), 2),
                (datetime!(2016-12-31 23:59:59.5 UTC), 1),
                (datetime!(2017-01-01 00:00 UTC), 0),
                (datetime!(2017-01-02 00:00 UTC), 0),
            ] {
                assert_eq!(retry_after_seconds(value, now), Some(expected), "{value}");
            }
        }
    }

    #[test]
    fn retry_after_rejects_leap_seconds_outside_the_last_minute() {
        let now = datetime!(2016-12-31 00:00 UTC);
        for value in [
            "Sat, 31 Dec 2016 22:59:60 GMT",
            "Saturday, 31-Dec-16 23:58:60 GMT",
            "Sat Dec 31 24:59:60 2016",
            "Sat, 31 Dec 2016 23:59:61 GMT",
            "Saturday, 31-Feb-16 23:59:60 GMT",
            "Sat Dec 31 23:59:60 2016 junk",
        ] {
            assert_eq!(retry_after_seconds(value, now), None, "{value}");
        }
    }

    #[test]
    fn retry_after_rejects_invalid_dates_and_non_gmt_zones() {
        let now = datetime!(2026-10-17 12:00 UTC);
        for value in [
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 06 Nov 1994 08:49:37 +0000",
            "Sunday, 06-Nov-94 08:49:37 PST",
            "Fri, 29 Feb 2025 00:00:00 GMT",
            "Sun, 06 Nov 1994 25:49:37 GMT",
            "Sun, 06 Nov 1994 08:49:37 GMT junk",
            "Sunday, 06-Nov-94 08:49:37 GMT junk",
            "tomorrow",
        ] {
            assert_eq!(retry_after_seconds(value, now), None, "{value}");
        }
    }

    #[test]
    fn retry_after_rounds_up_across_year_and_leap_day_boundaries() {
        assert_eq!(
            retry_after_seconds(
                "Wed, 01 Jan 2025 00:00:01 GMT",
                datetime!(2024-12-31 23:59:59.25 UTC)
            ),
            Some(2)
        );
        assert_eq!(
            retry_after_seconds(
                "Thu, 29 Feb 2024 00:00:00 GMT",
                datetime!(2024-02-28 23:59:59.001 UTC)
            ),
            Some(1)
        );
    }

    #[test]
    fn retry_after_resolves_obsolete_years_with_a_calendar_fifty_year_limit() {
        let now = datetime!(2026-10-17 12:00 UTC);
        let at_limit = "Saturday, 17-Oct-76 12:00:00 GMT";
        let full_limit = "Sat, 17 Oct 2076 12:00:00 GMT";
        assert_eq!(
            retry_after_seconds(at_limit, now),
            retry_after_seconds(full_limit, now)
        );
        assert_eq!(
            retry_after_seconds("Saturday, 17-Oct-76 12:00:01 GMT", now),
            Some(0)
        );
        assert_eq!(
            retry_after_seconds(
                "Monday, 01-Jan-10 00:00:00 GMT",
                datetime!(2090-01-01 00:00 UTC)
            ),
            retry_after_seconds(
                "Mon, 01 Jan 2110 00:00:00 GMT",
                datetime!(2090-01-01 00:00 UTC)
            )
        );
        assert_eq!(
            retry_after_seconds(
                "Tuesday, 29-Feb-00 00:00:00 GMT",
                datetime!(2050-02-28 00:00 UTC)
            ),
            Some(0)
        );
    }
}
