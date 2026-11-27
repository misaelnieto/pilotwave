//! Sidecar `PilotwaveHeartbeat` writes.
//!
//! The `pilotwave_heartbeat(key='pilotwave_worker')` row is the canonical
//! liveness source: Django (`get_pilotwave_status`) only reads this row
//! (never `ps`/pidfile/`/proc` in the request path) and `status`
//! shows it when a DB is available, degrading to the local probe otherwise.
//!
//! Pace: every poll cycle **and** before/after each send (with a single
//! connection and a sequential loop, a 30 s send would age the heartbeat
//! past the honest threshold when written only once per cycle).
//! The `metadata` contract is counters-only + operational identity of the
//! process (pid/uptime/rss): never recipients, subjects, bodies,
//! `form_data`, error texts, DSNs or attachment paths.

/// Row key: reuses the contract (single source).
pub use crate::contract::HEARTBEAT_KEY;

/// `metadata` schema version (starts at 1).
pub const HEARTBEAT_VERSION: u32 = 1;

/// Stale threshold: `TASK_TIMEOUT 30` + `POLL_MAX 5` + margin. Mirrors the
/// Django `PILOTWAVE_STALE_AFTER_SECS = 90` (the generic 10-minute rule
/// does NOT apply to this key).
pub const STALE_AFTER_SECS: i64 = 90;

/// `fresh` when `age_secs` is within the threshold, `stale` otherwise.
pub fn freshness(age_secs: i64) -> &'static str {
    if age_secs <= STALE_AFTER_SECS {
        "fresh"
    } else {
        "stale"
    }
}

/// Counters and operational identity for one heartbeat write.
/// Everything comes from the DB (counts) or from our own `/proc`
/// (best-effort); no field carries PII.
pub struct BeatCounters {
    /// Claimable `COUNT READY` seen in this cycle (`due_depth`).
    pub queue_depth_seen: i64,
    /// Tasks processed since process start.
    pub tasks_processed: u64,
    /// UUID of the last touched task (the id only, never the content).
    pub last_task_id: Option<String>,
    /// Last `pilotwave_worker.*` (code only, never attacker-text detail).
    pub last_error_code: Option<String>,
    /// Own PID (cross-check with the `status` local probe).
    pub pid: u32,
    /// Best-effort (`None` => `null`, el lector pinta `n/a`).
    pub uptime_secs: Option<u64>,
    /// Best-effort (`None` => `null`).
    pub rss_kb: Option<u64>,
}

/// Serialize `metadata` as JSON (keys ordered by construction).
/// Pure function for DB-free testing.
pub fn render_metadata(c: &BeatCounters) -> String {
    serde_json::json!({
        "version": HEARTBEAT_VERSION,
        "queue_depth_seen": c.queue_depth_seen,
        "tasks_processed": c.tasks_processed,
        "last_task_id": c.last_task_id,
        "last_error_code": c.last_error_code,
        "pid": c.pid,
        "uptime_secs": c.uptime_secs,
        "rss_kb": c.rss_kb,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BeatCounters {
        BeatCounters {
            queue_depth_seen: 3,
            tasks_processed: 42,
            last_task_id: Some("11111111-2222-3333-4444-555555555555".to_owned()),
            last_error_code: Some("pilotwave_worker.smtp_transient".to_owned()),
            pid: 1234,
            uptime_secs: Some(3723),
            rss_kb: Some(4192),
        }
    }

    #[test]
    fn metadata_lleva_contadores_identidad_y_nulos() {
        let v: serde_json::Value = serde_json::from_str(&render_metadata(&sample())).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["queue_depth_seen"], 3);
        assert_eq!(v["tasks_processed"], 42);
        assert_eq!(v["last_task_id"], "11111111-2222-3333-4444-555555555555");
        assert_eq!(v["last_error_code"], "pilotwave_worker.smtp_transient");
        assert_eq!(v["pid"], 1234);
        assert_eq!(v["uptime_secs"], 3723);
        assert_eq!(v["rss_kb"], 4192);
        // Missing => null without panicking (/proc best-effort).
        let v: serde_json::Value = serde_json::from_str(&render_metadata(&BeatCounters {
            queue_depth_seen: 0,
            tasks_processed: 0,
            last_task_id: None,
            last_error_code: None,
            pid: 1,
            uptime_secs: None,
            rss_kb: None,
        }))
        .unwrap();
        assert!(v["last_task_id"].is_null());
        assert!(v["last_error_code"].is_null());
        assert!(v["uptime_secs"].is_null());
        assert!(v["rss_kb"].is_null());
    }

    #[test]
    fn freshness_corta_en_90s() {
        assert_eq!(freshness(0), "fresh");
        assert_eq!(freshness(90), "fresh");
        assert_eq!(freshness(91), "stale");
        assert_eq!(freshness(3_600), "stale");
    }
}
