//! Timeouts, output caps and retry policy shared by every provider.
//!
//! A provider classifies its own failures ([`ProviderError::is_transient`]); the caller
//! decides whether to try again with a [`RetryPolicy`]. The runtime retries a model call only
//! before any of its text reached the user and never re-runs a tool: tools run after a model
//! call returns, so retrying the call repeats the request, not its effects.

use std::time::Duration;

use crate::ProviderError;

/// HTTP limits for a provider's client. Defaults come from the environment:
///
/// | Variable | Default | Meaning |
/// |---|---:|---|
/// | `ARK_HTTP_CONNECT_TIMEOUT_S` | 10 | connecting to the endpoint |
/// | `ARK_HTTP_IDLE_TIMEOUT_S` | 120 | silence while waiting for the next bytes |
/// | `ARK_HTTP_TIMEOUT_S` | 600 | a whole non-streaming request |
/// | `ARK_MAX_OUTPUT_TOKENS` | per provider | output cap sent with each request |
#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
    /// Output cap sent with each request. `None` uses the provider's default.
    pub max_output_tokens: Option<u32>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        let secs = |k: &str, d: u64| {
            Duration::from_secs(
                std::env::var(k)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d),
            )
        };
        HttpConfig {
            connect_timeout: secs("ARK_HTTP_CONNECT_TIMEOUT_S", 10),
            idle_timeout: secs("ARK_HTTP_IDLE_TIMEOUT_S", 120),
            request_timeout: secs("ARK_HTTP_TIMEOUT_S", 600),
            max_output_tokens: std::env::var("ARK_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok()),
        }
    }
}

impl HttpConfig {
    /// A client with these limits. The idle timeout applies to every read, so a stream that
    /// goes silent fails instead of hanging; a long stream that keeps producing is fine.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .read_timeout(self.idle_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }
}

/// How often and how patiently to retry a failed model call.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Retries after the first attempt (`ARK_MODEL_RETRIES`, default 2).
    pub max_retries: u32,
    /// First backoff; it doubles each retry.
    pub base_delay: Duration,
    /// Longest single wait, including a server's `retry-after`.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: std::env::var("ARK_MODEL_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        RetryPolicy {
            max_retries: 0,
            ..RetryPolicy::default()
        }
    }

    /// Whether `err` after `attempt` retries is worth another try.
    pub fn should_retry(&self, err: &ProviderError, attempt: u32) -> bool {
        attempt < self.max_retries && err.is_transient()
    }

    /// The wait before retry number `attempt + 1`: exponential with full jitter, never
    /// shorter than the server's `retry-after`, never longer than `max_delay`.
    pub fn delay(&self, attempt: u32, err: &ProviderError) -> Duration {
        let cap = self
            .base_delay
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max_delay);
        let jittered = cap.mul_f64(0.5 + 0.5 * unit_random());
        match err.retry_after() {
            Some(server) => server.min(self.max_delay).max(jittered),
            None => jittered,
        }
    }
}

/// A number in [0, 1) that differs between calls. Jitter only needs to spread retries out,
/// not to be unpredictable.
fn unit_random() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut x = STATE.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) ^ seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Read `retry-after` (seconds) or `retry-after-ms` from a response.
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.trim().parse::<f64>().ok()) {
        return Some(Duration::from_secs_f64(ms / 1000.0));
    }
    get("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(Duration::from_secs_f64)
}

/// Turn a non-success response into an error, reading the detail with `detail` from the body.
pub(crate) async fn status_error(
    provider: &str,
    resp: reqwest::Response,
    detail: impl Fn(&serde_json::Value) -> Option<String>,
) -> ProviderError {
    let status = resp.status().as_u16();
    let retry_after = retry_after(resp.headers());
    let text = resp.text().await.unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let detail = detail(&body).unwrap_or_else(|| {
        let t = text.trim();
        if t.is_empty() {
            "request failed".to_string()
        } else {
            t.chars().take(300).collect()
        }
    });
    ProviderError::Status {
        provider: provider.to_string(),
        status,
        detail,
        retry_after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16) -> ProviderError {
        ProviderError::Status {
            provider: "p".into(),
            status: code,
            detail: String::new(),
            retry_after: None,
        }
    }

    #[test]
    fn transient_and_permanent_failures() {
        for code in [408, 429, 500, 502, 503, 504, 529] {
            assert!(status(code).is_transient(), "{code}");
        }
        for code in [400, 401, 403, 404, 413, 422] {
            assert!(!status(code).is_transient(), "{code}");
        }
        assert!(ProviderError::Overloaded {
            provider: "p".into(),
            detail: "x".into()
        }
        .is_transient());
        assert!(!ProviderError::Api {
            provider: "p".into(),
            detail: "bad".into()
        }
        .is_transient());
    }

    #[test]
    fn delays_grow_stay_bounded_and_respect_retry_after() {
        let p = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        };
        let e = status(503);
        for attempt in 0..6 {
            let d = p.delay(attempt, &e);
            let cap = Duration::from_millis(100 * (1 << attempt)).min(Duration::from_secs(2));
            assert!(d <= cap && d >= cap / 2, "attempt {attempt}: {d:?}");
        }
        let slow = ProviderError::Status {
            provider: "p".into(),
            status: 429,
            detail: String::new(),
            retry_after: Some(Duration::from_secs(1)),
        };
        assert!(p.delay(0, &slow) >= Duration::from_secs(1));
        let huge = ProviderError::Status {
            provider: "p".into(),
            status: 429,
            detail: String::new(),
            retry_after: Some(Duration::from_secs(3600)),
        };
        assert_eq!(p.delay(0, &huge), Duration::from_secs(2), "capped");
        assert!(!p.should_retry(&e, 5));
        assert!(p.should_retry(&e, 4));
        assert!(!p.should_retry(&status(400), 0));
    }
}
