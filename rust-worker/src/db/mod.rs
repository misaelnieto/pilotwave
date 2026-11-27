//! PostgreSQL access.
//!
//! Single connection, autocommit, no TLS (local DB). Connections only;
//! SQL and queries live in `postgres.rs` behind the `store` trait.
//! The minimum-privilege check (roadmap R-6) also lives here: after
//! connecting, the worker fails loud if the role lacks the `.env.example`
//! `GRANT`s.

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

/// Least privilege the worker DSN must hold (see roadmap R-6):
/// `SELECT, UPDATE` on the queue, `SELECT, INSERT, UPDATE` on the heartbeat
/// (see `.env.example`). `SELECT` on the heartbeat is required because
/// `status` and `is-alive` reuse the worker DSN when no reader DSN is
/// configured. Positional order: `missing_grants` maps results
/// to missing pairs.
pub const REQUIRED_GRANTS: &[(&str, &str)] = &[
    (QUEUE_TABLE, "SELECT"),
    (QUEUE_TABLE, "UPDATE"),
    (HEARTBEAT_TABLE, "SELECT"),
    (HEARTBEAT_TABLE, "INSERT"),
    (HEARTBEAT_TABLE, "UPDATE"),
];

/// Positional probe results (`true` = granted) mapped to missing
/// `(table, privilege)` pairs. Pure function, testable without a DB.
pub fn missing_grants(results: &[bool]) -> Vec<(&'static str, &'static str)> {
    REQUIRED_GRANTS
        .iter()
        .zip(results.iter())
        .filter(|(_, granted)| !**granted)
        .map(|((table, priv_), _)| (*table, *priv_))
        .collect()
}

/// Actionable message for a loud failure: what is missing and how to grant it.
pub fn grant_error_message(current_user: &str, missing: &[(&str, &str)]) -> String {
    let grants: Vec<String> = missing
        .iter()
        .map(|(table, priv_)| format!("GRANT {priv_} ON {table} TO {current_user};"))
        .collect();
    format!(
        "least-privilege check failed for role {current_user}: missing {} \
         (worker needs SELECT, UPDATE on {QUEUE_TABLE} and SELECT, INSERT, UPDATE on {HEARTBEAT_TABLE}); \
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

/// A single connection, autocommit, no TLS (local DB).
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

/// Verify least privilege after connecting (see roadmap R-6): probe
/// `has_table_privilege(current_user, table, priv)` for each
/// `REQUIRED_GRANTS` entry and fail loud with the actionable `GRANT`
/// when something is missing. `skip=true` (`PILOTWAVE_SKIP_GRANT_CHECK`)
/// skips it (best-effort for ad-hoc probes); enabled by default.
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
        assert!(missing_grants(&[true, true, true, true, true]).is_empty());
    }

    #[test]
    fn missing_grants_mapea_posicionales_a_pares() {
        let missing = missing_grants(&[true, false, true, false, true]);
        assert_eq!(
            missing,
            vec![(QUEUE_TABLE, "UPDATE"), (HEARTBEAT_TABLE, "INSERT")]
        );
        // The new heartbeat SELECT is positional too: without it the
        // fallback `is-alive`/`status` probe cannot read the heartbeat.
        let missing = missing_grants(&[true, true, false, true, true]);
        assert_eq!(missing, vec![(HEARTBEAT_TABLE, "SELECT")]);
        let missing = missing_grants(&[false, false, false, false, false]);
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
