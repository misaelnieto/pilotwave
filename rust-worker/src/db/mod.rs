//! PostgreSQL access.
//!
//! Port del prototipo (`db/`): exactamente una conexion, autocommit, sin TLS
//! (BD local). Solo conexion aqui; SQL y queries en `postgres.rs` tras el
//! trait `store`. La verificacion de privilegios minimo (R-6) tambien vive
//! aqui: tras conectar, el worker falla alto si el rol no tiene los `GRANT`
//! de `.env.example`.

pub mod models;
pub mod postgres;
pub mod scope;
pub mod store;

pub use postgres::PostgresTaskStore;
pub use postgres::sanitize_detail;
pub use scope::QueueScope;
pub use store::TaskStore;

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::contract::{HEARTBEAT_TABLE, QUEUE_TABLE};

/// Least-privilege que el DSN del worker debe tener (audit 001 §A07, R-6):
/// `SELECT, UPDATE` en la cola, `INSERT, UPDATE` en el heartbeat
/// (ver `.env.example`). Orden posicional: `missing_grants` lo usa para
/// mapear resultados a pares faltantes.
pub const REQUIRED_GRANTS: &[(&str, &str)] = &[
    (QUEUE_TABLE, "SELECT"),
    (QUEUE_TABLE, "UPDATE"),
    (HEARTBEAT_TABLE, "INSERT"),
    (HEARTBEAT_TABLE, "UPDATE"),
];

/// Resultados posicionales del sondeo (`true` = concedido) a pares
/// `(tabla, privilegio)` faltantes. Funcion pura, testeable sin BD.
pub fn missing_grants(results: &[bool]) -> Vec<(&'static str, &'static str)> {
    REQUIRED_GRANTS
        .iter()
        .zip(results.iter())
        .filter(|(_, granted)| !**granted)
        .map(|((table, priv_), _)| (*table, *priv_))
        .collect()
}

/// Mensaje accionable para fallo alto: que falta y como concederlo.
pub fn grant_error_message(current_user: &str, missing: &[(&str, &str)]) -> String {
    let grants: Vec<String> = missing
        .iter()
        .map(|(table, priv_)| format!("GRANT {priv_} ON {table} TO {current_user};"))
        .collect();
    format!(
        "least-privilege check failed for role {current_user}: missing {} \
         (worker needs SELECT, UPDATE on {QUEUE_TABLE} and INSERT, UPDATE on {HEARTBEAT_TABLE}); \
         ask the DBA to run: {}. \
         Bypass only for ad-hoc checks with PILOTWAVE_SKIP_GRANT_CHECK=1.",
        missing
            .iter()
            .map(|(t, p)| format!("{p} on {t}"))
            .collect::<Vec<_>>()
            .join(", "),
        grants.join(" "),
    )
}

/// Una sola conexion, autocommit, sin TLS (BD local).
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

/// Verifica least-privilege tras conectar (audit 001 §A07, R-6): sondea
/// `has_table_privilege(current_user, tabla, priv)` por cada entrada de
/// `REQUIRED_GRANTS` y falla alto con el `GRANT` accionable si falta algo.
/// `skip=true` (`PILOTWAVE_SKIP_GRANT_CHECK`) lo omite (best-effort para
/// sondas ad-hoc); por defecto esta activado.
pub async fn verify_least_privilege(pool: &PgPool, skip: bool) -> Result<(), String> {
    if skip {
        return Ok(());
    }
    let user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(pool)
        .await
        .map_err(|err| format!("grant check: no se pudo leer current_user: {err}"))?;
    let mut results = Vec::with_capacity(REQUIRED_GRANTS.len());
    for (table, priv_) in REQUIRED_GRANTS {
        let has: bool = sqlx::query_scalar("SELECT has_table_privilege(current_user, $1, $2)")
            .bind(*table)
            .bind(*priv_)
            .fetch_one(pool)
            .await
            .map_err(|err| format!("grant check: fallo al sondear {priv_} en {table}: {err}"))?;
        results.push(has);
    }
    let missing = missing_grants(&results);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(grant_error_message(&user, &missing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_concedido_no_falta_nada() {
        assert!(missing_grants(&[true, true, true, true]).is_empty());
    }

    #[test]
    fn missing_grants_mapea_posicionales_a_pares() {
        let missing = missing_grants(&[true, false, true, false]);
        assert_eq!(
            missing,
            vec![(QUEUE_TABLE, "UPDATE"), (HEARTBEAT_TABLE, "UPDATE")]
        );
        let missing = missing_grants(&[false, false, false, false]);
        assert_eq!(missing.len(), REQUIRED_GRANTS.len());
    }

    #[test]
    fn grant_error_message_es_accionable() {
        let msg = grant_error_message("pilotwave_worker", &[(QUEUE_TABLE, "SELECT")]);
        assert!(msg.contains("pilotwave_worker"));
        assert!(msg.contains("SELECT on"));
        assert!(msg.contains(
            "GRANT SELECT ON django_tasks_database_dbtaskresult TO pilotwave_worker;",
        ));
        assert!(msg.contains("PILOTWAVE_SKIP_GRANT_CHECK"));
    }
}
