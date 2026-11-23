//! Lógica del worker: reclamación, procesado y loops (§3.4, §3.6, §3.7, §3.11).
//!
//! `main.rs` queda como bootstrap fino (parse, config, despacho sin BD) y
//! delega aquí para oneshot (`status`/`inspect`/`list`/`run` único) y loop
//! (`run --fg` / `run --bg-child`). Sin cambiar mensajes, exit codes ni
//! stdout/stderr.

use std::time::Duration;

use crate::attachments;
use crate::cli::{self, DaemonInfo};
use crate::config::Config;
use crate::contract;
use crate::daemon;
use crate::db::models::{ClaimedTask, ListQuery};
use crate::db::{self, PostgresTaskStore, TaskStore};
use crate::heartbeat;
use crate::logging;
use crate::mailer;
use crate::retry;

/// Resultado unificado del worker (errores ya impresos por el caller).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Error `InvalidInput` boxeado (inspect/claim con id inválido, rutas del
/// daemon sin configurar). Centraliza los 3-4 `io::Error(InvalidInput)`.
pub fn invalid_input(msg: String) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
        as Box<dyn std::error::Error>
}

/// Edad máxima del daemon antes del autoreciclaje (§3.8): 7 días.
/// Se combina con `PILOTWAVE_RECYCLE_N` (por tareas); el que llegue primero gana.
const RECYCLE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cómo terminó el loop persistente (§3.9): apagado limpio (exit 0) o
/// autoreciclaje (exit 75, el keep-alive reinicia sin alerta).
pub enum LoopEnd {
    Shutdown,
    Recycle,
}

/// Lo que un ciclo aporta al heartbeat (§3.10): id de la tarea tocada,
/// su código de fallo (`None` si éxito o reencolado limpio... en la
/// práctica el transitorio también deja código, ver `handle_claimed`) y
/// la profundidad vista (para `queue_depth_seen`).
pub(crate) struct CycleBeat {
    pub task_id: String,
    pub error_code: Option<String>,
    pub queue_depth_seen: i64,
}

/// Autoreciclaje por número de tareas (§3.8). Función pura, testeable.
pub(crate) fn should_recycle_tasks(processed: u64, recycle_n: u64) -> bool {
    processed >= recycle_n
}

/// Autoreciclaje por antigüedad (§3.8, 7 días). Función pura, testeable.
pub(crate) fn should_recycle_age(uptime: Duration, max_age: Duration) -> bool {
    uptime >= max_age
}

/// Delay idle entre polls (§3.11): `poll_min` hasta 20 vacíos seguidos,
/// luego `poll_max`. Función pura para poder probarla sin timers.
pub(crate) fn idle_delay(empty_streak: u32, poll_min: Duration, poll_max: Duration) -> Duration {
    if empty_streak >= 20 {
        poll_max
    } else {
        poll_min
    }
}

/// Backoff de reconexión a BD (solo `run --fg`).
///
/// `failures` es el contador de fallos consecutivos (1 = primer fallo).
/// Secuencia: 1s → 2s → 4s → 8s → 16s → 30s (cap). Función pura para
/// poder probarla sin timers ni BD.
pub(crate) fn db_reconnect_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    Duration::from_secs((1u64 << shift).min(30))
}

/// Espera a SIGTERM o SIGINT (§3.9). En Unix escucha ambas;
/// fuera de Unix solo Ctrl-C.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(sig) => sig,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Intento único de conexión, con reintento infinito + backoff.
///
/// Solo para el loop persistente (`run --fg`): los one-shot (`status`,
/// `list`, `inspect`, `run` sin `--fg`) usan `db::connect` directo y
/// fallan alto. Devuelve `None` si llega SIGTERM/SIGINT durante la espera:
/// el caller sale con `Ok(())` (exit 0, §3.9).
pub(crate) async fn connect_with_backoff(database_url: &str) -> Option<sqlx::postgres::PgPool> {
    let mut failures: u32 = 0;
    loop {
        match db::connect(database_url).await {
            Ok(pool) => return Some(pool),
            Err(err) => {
                failures = failures.saturating_add(1);
                let delay = db_reconnect_delay(failures);
                logging::warn(&format!(
                    "postgres no disponible (intento {failures}): {err}; reintentando en {}s",
                    delay.as_secs()
                ));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown_signal() => {
                        logging::info("shutdown durante reconexión a BD, saliendo");
                        return None;
                    }
                }
            }
        }
    }
}

/// Worker oneshot/loop: `config` para alcance y SMTP, `store` para la BD.
pub struct Worker {
    config: Config,
    store: PostgresTaskStore,
}

impl Worker {
    /// Crea el worker sobre un pool ya conectado (el caller decide la
    /// política de conexión: fallo alto en oneshot, backoff en loop).
    pub fn new(config: Config, store: PostgresTaskStore) -> Self {
        Self { config, store }
    }

    // El pool de sqlx se cierra por drop al salir (sin close explícito:
    // ningún camino de salida puede saltárselo en silencio).
    /// `status`: tabla de conteos + sección daemon ya resuelta.
    ///
    /// La fila heartbeat es la fuente canónica de vida (§3.10): se lee de
    /// la BD y se muestra tras el daemon. Sin fila => `never_run` (el
    /// sidecar aún no escribió); la sonda local sigue visible arriba.
    pub async fn run_status(&self, daemon: Option<&DaemonInfo>) -> Result<()> {
        let status = self.store.queue_status().await?;
        cli::print_status_table(&status);
        if let Some(info) = daemon {
            println!("{}", cli::render_daemon_status(info));
        }
        match self.store.read_heartbeat().await? {
            Some(hb) => println!(
                "{}",
                cli::render_heartbeat_status(hb.age_secs, &hb.last_seen, &hb.metadata)
            ),
            None => println!("heartbeat: never_run (el sidecar aún no escribió)"),
        }
        Ok(())
    }

    /// `inspect <UUID>`: todos los campos de una fila por id (sin filtro).
    /// Por defecto recorta la PII; con `show_pii` (`--show-pii`, solo
    /// operadores) imprime destinatario, cuerpos y `traceback` completos.
    pub async fn run_inspect(&self, id: &str, show_pii: bool) -> Result<()> {
        match self.store.inspect(id).await {
            Err(err) => {
                // Un id que no es UUID revienta el `$1::uuid` en Postgres:
                // mensaje limpio en vez del error crudo de la BD.
                let msg = cli::format_inspect_error(id, &err.to_string());
                Err(invalid_input(msg))
            }
            Ok(None) => {
                println!("no existe ninguna tarea con id {id}");
                Ok(())
            }
            Ok(Some(task)) => {
                cli::print_inspected_task(&task, show_pii);
                Ok(())
            }
        }
    }

    /// `list <estado>`: tabla de tareas con `--limit`/`--offset`.
    pub async fn run_list(&self, query: &ListQuery) -> Result<()> {
        let tasks = self.store.list_tasks(query).await?;
        cli::print_task_list(&tasks);
        Ok(())
    }

    /// `run` único: reclamo dirigido (`--id`) o genérico, sin loop.
    pub async fn run_once(&self, id: Option<&str>) -> Result<()> {
        // Solo metadatos de alcance: nada de secretos ni cuerpos.
        logging::debug(&format!(
            "alcance claim queue={} backend={} tasks={} worker_id={}",
            self.config.queue_name,
            self.config.backend_name,
            self.config.task_paths.join(","),
            self.config.worker_id
        ));
        let Some(id) = id else {
            self.drain_once().await?;
            return Ok(());
        };
        // Reclamo dirigido: solo si es ejecutable (READY + vencida +
        // alcance). Mismo pipeline que el genérico, sin loop.
        match self.store.claim_by_id(id).await {
            Err(err) => {
                let msg = cli::format_inspect_error(id, &err.to_string());
                Err(invalid_input(msg))
            }
            Ok(Some(task)) => {
                self.handle_claimed(&task).await?;
                Ok(())
            }
            Ok(None) => match self.store.inspect(id).await {
                Err(err) => {
                    let msg = cli::format_inspect_error(id, &err.to_string());
                    Err(invalid_input(msg))
                }
                Ok(None) => {
                    logging::info(&format!("no existe ninguna tarea con id {id}"));
                    Ok(())
                }
                Ok(Some(row)) => {
                    logging::info(&format!(
                        "tarea {id} no ejecutable: status={} queue={} backend={} task_path={}",
                        row.status, row.queue_name, row.backend_name, row.task_path
                    ));
                    Ok(())
                }
            },
        }
    }

    /// Procesa una tarea ya reclamada: parse → policy disco → SMTP → finalize.
    /// Devuelve `Ok(Some(código))` si la tarea se finalizó `FAILED`,
    /// `Ok(None)` si éxito o reencolado transitorio; el `Err` solo es de
    /// BD. El código alimenta `last_error_code` del heartbeat (§3.10).
    pub async fn handle_claimed(&self, task: &ClaimedTask) -> Result<Option<String>> {
        let store = &self.store;
        let config = &self.config;
        logging::info(&format!(
            "claimed id={} args_bytes={} attempts={}",
            task.id,
            task.args_kwargs.len(),
            task.attempts
        ));
        let content = match contract::parse_args_kwargs(&task.args_kwargs) {
            Err(err) => {
                // `detail` puede portar texto atacante: se muestra
                // sanitizado (igual que se persiste, §3.2).
                logging::debug(&format!(
                    "payload rechazado id={} code={} detail={}",
                    task.id,
                    err.code,
                    db::sanitize_detail(&err.detail)
                ));
                store
                    .finalize_failed(&task.id, err.code, &err.detail)
                    .await?;
                logging::debug(&format!("finalizado id={} FAILED {}", task.id, err.code));
                logging::info(&format!("finalized id={} FAILED {}", task.id, err.code));
                return Ok(Some(err.code.to_owned()));
            }
            Ok(content) => content,
        };
        // Policy en disco (tamaños reales, symlinks,
        // roots) antes de ensamblar nada (§3.4 paso B).
        let files = match attachments::load(&config.attachment_roots, &content.attachment_paths) {
            Err(err) => {
                logging::debug(&format!(
                    "adjunto rechazado id={} code={} detail={}",
                    task.id,
                    err.code(),
                    db::sanitize_detail(err.detail())
                ));
                store
                    .finalize_failed(&task.id, err.code(), &db::sanitize_detail(err.detail()))
                    .await?;
                logging::info(&format!("finalized id={} FAILED {}", task.id, err.code()));
                return Ok(Some(err.code().to_owned()));
            }
            Ok(files) => files,
        };
        // Solo direcciones y presencia de cuerpos, nunca su contenido.
        logging::debug(&format!(
            "payload ok id={} to={} subject={:?} text={} html={} attachments={}",
            task.id,
            content.to,
            content.subject,
            content.text_body.is_some(),
            content.html_body.is_some(),
            files.len()
        ));
        let kind = match (&content.text_body, &content.html_body) {
            (Some(_), Some(_)) => "alternative",
            (Some(_), None) => "plain",
            (None, Some(_)) => "html",
            (None, None) => "sin-cuerpo",
        };
        logging::debug(&format!(
            "mensaje id={} kind={kind} to={}",
            task.id, content.to
        ));
        // Sin `smtp_password`: solo parámetros de conexión.
        logging::debug(&format!(
            "smtp host={} port={} tls={} user={}",
            config.smtp_host, config.smtp_port, config.smtp_use_tls, config.smtp_user
        ));
        let started = std::time::Instant::now();
        match mailer::send(config, &content, &files).await {
            Ok(()) => {
                logging::debug(&format!(
                    "envío id={} ok latency_ms={}",
                    task.id,
                    started.elapsed().as_millis()
                ));
                store.finalize_success(&task.id).await?;
                logging::debug(&format!("finalizado id={} SUCCESSFUL", task.id));
                logging::info(&format!(
                    "finalized id={} SUCCESSFUL latency_ms={}",
                    task.id,
                    started.elapsed().as_millis()
                ));
            }
            Err(err) => {
                logging::debug(&format!(
                    "envío id={} error={} detail={} latency_ms={}",
                    task.id,
                    err.code(),
                    db::sanitize_detail(err.detail()),
                    started.elapsed().as_millis()
                ));
                match err {
                    mailer::SendError::Transient(detail) => {
                        if (task.attempts as usize) >= config.max_attempts {
                            store
                                .finalize_failed(
                                    &task.id,
                                    "pilotwave_worker.smtp_exhausted",
                                    &detail,
                                )
                                .await?;
                            logging::info(&format!(
                                "finalized id={} FAILED pilotwave_worker.smtp_exhausted attempts={}",
                                task.id, task.attempts
                            ));
                            return Ok(Some("pilotwave_worker.smtp_exhausted".to_owned()));
                        } else {
                            let delay = retry::backoff_for_attempt(task.attempts)
                                .unwrap_or_else(|| std::time::Duration::from_secs(1_800));
                            let secs = delay.as_secs() as i64;
                            store.requeue_transient(&task.id, secs).await?;
                            logging::info(&format!(
                                "requeued id={} attempts={} run_after_in={secs}s",
                                task.id, task.attempts
                            ));
                            return Ok(Some("pilotwave_worker.smtp_transient".to_owned()));
                        }
                    }
                    other => {
                        let code = other.code();
                        let detail = db::sanitize_detail(other.detail());
                        store.finalize_failed(&task.id, code, &detail).await?;
                        logging::info(&format!("finalized id={} FAILED {code}", task.id));
                        return Ok(Some(code.to_owned()));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Una iteración: reap zombies (§3.7) + un claim genérico.
    /// Devuelve el aporte al heartbeat si hubo trabajo (`None` si la cola
    /// estaba vacía). Los errores de BD siguen siendo `Err`.
    pub async fn drain_once(&self) -> Result<Option<CycleBeat>> {
        let due = self.store.count_due().await?;
        let reaped = self.store.reap_zombies().await?;
        if reaped > 0 {
            logging::info(&format!("reaped {reaped} zombie tasks"));
        }
        logging::info(&format!("claimable tasks: {due}"));
        match self.store.claim().await? {
            Some(task) => {
                let id = task.id.clone();
                let error_code = self.handle_claimed(&task).await?;
                Ok(Some(CycleBeat {
                    task_id: id,
                    error_code,
                    queue_depth_seen: due,
                }))
            }
            None => {
                logging::info("no claimable tasks");
                Ok(None)
            }
        }
    }

    /// Escribe el heartbeat best-effort (§3.10): un fallo aquí no aborta
    /// el loop (la frescura la decide el lector con el umbral de 90 s).
    async fn beat(
        &self,
        due_seen: i64,
        processed: u64,
        last_task_id: &Option<String>,
        last_error_code: &Option<String>,
    ) {
        let pid = std::process::id();
        let meta = heartbeat::render_metadata(&heartbeat::BeatCounters {
            queue_depth_seen: due_seen,
            tasks_processed: processed,
            last_task_id: last_task_id.clone(),
            last_error_code: last_error_code.clone(),
            pid,
            uptime_secs: crate::proc::uptime_secs(pid),
            rss_kb: crate::proc::read_vmrss_kb(pid),
        });
        if let Err(err) = self.store.write_heartbeat(&meta).await {
            logging::warn(&format!("heartbeat no escrito: {err}"));
        }
    }

    /// Loop persistente §3.11 + shutdown graceful §3.9 + reconexión §3.6:
    /// - El hijo daemon toma el lock ANTES de tocar la BD (fail loud → exit
    ///   1); el guard vive hasta el final y suelta el lock al morir.
    /// - Conexión inicial con backoff infinito (si la BD está caída al
    ///   arrancar, no salimos con exit 1: esperamos a que vuelva).
    /// - Cada iteración que falle por BD no aborta: log WARN, backoff
    ///   1s→30s, y seguir. El pool de sqlx reconecta solo en el próximo
    ///   `acquire`; nosotros solo evitamos el hot-loop contra una BD caída.
    /// - El sleep (idle o backoff) siempre compite con `shutdown_signal`:
    ///   SIGTERM/SIGINT sale con exit 0 aunque estemos esperando a la BD.
    /// - Autoreciclaje (§3.8): tras `PILOTWAVE_RECYCLE_N` tareas o 7 días,
    ///   devuelve `LoopEnd::Recycle` → exit 75 para que el keep-alive
    ///   reinicie y mallocng (musl) recupere la memoria fragmentada.
    /// - Heartbeat (§3.10): beat antes del drain (pre-envío) y después
    ///   (contadores actualizados), más uno final antes de reciclar.
    pub async fn run_loop(config: Config, is_bg_child: bool) -> Result<LoopEnd> {
        let _pid_guard = if is_bg_child {
            let (pid_path, _) = daemon::paths(&config).map_err(invalid_input)?;
            Some(daemon::acquire(&pid_path).map_err(|err| {
                let kind = match err {
                    daemon::AcquireError::AlreadyRunning(_) => std::io::ErrorKind::AlreadyExists,
                    daemon::AcquireError::Io(_) => std::io::ErrorKind::Other,
                };
                std::io::Error::new(kind, err.to_string())
            })?)
        } else {
            None
        };
        let Some(pool) = connect_with_backoff(&config.database_url).await else {
            // Shutdown durante la reconexión: salida limpia, exit 0 (§3.9).
            return Ok(LoopEnd::Shutdown);
        };
        logging::info("postgres connected (max_connections=1)");
        // R-6 (audit 001 §A07): fail loud si el rol no tiene los GRANT
        // minimos. Es error de config, no de red: sin backoff, exit 1.
        if let Err(err) = db::verify_least_privilege(&pool, config.skip_grant_check).await {
            logging::error(&err);
            return Err(invalid_input(err));
        }
        let worker_id = config.worker_id.clone();
        let scope = db::QueueScope::from(&config);
        let worker = Self::new(config, PostgresTaskStore::new(pool, scope, worker_id));
        logging::debug(&format!(
            "alcance claim queue={} backend={} tasks={} worker_id={}",
            worker.config.queue_name,
            worker.config.backend_name,
            worker.config.task_paths.join(","),
            worker.config.worker_id
        ));
        logging::info(&format!(
            "loop {} poll_min={}ms poll_max={}ms",
            if is_bg_child { "bg-child" } else { "fg" },
            worker.config.poll_min.as_millis(),
            worker.config.poll_max.as_millis()
        ));
        let mut empty: u32 = 0;
        let mut db_failures: u32 = 0;
        let mut processed: u64 = 0;
        let mut last_task_id: Option<String> = None;
        let mut last_error_code: Option<String> = None;
        // `due_seen` alimenta `queue_depth_seen`: el beat previo al primer
        // drain usa 0 (aún no se ha visto la cola).
        let mut due_seen: i64 = 0;
        let started = std::time::Instant::now();
        loop {
            // §3.8: reciclar antes de reclamar la siguiente tarea (nunca a
            // mitad de un envío). El beat final deja la fila fresca para
            // que el keep-alive distinga el reciclaje de una muerte.
            if should_recycle_tasks(processed, worker.config.recycle_n)
                || should_recycle_age(started.elapsed(), RECYCLE_MAX_AGE)
            {
                logging::info(&format!(
                    "self-recycle: processed={processed} uptime_secs={}",
                    started.elapsed().as_secs()
                ));
                worker
                    .beat(due_seen, processed, &last_task_id, &last_error_code)
                    .await;
                return Ok(LoopEnd::Recycle);
            }
            // Beat previo al drain (§3.10: antes de cada envío; con un send
            // de 30 s este es el que mantiene la fila fresca).
            worker
                .beat(due_seen, processed, &last_task_id, &last_error_code)
                .await;
            match worker.drain_once().await {
                Ok(beat) => {
                    db_failures = 0;
                    let did_work = beat.is_some();
                    if let Some(beat) = beat {
                        processed = processed.saturating_add(1);
                        last_task_id = Some(beat.task_id);
                        last_error_code = beat.error_code;
                        due_seen = beat.queue_depth_seen;
                    }
                    // Beat posterior al drain (§3.10: después de cada envío,
                    // con los contadores ya actualizados).
                    worker
                        .beat(due_seen, processed, &last_task_id, &last_error_code)
                        .await;
                    empty = if did_work { 0 } else { empty.saturating_add(1) };
                    let delay = idle_delay(empty, worker.config.poll_min, worker.config.poll_max);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_signal() => {
                            logging::info("shutdown solicitado, saliendo");
                            break;
                        }
                    }
                }
                Err(err) => {
                    db_failures = db_failures.saturating_add(1);
                    let delay = db_reconnect_delay(db_failures);
                    logging::warn(&format!(
                        "db error (fallo consecutivo {db_failures}): {err}; reintentando en {}s",
                        delay.as_secs()
                    ));
                    // No tocamos `empty`: es racha de polls vacíos, no de fallos.
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_signal() => {
                            logging::info("shutdown solicitado, saliendo");
                            break;
                        }
                    }
                }
            }
        }
        // El pool cae por drop aquí (cierre sin camino silencioso).
        Ok(LoopEnd::Shutdown)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_delay_usa_min_hasta_20_vacios_y_luego_max() {
        let min = Duration::from_millis(1_500);
        let max = Duration::from_millis(5_000);
        assert_eq!(idle_delay(0, min, max), min);
        assert_eq!(idle_delay(19, min, max), min);
        assert_eq!(idle_delay(20, min, max), max);
        assert_eq!(idle_delay(1_000, min, max), max);
        assert_eq!(idle_delay(u32::MAX, min, max), max);
    }

    #[test]
    fn db_reconnect_backoff_dobla_hasta_30s() {
        assert_eq!(db_reconnect_delay(1), Duration::from_secs(1));
        assert_eq!(db_reconnect_delay(2), Duration::from_secs(2));
        assert_eq!(db_reconnect_delay(3), Duration::from_secs(4));
        assert_eq!(db_reconnect_delay(4), Duration::from_secs(8));
        assert_eq!(db_reconnect_delay(5), Duration::from_secs(16));
        assert_eq!(db_reconnect_delay(6), Duration::from_secs(30));
        assert_eq!(db_reconnect_delay(7), Duration::from_secs(30));
        assert_eq!(db_reconnect_delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn recycle_dispara_por_tareas_y_por_edad() {
        assert!(!should_recycle_tasks(0, 5_000));
        assert!(!should_recycle_tasks(4_999, 5_000));
        assert!(should_recycle_tasks(5_000, 5_000));
        assert!(should_recycle_tasks(5_001, 5_000));
        // Config inválida (recycle_n=0) nunca llega aquí (config lo rechaza),
        // pero el helper no debe pedir más de lo que el contador ya tiene.
        assert!(should_recycle_tasks(0, 0));
        // Antigüedad: 7 días.
        assert_eq!(RECYCLE_MAX_AGE, Duration::from_secs(7 * 24 * 60 * 60));
        assert!(!should_recycle_age(
            RECYCLE_MAX_AGE - Duration::from_secs(1),
            RECYCLE_MAX_AGE
        ));
        assert!(should_recycle_age(RECYCLE_MAX_AGE, RECYCLE_MAX_AGE));
        assert!(should_recycle_age(
            RECYCLE_MAX_AGE + Duration::from_secs(1),
            RECYCLE_MAX_AGE
        ));
    }
}
