//! Filas de `django_tasks_database_dbtaskresult` + objetos de consulta.
//!
//! No SQL, no connections, only types. Status literals come from
//! [`crate::contract`] (single source); no duplicated
//! `"READY"`/`"RUNNING"` strings here.

use crate::contract::{STATUS_FAILED, STATUS_READY, STATUS_RUNNING, STATUS_SUCCESSFUL};

/// List state for the `list` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    /// Exact literal of the `status` column (contract: `READY`/`RUNNING`/
    /// `SUCCESSFUL`/`FAILED`).
    pub fn as_db_str(&self) -> &'static str {
        match self {
            TaskStatus::Ready => STATUS_READY,
            TaskStatus::Running => STATUS_RUNNING,
            TaskStatus::Done => STATUS_SUCCESSFUL,
            TaskStatus::Failed => STATUS_FAILED,
        }
    }
}

/// Query object for `list`: state + already-validated `LIMIT`/`OFFSET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListQuery {
    pub status: TaskStatus,
    pub limit: i64,
    pub offset: i64,
}

impl ListQuery {
    pub fn new(status: TaskStatus, limit: i64, offset: i64) -> Result<Self, String> {
        if !(1..=200).contains(&limit) {
            return Err(format!("--limit must be between 1 and 200, got {limit}"));
        }
        if offset < 0 {
            return Err(format!("--offset must be >= 0, got {offset}"));
        }
        Ok(Self {
            status,
            limit,
            offset,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct ClaimedTask {
    pub id: String,
    pub args_kwargs: String,
    /// `len(worker_ids)` after the claim: attempt counter (contract Claim section).
    pub attempts: i64,
}

/// Counts for `status`: available (claimable READY),
/// in-flight (RUNNING) and completed (SUCCESSFUL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct QueueStatus {
    pub ready: i64,
    pub running: i64,
    pub done: i64,
}

/// One row for `list <state>`.
/// `enqueued_at` is formatted in SQL (`to_char`) to avoid adding
/// `chrono`/`time` to `sqlx` just for this listing.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ListedTask {
    pub id: String,
    pub task_path: String,
    pub priority: i32,
    pub enqueued_at: String,
}

/// One full row for `inspect <UUID>`.
/// `timestamptz` values are formatted in SQL (`to_char`, as in `list`)
/// to keep `chrono`/`time` out of the dependencies; nullables
/// come back `NULL` -> `None`. `jsonb` columns are selected as `::text`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct InspectedTask {
    pub id: String,
    pub status: String,
    pub task_path: String,
    pub queue_name: String,
    pub backend_name: String,
    pub priority: i32,
    pub run_after: String,
    pub enqueued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub worker_ids: String,
    pub return_value: Option<String>,
    pub exception_class_path: String,
    pub traceback: String,
    pub args_kwargs: String,
}

/// Heartbeat read for `status`: age in seconds (freshness) +
/// raw `metadata` (the renderer parses it best-effort).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Heartbeat {
    pub last_seen: String,
    pub age_secs: i64,
    pub metadata: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_y_list_query_rangos() {
        // Literals come from the contract, no duplicated strings here.
        assert_eq!(TaskStatus::Ready.as_db_str(), STATUS_READY);
        assert_eq!(TaskStatus::Running.as_db_str(), STATUS_RUNNING);
        assert_eq!(TaskStatus::Done.as_db_str(), STATUS_SUCCESSFUL);
        assert_eq!(TaskStatus::Failed.as_db_str(), STATUS_FAILED);
        assert!(ListQuery::new(TaskStatus::Ready, 25, 0).is_ok());
        assert!(ListQuery::new(TaskStatus::Ready, 0, 0).is_err());
        assert!(ListQuery::new(TaskStatus::Ready, 201, 0).is_err());
        assert!(ListQuery::new(TaskStatus::Ready, 10, -1).is_err());
    }
}
