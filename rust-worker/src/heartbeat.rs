//! Escritura del `PilotwaveHeartbeat` del sidecar (PLAN-003 §3.10).
//!
//! La fila `pilotwave_heartbeat(key='pilotwave_worker')` es la **fuente
//! canónica** de vida: Django (`get_pilotwave_status`) solo lee esta
//! fila (§3.10.4, prohibido `ps`/pidfile/`/proc` en request-path) y `status`
//! la muestra cuando hay BD, degradando a la sonda local si no la hay.
//!
//! Ritmo: cada ciclo de poll **y** antes/después de cada envío (§3.10: con
//! una sola conexión y un loop secuencial, un send de 30 s envejecería el
//! heartbeat más allá del umbral honesto si solo se escribe por ciclo).
//! El contrato `metadata` es solo-contadores + identidad operativa del
//! proceso (pid/uptime/rss): nunca recipients, subjects, cuerpos,
//! `form_data`, textos de error, DSNs ni rutas de adjuntos.

/// Clave de la fila: reutiliza el contrato (fuente unica).
pub use crate::contract::HEARTBEAT_KEY;

/// Versión del esquema `metadata` (arranca en 1, como el payload §3.4).
pub const HEARTBEAT_VERSION: u32 = 1;

/// Umbral stale: `TASK_TIMEOUT 30` + `POLL_MAX 5` + margen. Espejo del
/// `PILOTWAVE_STALE_AFTER_SECS = 90` de Django (la regla genérica de
/// 10 minutos NO aplica a esta clave).
pub const STALE_AFTER_SECS: i64 = 90;

/// `fresh` si `age_secs` está dentro del umbral, `stale` si no.
pub fn freshness(age_secs: i64) -> &'static str {
    if age_secs <= STALE_AFTER_SECS {
        "fresh"
    } else {
        "stale"
    }
}

/// Contadores e identidad operativa para una escritura de heartbeat.
/// Todo sale de la BD (conteos) o de `/proc` propio (best-effort);
/// ningún campo porta PII.
pub struct BeatCounters {
    /// `COUNT READY` reclamable visto en este ciclo (§3.10.1 `due_depth`).
    pub queue_depth_seen: i64,
    /// Tareas procesadas desde el arranque del proceso.
    pub tasks_processed: u64,
    /// UUID de la última tarea tocada (solo el id, nunca el contenido).
    pub last_task_id: Option<String>,
    /// Último `pilotwave_worker.*` (código, nunca el detalle con texto atacante).
    pub last_error_code: Option<String>,
    /// PID propio (cruce con la sonda local de `status`).
    pub pid: u32,
    /// Best-effort (`None` => `null`, el lector pinta `n/a`).
    pub uptime_secs: Option<u64>,
    /// Best-effort (`None` => `null`).
    pub rss_kb: Option<u64>,
}

/// Serializa `metadata` como JSON (claves ordenadas por construcción).
/// Función pura para poder probarla sin BD.
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
        // Ausentes => null sin pánico (best-effort de /proc).
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
