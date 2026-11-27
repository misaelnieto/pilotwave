//! Worker logic: claim, processing and loops.
//!
//! `main.rs` stays a thin bootstrap (parse, config, dispatch without DB)
//! and delegates here for oneshots (`status`/`is-alive`/`inspect`/`list`/single `run`)
//! and loops (`run --fg` / `run --bg-child`). Messages, exit codes and
//! stdout/stderr stay unchanged.

use std::time::Duration;

use std::process::ExitCode;

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

/// Unified worker result (errors already printed by the caller).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Boxed `InvalidInput` error (inspect/claim with invalid id, unconfigured
/// daemon paths). Centralizes the `io::Error(InvalidInput)` sites.
pub fn invalid_input(msg: String) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
        as Box<dyn std::error::Error>
}

/// Maximum daemon age before self-recycling: 7 days.
/// Combined with `PILOTWAVE_RECYCLE_N` (by tasks); whichever hits first wins.
const RECYCLE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Bounded wait for the `is-alive` heartbeat `SELECT`: postgres may accept
/// the connection and then stall the query/response, and `db::connect` only
/// bounds acquisition. Without this the cron probe hangs without its
/// promised one-line result.
const IS_ALIVE_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);

/// How the persistent loop ended: clean shutdown (exit 0) or
/// self-recycle (exit 75, the keep-alive restarts without alerting).
pub enum LoopEnd {
    Shutdown,
    Recycle,
}

/// What one cycle contributes to the heartbeat: id of the touched task,
/// its failure code (`None` on success or clean requeue -- in practice a
/// transient also leaves a code, see `handle_claimed`) and the observed
/// depth (for `queue_depth_seen`).
pub(crate) struct CycleBeat {
    pub task_id: String,
    pub error_code: Option<String>,
    pub queue_depth_seen: i64,
}

/// Self-recycling by task count. Pure function, testable.
pub(crate) fn should_recycle_tasks(processed: u64, recycle_n: u64) -> bool {
    processed >= recycle_n
}

/// Self-recycling by age (7 days). Pure function, testable.
pub(crate) fn should_recycle_age(uptime: Duration, max_age: Duration) -> bool {
    uptime >= max_age
}

/// Idle delay between polls: `poll_min` until 20 consecutive empty polls,
/// then `poll_max`. Pure function for timer-free testing.
pub(crate) fn idle_delay(empty_streak: u32, poll_min: Duration, poll_max: Duration) -> Duration {
    if empty_streak >= 20 {
        poll_max
    } else {
        poll_min
    }
}

/// DB reconnect backoff (`run --fg` only).
///
/// `failures` counts consecutive failures (1 = first failure).
/// Sequence: 1s -> 2s -> 4s -> 8s -> 16s -> 30s (cap). Pure function for
/// timer- and DB-free testing.
pub(crate) fn db_reconnect_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    Duration::from_secs((1u64 << shift).min(30))
}

/// Wait for SIGTERM or SIGINT. On Unix listen to both;
/// off Unix, Ctrl-C only.
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

/// Single connection attempt, with infinite retry + backoff.
///
/// Only for the persistent loop (`run --fg`): one-shots (`status`,
/// `is-alive`, `list`, `inspect`, `run` without `--fg`) use `db::connect` directly and
/// fail loud. Returns `None` when SIGTERM/SIGINT arrives while waiting:
/// the caller exits with `Ok(())` (exit 0).
pub(crate) async fn connect_with_backoff(database_url: &str) -> Option<sqlx::postgres::PgPool> {
    let mut failures: u32 = 0;
    loop {
        match db::connect(database_url).await {
            Ok(pool) => return Some(pool),
            Err(err) => {
                failures = failures.saturating_add(1);
                let delay = db_reconnect_delay(failures);
                logging::warn(&format!(
                    "postgres unavailable (attempt {failures}): {err}; retrying in {}s",
                    delay.as_secs()
                ));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown_signal() => {
                        logging::info("shutdown during DB reconnect, exiting");
                        return None;
                    }
                }
            }
        }
    }
}

/// Worker oneshot/loop: `config` for scope and SMTP, `store` for the DB.
pub struct Worker {
    config: Config,
    store: PostgresTaskStore,
}

impl Worker {
    /// Build the worker on an already-connected pool (the caller picks the
    /// connection policy: fail loud in oneshots, backoff in loops).
    pub fn new(config: Config, store: PostgresTaskStore) -> Self {
        Self { config, store }
    }

    // The sqlx pool closes by drop on exit (no explicit close: no exit
    // path can silently skip it).
    /// `status`: counts table + already-resolved daemon section.
    ///
    /// The heartbeat row is the canonical liveness source: read from the
    /// DB and shown after the daemon. No row => `never_run` (the sidecar
    /// has not written yet); the local probe stays visible above.
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
            None => println!("heartbeat: never_run (the sidecar has not written yet)"),
        }
        Ok(())
    }

    /// `is-alive`: one machine-readable liveness line on stdout (ADR-004).
    ///
    /// Read-only: a single heartbeat `SELECT` through the reader DSN (the
    /// caller picks the pool). Never writes, never signals, never starts
    /// anything. Always returns `Ok` with the explicit code: 0 fresh,
    /// 1 stale/down (restartable), 2 error (alert, never blind-restart) --
    /// `main()` maps `Err` to 1, so no `?` may escape from here.
    ///
    /// Local liveness requires the local daemon: a fresh shared heartbeat
    /// (possibly from another host, or left by a recently stopped local
    /// daemon) must not report the local daemon as alive. `daemon` is the
    /// already-resolved local probe (`main.rs` resolves it before touching
    /// the DB): `NotRunning` => `down` (exit 1, cron restarts),
    /// `NotConfigured` => `error` (exit 2, needs human), only `Running`
    /// proceeds to the heartbeat age check. `None` keeps the legacy
    /// heartbeat-only check (callers without a local probe).
    pub async fn run_is_alive(
        &self,
        daemon: Option<&DaemonInfo>,
        max_age_secs: u64,
    ) -> Result<ExitCode> {
        let Ok(max_age) = cli::validate_max_age_secs(max_age_secs) else {
            println!("{}", cli::render_is_alive_error("invalid_max_age_secs"));
            return Ok(ExitCode::from(2));
        };
        match daemon {
            Some(DaemonInfo::Running { .. }) | None => {}
            Some(DaemonInfo::NotRunning) => {
                println!("{}", cli::render_is_alive_down("daemon_not_running"));
                return Ok(ExitCode::from(1));
            }
            Some(DaemonInfo::NotConfigured) => {
                println!("{}", cli::render_is_alive_error("daemon_not_configured"));
                return Ok(ExitCode::from(2));
            }
        }
        let heartbeat = match tokio::time::timeout(
            IS_ALIVE_HEARTBEAT_TIMEOUT,
            self.store.read_heartbeat(),
        )
        .await
        {
            Err(_) => {
                // stderr, never stdout: the probe line stays machine-readable.
                eprintln!(
                    "WARNING pilotwave-worker: is-alive: heartbeat timed out after {}s",
                    IS_ALIVE_HEARTBEAT_TIMEOUT.as_secs()
                );
                println!("{}", cli::render_is_alive_error("heartbeat_read_timeout"));
                return Ok(ExitCode::from(2));
            }
            Ok(Err(err)) => {
                // stderr, never stdout: the probe line stays machine-readable.
                eprintln!("WARNING pilotwave-worker: is-alive: heartbeat unreadable: {err}");
                println!("{}", cli::render_is_alive_error("heartbeat_read_failed"));
                return Ok(ExitCode::from(2));
            }
            Ok(Ok(heartbeat)) => heartbeat,
        };
        let Some(hb) = heartbeat else {
            println!("{}", cli::render_is_alive_down("no_heartbeat"));
            return Ok(ExitCode::from(1));
        };
        let pid = cli::is_alive_pid(&hb.metadata);
        if hb.age_secs < 0 || (hb.age_secs as u64) <= max_age {
            println!("{}", cli::render_is_alive_fresh(hb.age_secs, &pid));
            Ok(ExitCode::SUCCESS)
        } else {
            println!("{}", cli::render_is_alive_stale(hb.age_secs, &pid));
            Ok(ExitCode::from(1))
        }
    }

    /// `inspect <UUID>`: every field of one row by id (unfiltered).
    /// By default PII is trimmed; with `show_pii` (`--show-pii`, operators
    /// only) print recipient, bodies and full `traceback`.
    pub async fn run_inspect(&self, id: &str, show_pii: bool) -> Result<()> {
        match self.store.inspect(id).await {
            Err(err) => {
                // A non-UUID id blows up the `$1::uuid` cast in Postgres:
                // clean message instead of the raw DB error.
                let msg = cli::format_inspect_error(id, &err.to_string());
                Err(invalid_input(msg))
            }
            Ok(None) => {
                println!("no task exists with id {id}");
                Ok(())
            }
            Ok(Some(task)) => {
                cli::print_inspected_task(&task, show_pii);
                Ok(())
            }
        }
    }

    /// `list <state>`: task table with `--limit`/`--offset`.
    pub async fn run_list(&self, query: &ListQuery) -> Result<()> {
        let tasks = self.store.list_tasks(query).await?;
        cli::print_task_list(&tasks);
        Ok(())
    }

    /// Single `run`: targeted (`--id`) or generic claim, no loop.
    pub async fn run_once(&self, id: Option<&str>) -> Result<()> {
        // Scope metadata only: no secrets or bodies.
        logging::debug(&format!(
            "claim scope queue={} backend={} tasks={} worker_id={}",
            self.config.queue_name,
            self.config.backend_name,
            self.config.task_paths.join(","),
            self.config.worker_id
        ));
        let Some(id) = id else {
            self.drain_once().await?;
            return Ok(());
        };
        // Targeted claim: only when runnable (READY + due + in scope).
        // Same pipeline as the generic one, without the loop.
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
                    logging::info(&format!("no task exists with id {id}"));
                    Ok(())
                }
                Ok(Some(row)) => {
                    logging::info(&format!(
                        "task {id} not runnable: status={} queue={} backend={} task_path={}",
                        row.status, row.queue_name, row.backend_name, row.task_path
                    ));
                    Ok(())
                }
            },
        }
    }

    /// Process an already-claimed task: parse -> disk policy -> SMTP -> finalize.
    /// Return `Ok(Some(code))` when the task finalized `FAILED`,
    /// `Ok(None)` on success or transient requeue; `Err` is DB-only.
    /// The code feeds the heartbeat's `last_error_code`.
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
                // `detail` may carry attacker text: shown sanitized
                // (same as persisted).
                logging::debug(&format!(
                    "payload rejected id={} code={} detail={}",
                    task.id,
                    err.code,
                    db::sanitize_detail(&err.detail)
                ));
                store
                    .finalize_failed(&task.id, err.code, &err.detail)
                    .await?;
                logging::debug(&format!("finalized id={} FAILED {}", task.id, err.code));
                logging::info(&format!("finalized id={} FAILED {}", task.id, err.code));
                return Ok(Some(err.code.to_owned()));
            }
            Ok(content) => content,
        };
        // On-disk policy (real sizes, symlinks, roots) before assembling
        // anything.
        let files = match attachments::load(&config.attachment_roots, &content.attachment_paths) {
            Err(err) => {
                logging::debug(&format!(
                    "attachment rejected id={} code={} detail={}",
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
        // Only addresses and body presence, never their content.
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
            (None, None) => "no-body",
        };
        logging::debug(&format!(
            "message id={} kind={kind} to={}",
            task.id, content.to
        ));
        // Without AUTH: connection parameters only, never secrets.
        logging::debug(&format!(
            "smtp host={} port={} tls={} user={}",
            config.smtp_host,
            config.smtp_port,
            config.smtp_use_tls,
            config
                .smtp_user
                .as_deref()
                .map(|_| "<redacted>")
                .unwrap_or("(no-auth)")
        ));
        let started = std::time::Instant::now();
        match mailer::send(config, &content, &files).await {
            Ok(()) => {
                logging::debug(&format!(
                    "sent id={} ok latency_ms={}",
                    task.id,
                    started.elapsed().as_millis()
                ));
                store.finalize_success(&task.id).await?;
                logging::debug(&format!("finalized id={} SUCCESSFUL", task.id));
                logging::info(&format!(
                    "finalized id={} SUCCESSFUL latency_ms={}",
                    task.id,
                    started.elapsed().as_millis()
                ));
            }
            Err(err) => {
                logging::debug(&format!(
                    "sent id={} error={} detail={} latency_ms={}",
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

    /// One iteration: reap zombies + one generic claim.
    /// Return the heartbeat contribution when there was work (`None` when
    /// the queue was empty). DB errors stay `Err`.
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

    /// Write the heartbeat best-effort: a failure here never aborts
    /// the loop (the reader decides freshness with the 90 s threshold).
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
            logging::warn(&format!("heartbeat write failed: {err}"));
        }
    }

    /// Persistent loop + graceful shutdown + reconnection:
    /// - The daemon child takes the lock BEFORE touching the DB (fail loud
    ///   -> exit 1); the guard lives until the end and releases the lock on
    ///   death.
    /// - Initial connection with infinite backoff (when the DB is down at
    ///   startup, don't exit 1: wait for it to come back).
    /// - An iteration failing on the DB never aborts: log WARN, 1s->30s
    ///   backoff, and carry on. The sqlx pool reconnects on its own at the
    ///   next `acquire`; we only avoid hot-looping a dead DB.
    /// - Sleeping (idle or backoff) always races `shutdown_signal`:
    ///   SIGTERM/SIGINT exits 0 even while waiting on the DB.
    /// - Self-recycling: after `PILOTWAVE_RECYCLE_N` tasks or 7 days,
    ///   return `LoopEnd::Recycle` -> exit 75 so the keep-alive restarts
    ///   and the allocator recovers fragmented memory.
    /// - Heartbeat: beat before the drain (pre-send) and after (updated
    ///   counters), plus a final one before recycling.
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
            // Shutdown during reconnect: clean exit, exit 0.
            return Ok(LoopEnd::Shutdown);
        };
        logging::info("postgres connected (max_connections=1)");
        // Least-privilege check (roadmap R-6): fail loud when the role
        // lacks the minimum GRANTs. Config error, not network: no backoff,
        // exit 1.
        if let Err(err) = db::verify_least_privilege(&pool, config.skip_grant_check).await {
            logging::error(&err);
            return Err(invalid_input(err));
        }
        let worker_id = config.worker_id.clone();
        let scope = db::QueueScope::from(&config);
        let worker = Self::new(config, PostgresTaskStore::new(pool, scope, worker_id));
        logging::debug(&format!(
            "claim scope queue={} backend={} tasks={} worker_id={}",
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
        // `due_seen` feeds `queue_depth_seen`: the beat before the first
        // drain uses 0 (the queue has not been seen yet).
        let mut due_seen: i64 = 0;
        let started = std::time::Instant::now();
        loop {
            // Recycle before claiming the next task (never mid-send). The
            // final beat leaves the row fresh so the keep-alive can tell a
            // recycle apart from a death.
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
            // Pre-drain beat (before each send; with a 30 s send this is
            // the one keeping the row fresh).
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
                    // Post-drain beat (after each send, with counters
                    // already updated).
                    worker
                        .beat(due_seen, processed, &last_task_id, &last_error_code)
                        .await;
                    empty = if did_work { 0 } else { empty.saturating_add(1) };
                    let delay = idle_delay(empty, worker.config.poll_min, worker.config.poll_max);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_signal() => {
                            logging::info("shutdown requested, exiting");
                            break;
                        }
                    }
                }
                Err(err) => {
                    db_failures = db_failures.saturating_add(1);
                    let delay = db_reconnect_delay(db_failures);
                    logging::warn(&format!(
                        "db error (consecutive failure {db_failures}): {err}; retrying in {}s",
                        delay.as_secs()
                    ));
                    // Leave `empty` alone: it counts empty polls, not failures.
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_signal() => {
                            logging::info("shutdown requested, exiting");
                            break;
                        }
                    }
                }
            }
        }
        // The pool drops here (no silent close path).
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
        // Invalid config (recycle_n=0) never reaches here (config rejects
        // it), but the helper must not ask for more than the counter holds.
        assert!(should_recycle_tasks(0, 0));
        // Age: 7 days.
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
