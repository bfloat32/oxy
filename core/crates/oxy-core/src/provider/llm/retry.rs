//! What to do when a model server says "not now".
//!
//! A local server that is still loading a model answers 503 for a while; a
//! rate-limited endpoint answers 429 with a `Retry-After`. Neither was handled
//! at all: the turn failed, the card said so, and the user pressed Ctrl+Enter
//! again. The rules here are deliberately small, and each one is a failure
//! worth not rediscovering:
//!
//! - a server-supplied delay is honoured, but **capped at 60 s**, so a
//!   malformed or hostile upstream cannot stall a turn indefinitely;
//! - the parse saturates, so an arbitrarily long digit string is safe;
//! - a value we cannot parse costs one backoff step, not the turn.
//!
//! Only delta-seconds are parsed. A date-form `Retry-After` (rare, and sent by
//! remote APIs rather than a loopback server) falls back to our own backoff —
//! when the client grows TLS and remote endpoints, the date parse belongs here.

use std::time::Duration;

/// Longest server-requested delay honoured.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// How many times one question is sent before the card gets the error.
pub const MAX_ATTEMPTS: u32 = 3;

/// Our own backoff when the server named no delay: 400ms, 800ms, …
pub const BACKOFF_BASE: Duration = Duration::from_millis(400);

/// `Retry-After: 12`. A date, a negative or an empty value is `None`.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let max_secs = MAX_RETRY_AFTER.as_secs();
    let secs = value.bytes().fold(0u64, |acc, b| {
        acc.saturating_mul(10)
            .saturating_add(u64::from(b - b'0'))
            .min(max_secs)
    });
    Some(Duration::from_secs(secs))
}

/// Is this status worth sending the same question again? 429 is the rate
/// limit, 5xx is the server's own trouble, and 408/425 are timeouts the
/// client is allowed to repeat. Everything else — a bad model name, a bad
/// body — is a bug in our request and repeating it changes nothing.
pub fn retryable(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500..=599)
}

/// How long to wait after attempt `attempt` (1-based) failed. `server` is the
/// header when it sent one.
pub fn wait_before(attempt: u32, server: Option<Duration>) -> Duration {
    if let Some(delay) = server {
        return delay.min(MAX_RETRY_AFTER);
    }
    BACKOFF_BASE
        .saturating_mul(1u32 << attempt.saturating_sub(1).min(8))
        .min(MAX_RETRY_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_seconds_are_honoured() {
        assert_eq!(parse_retry_after("12"), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after(" 0 "), Some(Duration::from_secs(0)));
    }

    #[test]
    fn a_hostile_delay_is_capped_and_cannot_overflow() {
        assert_eq!(
            parse_retry_after("999999999999999999999999"),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(parse_retry_after("3600"), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn a_date_or_a_junk_value_is_ours_to_decide() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after(""), None);
        // …and a missing header falls back to the backoff, never to zero.
        assert_eq!(wait_before(1, None), BACKOFF_BASE);
        assert_eq!(wait_before(2, None), BACKOFF_BASE * 2);
    }

    #[test]
    fn the_server_delay_wins_over_ours() {
        assert_eq!(
            wait_before(3, Some(Duration::from_millis(50))),
            Duration::from_millis(50)
        );
        assert_eq!(
            wait_before(3, Some(Duration::from_secs(9999))),
            MAX_RETRY_AFTER
        );
    }

    #[test]
    fn only_the_transient_statuses_are_retried() {
        for status in [408, 425, 429, 500, 502, 503, 599] {
            assert!(retryable(status), "{status} is transient");
        }
        for status in [200, 201, 400, 401, 403, 404, 413, 422] {
            assert!(!retryable(status), "{status} is our own bug");
        }
    }
}
