//! Abstraccion de persistencia (Dependency Inversion).
//!
//! Port del prototipo: el binario programa contra este trait, no contra
//! `PgPool`; los subcomandos de solo lectura (lote 7.4) se prueban con un
//! fake sin PG.

use super::models::{ClaimedTask, Heartbeat, InspectedTask, ListQuery, ListedTask, QueueStatus};

#[allow(async_fn_in_trait)]
pub trait TaskStore {
    async fn count_due(&self) -> Result<i64, sqlx::Error>;
    async fn queue_status(&self) -> Result<QueueStatus, sqlx::Error>;
    async fn list_tasks(&self, query: &ListQuery) -> Result<Vec<ListedTask>, sqlx::Error>;
    /// Detalle de una fila por id, sin filtrar por cola (herramienta de depuración).
    async fn inspect(&self, id: &str) -> Result<Option<InspectedTask>, sqlx::Error>;
    async fn claim(&self) -> Result<Option<ClaimedTask>, sqlx::Error>;
    /// Reclama una fila concreta si es ejecutable: READY + vencida +
    /// dentro del alcance (cola/backend/task_path). Si no lo es
    /// (inexistente, RUNNING, futura, fuera de alcance) devuelve `None`
    /// sin tocarla — nunca roba trabajo ajeno.
    async fn claim_by_id(&self, id: &str) -> Result<Option<ClaimedTask>, sqlx::Error>;
    async fn finalize_success(&self, id: &str) -> Result<(), sqlx::Error>;
    async fn finalize_failed(&self, id: &str, code: &str, detail: &str) -> Result<(), sqlx::Error>;
    /// Reencola un transitorio: `READY` + `run_after = NOW() + delay` (contrato §Failure codes).
    async fn requeue_transient(&self, id: &str, delay_secs: i64) -> Result<(), sqlx::Error>;
    /// Reaper de zombies propios: `RUNNING` viejos con sufijo `pilotwave-*`.
    /// Devuelve filas reencoladas. Nunca toca filas del fallback Python.
    async fn reap_zombies(&self) -> Result<u64, sqlx::Error>;
    /// Escribe el heartbeat del sidecar (upsert por clave, contrato §Heartbeat).
    /// `metadata_json` sale del render de 7.4 (contadores + identidad
    /// operativa, nunca PII).
    async fn write_heartbeat(&self, metadata_json: &str) -> Result<(), sqlx::Error>;
    /// Lee el heartbeat para `status`: `None` si el sidecar aún no escribió.
    async fn read_heartbeat(&self) -> Result<Option<Heartbeat>, sqlx::Error>;
}
