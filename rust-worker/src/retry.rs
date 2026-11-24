//! Retry backoff.
//!
//! Fixed ladder: 30s -> 2m -> 8m -> 30m (cap). `attempts` is
//! `len(worker_ids)` *after* the claim, so the first failure
//! returns 30s and `None` (>=5) means `FAILED smtp_exhausted`.

use std::time::Duration;

/// Wait seconds per attempt; `None` = exhausted, finalize `FAILED`.
pub fn backoff_for_attempt(attempts: i64) -> Option<Duration> {
    match attempts {
        1 => Some(Duration::from_secs(30)),
        2 => Some(Duration::from_secs(120)),
        3 => Some(Duration::from_secs(480)),
        4 => Some(Duration::from_secs(1_800)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_ladder_y_agotamiento() {
        assert_eq!(backoff_for_attempt(1), Some(Duration::from_secs(30)));
        assert_eq!(backoff_for_attempt(2), Some(Duration::from_secs(120)));
        assert_eq!(backoff_for_attempt(3), Some(Duration::from_secs(480)));
        assert_eq!(backoff_for_attempt(4), Some(Duration::from_secs(1_800)));
        // >=5 => FAILED smtp_exhausted; 0 is not a valid attempt either.
        assert_eq!(backoff_for_attempt(5), None);
        assert_eq!(backoff_for_attempt(6), None);
        assert_eq!(backoff_for_attempt(0), None);
    }
}
