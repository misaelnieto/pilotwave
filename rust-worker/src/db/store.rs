//! Persistence abstraction (dependency inversion).
//!
//! The binary programs against this trait, not `PgPool`; the read-only
//! subcommands are tested with a fake that needs no Postgres.

use super::models::{ClaimedTask, Heartbeat, InspectedTask, ListQuery, ListedTask, QueueStatus};

#[allow(async_fn_in_trait)]
pub trait TaskStore {
    async fn count_due(&self) -> Result<i64, sqlx::Error>;
    async fn queue_status(&self) -> Result<QueueStatus, sqlx::Error>;
    async fn list_tasks(&self, query: &ListQuery) -> Result<Vec<ListedTask>, sqlx::Error>;
    /// Detail of one row by id, unfiltered by queue (debugging tool).
    async fn inspect(&self, id: &str) -> Result<Option<InspectedTask>, sqlx::Error>;
    async fn claim(&self) -> Result<Option<ClaimedTask>, sqlx::Error>;
    /// Claim one specific row if it is runnable: READY + due +
    /// in scope (queue/backend/task_path). Otherwise (missing, RUNNING,
    /// future, out of scope) return `None` untouched -- never steal another
    /// worker's work.
    async fn claim_by_id(&self, id: &str) -> Result<Option<ClaimedTask>, sqlx::Error>;
    async fn finalize_success(&self, id: &str) -> Result<(), sqlx::Error>;
    async fn finalize_failed(&self, id: &str, code: &str, detail: &str) -> Result<(), sqlx::Error>;
    /// Requeue a transient: `READY` + `run_after = NOW() + delay` (contract Failure codes).
    async fn requeue_transient(&self, id: &str, delay_secs: i64) -> Result<(), sqlx::Error>;
    /// Reaper for our own zombies: old `RUNNING` rows with a `pilotwave-*` suffix.
    /// Returns requeued rows. Never touches rows from other worker families.
    async fn reap_zombies(&self) -> Result<u64, sqlx::Error>;
    /// Write the sidecar heartbeat (upsert by key, contract Heartbeat).
    /// `metadata_json` carries counters + operational identity, never PII.
    async fn write_heartbeat(&self, metadata_json: &str) -> Result<(), sqlx::Error>;
    /// Read the heartbeat for `status`: `None` when the sidecar has not written yet.
    async fn read_heartbeat(&self) -> Result<Option<Heartbeat>, sqlx::Error>;
}
