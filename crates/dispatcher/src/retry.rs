//! Retry policy: exponential backoff and attempt limit.

use std::time::Duration;

/// Controls how failed deliveries are retried.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of attempts before a mail is buried as dead.
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub base_delay: Duration,
    /// Upper bound for the backoff delay.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(3600),
        }
    }
}

impl RetryPolicy {
    /// Backoff before the next attempt, given how many attempts have already
    /// been made (`attempts >= 1`). Exponential: `base * 2^(attempts - 1)`,
    /// capped at `max_delay`.
    pub fn backoff(&self, attempts: u32) -> Duration {
        let shift = attempts.saturating_sub(1).min(31);
        let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let secs = self.base_delay.as_secs().saturating_mul(factor);
        Duration::from_secs(secs).min(self.max_delay)
    }

    /// Whether a mail that has made `attempts` attempts has no retries left.
    pub fn is_exhausted(&self, attempts: u32) -> bool {
        attempts >= self.max_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(3600),
        };
        assert_eq!(policy.backoff(1), Duration::from_secs(30));
        assert_eq!(policy.backoff(2), Duration::from_secs(60));
        assert_eq!(policy.backoff(3), Duration::from_secs(120));
        assert_eq!(policy.backoff(4), Duration::from_secs(240));
    }

    #[test]
    fn backoff_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 20,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(300),
        };
        assert_eq!(policy.backoff(10), Duration::from_secs(300));
    }

    #[test]
    fn exhaustion_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_exhausted(4));
        assert!(policy.is_exhausted(5));
        assert!(policy.is_exhausted(6));
    }
}
