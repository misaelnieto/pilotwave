//! Implementacion PostgreSQL de [`super::store::TaskStore`].
//!
//! Port del prototipo: unico lugar con SQL. Todo es `SELECT`/estatico con
//! binds — nunca interpolacion de payload.
//!
//! Los literales SQL (`READY`, tablas, sentinel) repiten el texto del
//! contrato a proposito: el SQL debe ser estatico y auditable en un solo
//! lugar. El test `sql_usa_literales_del_contrato` lo ancla a
//! [`crate::contract`] para que cualquier deriva falle en `cargo test`.

use sqlx::postgres::PgPool;

use super::models::{ClaimedTask, Heartbeat, InspectedTask, ListQuery, ListedTask, QueueStatus};
use super::scope::QueueScope;
use super::store::TaskStore;
use crate::contract::TRACEBACK_MAX_BYTES;
use crate::heartbeat::HEARTBEAT_KEY;

pub struct PostgresTaskStore {
    pool: PgPool,
    scope: QueueScope,
    worker_id: String,
}

/// Palabras cuyos `clave=valor` se redactan antes de persistir (contrato
/// §Failure codes: `traceback` siempre pasa por redaccion + truncado).
const SECRET_KEYS: &[&str] = &["password", "secret", "token", "key", "dsn", "url"];

/// Redacta valores `clave=valor` / `clave:valor` para las claves de
/// `SECRET_KEYS` (case-insensitive) y trunca a [`TRACEBACK_MAX_BYTES`]. Se
/// aplica tanto a la escritura en BD como a los logs debug: `err.detail`
/// incluye texto atacante (`invalid to: ...`) que puede portar
/// `password=...`.
pub fn sanitize_detail(raw: &str) -> String {
    let redacted = redact_secrets(raw);
    truncate_bytes(&redacted, TRACEBACK_MAX_BYTES).to_owned()
}

fn redact_secrets(raw: &str) -> String {
    let mut out = raw.to_owned();
    // Bucle: en cada pasada se redacta la ÚLTIMA ocurrencia `clave=valor` y
    // se vuelve a escanear `out` desde cero, así los índices siempre son
    // vigentes (cada redacción elimina el valor: progreso garantizado).
    while let Some((start, end)) = last_secret_value(&out) {
        out.replace_range(start..end, "[redacted]");
    }
    out
}

/// Último `clave=valor`/`clave:valor` (case-insensitive) con el rango del
/// valor en bytes, o `None`. El valor termina en el primer delimitador o a
/// los 256 bytes; el avance es por char para no partir UTF-8.
fn last_secret_value(hay: &str) -> Option<(usize, usize)> {
    let bytes = hay.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    for key in SECRET_KEYS {
        let kb = key.as_bytes();
        if kb.len() > bytes.len() {
            continue;
        }
        for pos in (0..=bytes.len() - kb.len()).rev() {
            if !bytes[pos..pos + kb.len()].eq_ignore_ascii_case(kb) {
                continue;
            }
            let mut i = pos + kb.len();
            while hay[i..].starts_with([' ', '\t']) {
                i += 1;
            }
            if !hay[i..].starts_with(['=', ':']) {
                continue;
            }
            i += 1;
            while hay[i..].starts_with([' ', '\t']) {
                i += 1;
            }
            let mut end = i;
            while end < hay.len()
                && !hay[end..].starts_with([' ', '\t', '\r', '\n', ',', ';', '\'', '"'])
                && end - i < 256
            {
                end += hay[end..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            if end > i {
                // `[redacted]` es terminal: si el valor ya lo es, seguir
                // buscando ocurrencias anteriores en vez de re-redactarlo
                // (aquí estaba el bucle infinito).
                if &hay[i..end] == "[redacted]" {
                    continue;
                }
                if best.is_none_or(|(bs, _)| pos >= bs) {
                    best = Some((i, end));
                }
            }
            break; // solo la última ocurrencia de esta clave importa
        }
    }
    best
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl PostgresTaskStore {
    pub fn new(pool: PgPool, scope: QueueScope, worker_id: String) -> Self {
        Self {
            pool,
            scope,
            worker_id,
        }
    }

    // Sin `close()` explícito: `sqlx::Pool` se cierra por drop y ningún
    // camino de salida (incluidos los `?` tempranos) puede saltárselo.
}

const STATUS_SQL: &str = r#"
    SELECT
        COUNT(*) FILTER (
            WHERE status = 'READY'
              AND (run_after <= NOW() OR run_after = '9999-01-01T00:00:00Z')
        )::bigint AS ready,
        COUNT(*) FILTER (WHERE status = 'RUNNING')::bigint AS running,
        COUNT(*) FILTER (WHERE status = 'SUCCESSFUL')::bigint AS done
    FROM django_tasks_database_dbtaskresult
    WHERE queue_name = $1
      AND backend_name = $2
      AND task_path = ANY($3)
    "#;

const LIST_SQL: &str = r#"
    SELECT
        id::text AS id,
        task_path,
        priority,
        to_char(enqueued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS enqueued_at
    FROM django_tasks_database_dbtaskresult
    WHERE queue_name = $1
      AND backend_name = $2
      AND task_path = ANY($3)
      AND status = $4
    ORDER BY priority DESC, enqueued_at ASC
    LIMIT $5
    OFFSET $6
    "#;

const CLAIM_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'RUNNING',
        started_at = NOW(),
        worker_ids = worker_ids || to_jsonb($4::text)
    WHERE id = (
        SELECT id FROM django_tasks_database_dbtaskresult
        WHERE status = 'READY'
          AND queue_name = $1
          AND backend_name = $2
          AND task_path = ANY($3)
          AND (run_after <= NOW() OR run_after = '9999-01-01T00:00:00Z')
        ORDER BY priority DESC, run_after ASC, enqueued_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT 1
    )
    RETURNING id::text AS id, args_kwargs::text AS args_kwargs,
        jsonb_array_length(worker_ids)::bigint AS attempts
    "#;

const FINALIZE_SUCCESS_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'SUCCESSFUL',
        finished_at = NOW()
    WHERE id = $1::uuid
      AND status = 'RUNNING'
    "#;

const CLAIM_BY_ID_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'RUNNING',
        started_at = NOW(),
        worker_ids = worker_ids || to_jsonb($5::text)
    WHERE id = (
        SELECT id FROM django_tasks_database_dbtaskresult
        WHERE id = $1::uuid
          AND status = 'READY'
          AND queue_name = $2
          AND backend_name = $3
          AND task_path = ANY($4)
          AND (run_after <= NOW() OR run_after = '9999-01-01T00:00:00Z')
        FOR UPDATE SKIP LOCKED
        LIMIT 1
    )
    RETURNING id::text AS id, args_kwargs::text AS args_kwargs,
        jsonb_array_length(worker_ids)::bigint AS attempts
    "#;

const COUNT_DUE_SQL: &str = r#"
    SELECT count(*)
    FROM django_tasks_database_dbtaskresult
    WHERE status = 'READY'
      AND queue_name = $1
      AND backend_name = $2
      AND task_path = ANY($3)
      AND (run_after <= NOW() OR run_after = '9999-01-01T00:00:00Z')
    "#;

const FINALIZE_FAILED_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'FAILED',
        finished_at = NOW(),
        exception_class_path = $2,
        traceback = $3
    WHERE id = $1::uuid
      AND status = 'RUNNING'
    "#;

const REQUEUE_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'READY',
        run_after = NOW() + ($2 * INTERVAL '1 second')
    WHERE id = $1::uuid
      AND status = 'RUNNING'
    "#;

const REAPER_SQL: &str = r#"
    UPDATE django_tasks_database_dbtaskresult
    SET status = 'READY'
    WHERE status = 'RUNNING'
      AND queue_name = $1
      AND backend_name = $2
      AND task_path = ANY($3)
      AND started_at < NOW() - INTERVAL '5 minutes'
      AND worker_ids->>-1 LIKE 'pilotwave-%'
    "#;

const INSPECT_SQL: &str = r#"
    SELECT
        id::text AS id,
        status,
        task_path,
        queue_name,
        backend_name,
        priority,
        to_char(run_after AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS run_after,
        to_char(enqueued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS enqueued_at,
        to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS started_at,
        to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS finished_at,
        worker_ids::text AS worker_ids,
        return_value::text AS return_value,
        exception_class_path,
        traceback,
        args_kwargs::text AS args_kwargs
    FROM django_tasks_database_dbtaskresult
    WHERE id = $1::uuid
    "#;

/// Upsert del heartbeat (contrato §Heartbeat): la fila es la fuente canonica
/// de vida. `last_seen` lo pone la BD (`NOW()`), nunca el reloj del proceso.
const HEARTBEAT_UPSERT_SQL: &str = r#"
    INSERT INTO pilotwave_heartbeat (key, last_seen, metadata)
    VALUES ($1, NOW(), $2::jsonb)
    ON CONFLICT (key) DO UPDATE
    SET last_seen = NOW(), metadata = EXCLUDED.metadata
    "#;

/// Lectura del heartbeat para `status`: `last_seen` formateado + edad en
/// segundos (frescura sin parsear fechas en Rust) + `metadata` crudo.
const HEARTBEAT_READ_SQL: &str = r#"
    SELECT
        to_char(last_seen AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS last_seen,
        EXTRACT(EPOCH FROM (NOW() - last_seen))::bigint AS age_secs,
        metadata::text AS metadata
    FROM pilotwave_heartbeat
    WHERE key = $1
    "#;

impl TaskStore for PostgresTaskStore {
    async fn count_due(&self) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(COUNT_DUE_SQL)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .fetch_one(&self.pool)
            .await
    }

    async fn queue_status(&self) -> Result<QueueStatus, sqlx::Error> {
        sqlx::query_as::<_, QueueStatus>(STATUS_SQL)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .fetch_one(&self.pool)
            .await
    }

    async fn list_tasks(&self, query: &ListQuery) -> Result<Vec<ListedTask>, sqlx::Error> {
        sqlx::query_as::<_, ListedTask>(LIST_SQL)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .bind(query.status.as_db_str())
            .bind(query.limit)
            .bind(query.offset)
            .fetch_all(&self.pool)
            .await
    }

    async fn inspect(&self, id: &str) -> Result<Option<InspectedTask>, sqlx::Error> {
        // Sin filtro de cola/backend/task_path: `inspect` es depuración
        // de cualquier fila por id.
        sqlx::query_as::<_, InspectedTask>(INSPECT_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    async fn claim(&self) -> Result<Option<ClaimedTask>, sqlx::Error> {
        sqlx::query_as::<_, ClaimedTask>(CLAIM_SQL)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .bind(&self.worker_id)
            .fetch_optional(&self.pool)
            .await
    }

    async fn claim_by_id(&self, id: &str) -> Result<Option<ClaimedTask>, sqlx::Error> {
        sqlx::query_as::<_, ClaimedTask>(CLAIM_BY_ID_SQL)
            .bind(id)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .bind(&self.worker_id)
            .fetch_optional(&self.pool)
            .await
    }

    async fn finalize_success(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(FINALIZE_SUCCESS_SQL)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn finalize_failed(&self, id: &str, code: &str, detail: &str) -> Result<(), sqlx::Error> {
        // Sanitizar en Rust (no en SQL): `substring` es por caracteres y no
        // redacta nada (contrato §Failure codes).
        let clean = sanitize_detail(detail);
        sqlx::query(FINALIZE_FAILED_SQL)
            .bind(id)
            .bind(code)
            .bind(clean)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn requeue_transient(&self, id: &str, delay_secs: i64) -> Result<(), sqlx::Error> {
        sqlx::query(REQUEUE_SQL)
            .bind(id)
            .bind(delay_secs)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn reap_zombies(&self) -> Result<u64, sqlx::Error> {
        let done = sqlx::query(REAPER_SQL)
            .bind(&self.scope.queue_name)
            .bind(&self.scope.backend_name)
            .bind(&self.scope.task_paths)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    async fn write_heartbeat(&self, metadata_json: &str) -> Result<(), sqlx::Error> {
        sqlx::query(HEARTBEAT_UPSERT_SQL)
            .bind(HEARTBEAT_KEY)
            .bind(metadata_json)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn read_heartbeat(&self) -> Result<Option<Heartbeat>, sqlx::Error> {
        sqlx::query_as::<_, Heartbeat>(HEARTBEAT_READ_SQL)
            .bind(HEARTBEAT_KEY)
            .fetch_optional(&self.pool)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        HEARTBEAT_TABLE, QUEUE_TABLE, RUN_AFTER_SENTINEL, STATUS_FAILED, STATUS_READY,
        STATUS_RUNNING, STATUS_SUCCESSFUL,
    };

    #[test]
    fn secrets_are_redacted_case_insensitively_y_palabras_sueltas_sobreviven() {
        for raw in [
            "smtp failed password=s3cr3t",
            "SMTP failed PASSWORD = s3cr3t",
            "bad dsn:postgres://u:p@h/db",
            "invalid to: \"a@b.c token:abc123\"",
            "key = value",
        ] {
            let clean = sanitize_detail(raw);
            assert!(
                !clean.contains("s3cr3t")
                    && !clean.contains("p@h")
                    && !clean.contains("abc123")
                    && !clean.contains("value"),
                "raw={raw:?} clean={clean:?}"
            );
            assert!(clean.contains("[redacted]"), "raw={raw:?}");
        }
        // Sin clave=valor no hay nada que redactar.
        let clean = sanitize_detail("invalid to mailbox: missing @");
        assert!(clean.contains("mailbox"));
    }

    #[test]
    fn truncation_respects_utf8_boundaries_and_cap() {
        let raw = format!("note password=x {}", "é".repeat(2000));
        let clean = sanitize_detail(&raw);
        assert!(clean.len() <= TRACEBACK_MAX_BYTES);
        assert!(clean.is_char_boundary(clean.len()));
        assert!(clean.contains("[redacted]"));
    }

    /// Ancla el SQL estatico a `contract.rs`: si un literal del contrato
    /// cambia, este test obliga a actualizar el SQL (o viceversa).
    #[test]
    fn sql_usa_literales_del_contrato() {
        for sql in [
            STATUS_SQL,
            LIST_SQL,
            CLAIM_SQL,
            FINALIZE_SUCCESS_SQL,
            CLAIM_BY_ID_SQL,
            COUNT_DUE_SQL,
            FINALIZE_FAILED_SQL,
            REQUEUE_SQL,
            REAPER_SQL,
            INSPECT_SQL,
        ] {
            assert!(
                sql.contains(QUEUE_TABLE),
                "SQL sin tabla de cola {QUEUE_TABLE}: {sql}"
            );
        }
        for sql in [HEARTBEAT_UPSERT_SQL, HEARTBEAT_READ_SQL] {
            assert!(
                sql.contains(HEARTBEAT_TABLE),
                "SQL sin tabla de heartbeat {HEARTBEAT_TABLE}: {sql}"
            );
        }
        for status in [
            STATUS_READY,
            STATUS_RUNNING,
            STATUS_SUCCESSFUL,
            STATUS_FAILED,
        ] {
            let needle = format!("'{status}'");
            assert!(
                [
                    STATUS_SQL,
                    CLAIM_SQL,
                    FINALIZE_SUCCESS_SQL,
                    FINALIZE_FAILED_SQL
                ]
                .iter()
                .any(|sql| sql.contains(&needle)),
                "estado {status} ausente del SQL"
            );
        }
        // Sentinel + claim atomico + orden del contrato en cada query de
        // vencimiento/claim.
        for sql in [STATUS_SQL, CLAIM_SQL, CLAIM_BY_ID_SQL, COUNT_DUE_SQL] {
            assert!(sql.contains(RUN_AFTER_SENTINEL), "SQL sin sentinel: {sql}");
        }
        for sql in [CLAIM_SQL, CLAIM_BY_ID_SQL] {
            assert!(sql.contains("FOR UPDATE SKIP LOCKED"), "sin SKIP LOCKED");
            assert!(
                sql.contains("worker_ids || to_jsonb("),
                "claim sin append JSON de worker_ids"
            );
        }
        assert!(
            CLAIM_SQL.contains("ORDER BY priority DESC, run_after ASC, enqueued_at ASC"),
            "claim sin el orden del contrato"
        );
        // Heartbeat: upsert por clave, `last_seen` lo pone la BD.
        assert!(HEARTBEAT_UPSERT_SQL.contains("ON CONFLICT (key) DO UPDATE"));
        assert!(HEARTBEAT_UPSERT_SQL.contains("NOW()"));
        // Finalize FAILED escribe codigo + detalle sanitizado.
        assert!(FINALIZE_FAILED_SQL.contains("exception_class_path"));
        assert!(FINALIZE_FAILED_SQL.contains("traceback"));
        // Requeue de transitorios: READY + run_after diferido.
        assert!(REQUEUE_SQL.contains("NOW() + ($2 * INTERVAL '1 second')"));
        // Reaper solo toca zombies propios del worker Rust.
        assert!(REAPER_SQL.contains("LIKE 'pilotwave-%'"));
    }
}
