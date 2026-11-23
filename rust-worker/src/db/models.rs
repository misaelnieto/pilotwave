//! Filas de `django_tasks_database_dbtaskresult` + objetos de consulta.
//!
//! Port del prototipo: sin SQL ni conexion, solo tipos. Los literales de
//! estado salen de [`crate::contract`] (fuente unica); aqui no hay strings
//! `"READY"`/`"RUNNING"` duplicados.

use crate::contract::{STATUS_FAILED, STATUS_READY, STATUS_RUNNING, STATUS_SUCCESSFUL};

/// Estado de listado del subcomando `list` (lote 7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    /// Literal exacto de la columna `status` (contrato: `READY`/`RUNNING`/
    /// `SUCCESSFUL`/`FAILED`). `Done` conserva el nombre del prototipo.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            TaskStatus::Ready => STATUS_READY,
            TaskStatus::Running => STATUS_RUNNING,
            TaskStatus::Done => STATUS_SUCCESSFUL,
            TaskStatus::Failed => STATUS_FAILED,
        }
    }
}

/// Query Object de `list`: estado + `LIMIT`/`OFFSET` ya validados.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListQuery {
    pub status: TaskStatus,
    pub limit: i64,
    pub offset: i64,
}

impl ListQuery {
    pub fn new(status: TaskStatus, limit: i64, offset: i64) -> Result<Self, String> {
        if !(1..=200).contains(&limit) {
            return Err(format!("--limit debe estar entre 1 y 200, got {limit}"));
        }
        if offset < 0 {
            return Err(format!("--offset debe ser >= 0, got {offset}"));
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
    /// `len(worker_ids)` tras el claim: contador de intentos (contrato §Claim).
    pub attempts: i64,
}

/// Conteos para `status`: disponibles (READY reclamables),
/// en proceso (RUNNING) y completadas (SUCCESSFUL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct QueueStatus {
    pub ready: i64,
    pub running: i64,
    pub done: i64,
}

/// Una fila para `list <estado>`.
/// `enqueued_at` se formatea en SQL (`to_char`) para no añadir
/// `chrono`/`time` a `sqlx` solo por este listado.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ListedTask {
    pub id: String,
    pub task_path: String,
    pub priority: i32,
    pub enqueued_at: String,
}

/// Una fila completa para `inspect <UUID>`.
/// Los `timestamptz` se formatean en SQL (`to_char`, como en `list`)
/// para no añadir `chrono`/`time` a las dependencias; los nulables
/// quedan `NULL` -> `None`. Los `jsonb` se seleccionan como `::text`.
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

/// Lectura del heartbeat para `status`: edad en segundos (frescura) +
/// `metadata` crudo (el render lo parsea best-effort, lote 7.4).
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
        // Los literales son los del contrato, sin duplicar strings aqui.
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
