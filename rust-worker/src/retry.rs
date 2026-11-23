//! Backoff de reintentos (PLAN-003 §3.6).
//!
//! Escalera fija: 30s → 2m → 8m → 30m (cap). `attempts` es
//! `len(worker_ids)` *después* del claim, así que el primer fallo
//! devuelve 30s y `None` (≥5) significa `FAILED smtp_exhausted`.

use std::time::Duration;

/// Segundos de espera por intento; `None` = agotado, finalizar `FAILED`.
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
        // ≥5 => FAILED smtp_exhausted; 0 tampoco es un intento válido.
        assert_eq!(backoff_for_attempt(5), None);
        assert_eq!(backoff_for_attempt(6), None);
        assert_eq!(backoff_for_attempt(0), None);
    }
}
