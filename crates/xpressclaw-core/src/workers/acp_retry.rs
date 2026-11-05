//! Recognize temporary provider failures from ACP error envelopes.

use std::{collections::hash_map::DefaultHasher, hash::Hash, hash::Hasher, time::Duration};

use agent_client_protocol::{Error, ErrorCode};
use serde_json::Value;

use std::sync::{Arc, Mutex};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

/// The dispatcher owns this guard even while ACP temporarily yields its slot.
pub(crate) struct DispatchPermit {
    capacity: Arc<RetryCapacity>,
}

pub(crate) struct RetryCapacity {
    semaphore: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    active: watch::Sender<bool>,
}

impl DispatchPermit {
    pub fn new(semaphore: Arc<Semaphore>, permit: OwnedSemaphorePermit) -> Self {
        Self {
            capacity: Arc::new(RetryCapacity {
                semaphore,
                permit: Mutex::new(Some(permit)),
                active: watch::channel(true).0,
            }),
        }
    }

    pub fn retry_capacity(&self) -> Arc<RetryCapacity> {
        self.capacity.clone()
    }
}

impl Drop for DispatchPermit {
    fn drop(&mut self) {
        self.capacity.active.send_replace(false);
        self.capacity.release();
    }
}

impl RetryCapacity {
    pub fn release(&self) {
        self.permit.lock().unwrap().take();
    }

    pub async fn reacquire(&self) -> bool {
        let mut active = self.active.subscribe();
        if !*active.borrow_and_update() {
            return false;
        }
        let permit = tokio::select! {
            biased;
            _ = active.changed() => return false,
            permit = self.semaphore.clone().acquire_owned() => permit,
        };
        let Ok(permit) = permit else {
            return false;
        };
        let mut slot = self.permit.lock().unwrap();
        if !*active.borrow() {
            return false;
        }
        *slot = Some(permit);
        true
    }
}

pub(super) const MAX_RETRIES: usize = 5;
pub(super) const RETRY_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy)]
pub(super) struct RetryAdvice {
    pub reason: &'static str,
    retry_after: Duration,
}

impl RetryAdvice {
    pub fn delay(self, attempt_id: &str, retry: usize) -> Option<Duration> {
        if retry >= MAX_RETRIES || self.retry_after > Duration::from_secs(300) {
            return None;
        }
        let base = Duration::from_secs((15_u64 << retry).min(120));
        let mut hash = DefaultHasher::new();
        (attempt_id, retry).hash(&mut hash);
        let jitter = Duration::from_millis(hash.finish() % (base.as_millis() as u64 / 4 + 1));
        Some((base + jitter).max(self.retry_after))
    }
}

#[derive(Default)]
struct Signals {
    reason: Option<&'static str>,
    permanent: bool,
    retry_after: Duration,
}

impl Signals {
    fn status(&mut self, code: i64) {
        match code {
            429 => self.reason = Some("Provider rate limit"),
            503 | 529 => self.reason = Some("Provider capacity unavailable"),
            400..=405 | 422 => self.permanent = true,
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if text.len() > 16_384 {
            return;
        }
        let text = text.to_ascii_lowercase();
        if [
            "insufficient_quota",
            "insufficient quota",
            "credit balance is too low",
            "billing_hard_limit",
            "billing limit",
            "authentication_error",
            "invalid api key",
            "invalid_api_key",
            "permission_denied",
            "context_length_exceeded",
        ]
        .iter()
        .any(|marker| text.contains(marker))
        {
            self.permanent = true;
        }
        if text.contains("rate_limit")
            || text.contains("rate limit")
            || text.contains("too_many_requests")
            || text.contains("too many requests")
        {
            self.reason = Some("Provider rate limit");
        } else if text.contains("overloaded_error")
            || text.contains("server_overloaded")
            || text.contains("server is overloaded")
            || text.contains("model is overloaded")
            || (text.contains("at capacity")
                && ["model", "provider", "server", "service"]
                    .iter()
                    .any(|subject| text.contains(subject)))
        {
            self.reason = Some("Provider capacity unavailable");
        }
    }

    fn retry_after(&mut self, value: &Value, milliseconds: bool) {
        let numeric = value
            .as_u64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()));
        let delay = if let Some(value) = numeric {
            if milliseconds {
                Duration::from_millis(value)
            } else {
                Duration::from_secs(value)
            }
        } else if let Some(date) = value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok())
        {
            (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
        } else {
            return;
        };
        self.retry_after = self.retry_after.max(delay);
    }

    fn data(&mut self, value: &Value, depth: usize) {
        if depth > 4 {
            return;
        }
        match value {
            Value::String(text) if text.len() <= 16_384 => {
                self.text(text);
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    self.data(&value, depth + 1);
                }
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    match key.to_ascii_lowercase().as_str() {
                        "status" | "statuscode" | "status_code" | "httpstatus" | "http_status"
                        | "code" => {
                            if let Some(code) = value
                                .as_i64()
                                .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
                            {
                                self.status(code);
                            } else if let Some(code) = value.as_str() {
                                self.text(code);
                            }
                        }
                        "type" | "error_type" | "message" => {
                            if let Some(text) = value.as_str() {
                                self.text(text);
                            }
                        }
                        "retry-after"
                        | "retry_after"
                        | "retryafter"
                        | "retry_after_seconds"
                        | "retryafterseconds" => self.retry_after(value, false),
                        "retry_after_ms" | "retryafterms" => self.retry_after(value, true),
                        "error" | "cause" | "data" | "response" | "headers" => {
                            self.data(value, depth + 1)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

pub(super) fn classify(error: &Error) -> Option<RetryAdvice> {
    // ACP v1 has no standard capacity/rate-limit code. Preserve its explicit
    // protocol/authentication failures and inspect only provider error data.
    if !matches!(error.code, ErrorCode::InternalError | ErrorCode::Other(_)) {
        return None;
    }
    let mut signals = Signals::default();
    signals.status(i32::from(error.code).into());
    signals.text(&error.message);
    if error.message.len() <= 16_384 {
        if let Ok(value) = serde_json::from_str::<Value>(&error.message) {
            signals.data(&value, 0);
        }
    }
    if let Some(data) = &error.data {
        signals.data(data, 0);
    }
    if signals.permanent {
        return None;
    }
    Some(RetryAdvice {
        reason: signals.reason?,
        retry_after: signals.retry_after,
    })
}

/// Only defer explicitly labelled error notices. A matching failed RPC is
/// still required before discarding a notice or retrying its prompt.
pub(super) fn is_notice(text: &str) -> bool {
    let text = text.trim();
    ["api error:", "provider error:", "error:"]
        .iter()
        .any(|prefix| {
            text.get(..prefix.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
                && classify(&Error::internal_error().data(text[prefix.len()..].trim())).is_some()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn abandoned_dispatch_cannot_reacquire_or_leak_a_shared_slot() {
        let semaphore = Arc::new(Semaphore::new(1));
        let owner = DispatchPermit::new(
            semaphore.clone(),
            semaphore.clone().acquire_owned().await.unwrap(),
        );
        let capacity = owner.retry_capacity();
        capacity.release();
        let other = semaphore.clone().acquire_owned().await.unwrap();
        let waiting = capacity.reacquire();
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            _ = &mut waiting => panic!("another Agent owns the slot"),
            _ = tokio::task::yield_now() => {},
        }
        drop(owner);
        assert!(!waiting.await);
        assert_eq!(semaphore.available_permits(), 0);
        drop(other);
        assert_eq!(semaphore.available_permits(), 1);
        assert!(!capacity.reacquire().await);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[test]
    fn recognizes_provider_envelopes_and_respects_retry_after() {
        for data in [
            json!({"status":429,"headers":{"Retry-After":"90"}}),
            json!({"error":{"type":"overloaded_error"},"retryAfterMs":90_000}),
            json!({"response":{"statusCode":503,"retry_after_seconds":90}}),
            json!(r#"{"error":{"code":"rate_limit_exceeded"},"retry_after":90}"#),
        ] {
            let advice = classify(&Error::internal_error().data(data)).unwrap();
            assert_eq!(advice.delay("attempt", 0), Some(Duration::from_secs(90)));
        }
        for status in [429, 503, 529] {
            assert!(classify(&Error::new(status, "Provider rejected request")).is_some());
        }
    }

    #[test]
    fn rejects_permanent_failures_even_with_a_rate_limit_status() {
        for code in [
            "insufficient_quota",
            "invalid_api_key",
            "billing_hard_limit_reached",
            "context_length_exceeded",
        ] {
            assert!(classify(
                &Error::new(429, "Too many requests").data(json!({"error":{"code":code}}))
            )
            .is_none());
        }
        for error in [
            Error::auth_required().data("Model at capacity"),
            Error::invalid_params().data("Model at capacity"),
            Error::request_cancelled(),
            Error::internal_error().data("Connection closed"),
            Error::internal_error().data("No space left on device"),
        ] {
            assert!(classify(&error).is_none());
        }
    }

    #[test]
    fn backoff_is_bounded_and_staggered_and_never_shortens_provider_delay() {
        let advice = classify(&Error::internal_error().data("Model is at capacity")).unwrap();
        let mut previous = Duration::ZERO;
        for retry in 0..MAX_RETRIES {
            let delay = advice.delay("attempt", retry).unwrap();
            let base = Duration::from_secs([15, 30, 60, 120, 120][retry]);
            assert!(delay >= base && delay <= base.mul_f64(1.25));
            if retry < 4 {
                assert!(delay > previous);
            }
            previous = delay;
        }
        assert!(advice.delay("attempt", MAX_RETRIES).is_none());
        assert_ne!(
            advice.delay("first-agent", 0),
            advice.delay("second-agent", 0)
        );
        let long_hint =
            classify(&Error::new(429, "Rate limit").data(json!({"retry_after":3600}))).unwrap();
        assert!(long_hint.delay("attempt", 0).is_none());
    }
}
