//! CLI definition (`clap` derive).
//!
//! The binary exposes subcommands. Everything `clap`-related lives here;
//! `main.rs` only dispatches and `db/` only accesses data.
//!
//! Behavior:
//! - Without a subcommand: show help (`arg_required_else_help`).
//! - `status`: print the counts table (see [`render_status_table`]).
//! - `run`: execute available tasks (logic in `main.rs` + `db/`).
//! - `stop`: stop the `--bg` daemon (SIGTERM + wait + SIGKILL,
//!   idempotent; never touches the DB).
//! - `list <ready|running|done|failed> [--limit N] [--offset M]`: list tasks
//!   with pagination (`LIMIT`/`OFFSET` in the query).
//! - `inspect <UUID> [--show-pii]`: show every field of one row by id
//!   (no queue filter; read-only). By default PII is trimmed (`to` shows
//!   only the domain, bodies shown as presence, `traceback` truncated); with
//!   `--show-pii` show everything (operators only).
//! - `log [-n N] [-f]`: last N lines of `PILOTWAVE_LOG_FILE` and, with `-f`,
//!   follow appends (tail); never touches the DB.

use clap::{Args, Parser, Subcommand};

use crate::db::models::{InspectedTask, ListQuery, ListedTask, QueueStatus, TaskStatus};

/// Sidecar draining the `email` queue of `django-tasks-db`.
#[derive(Debug, Parser)]
#[command(
    name = "pilotwave-worker",
    version,
    about = "Drena la cola email de django-tasks-db",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Show `DEBUG` diagnostic messages (valid before or after the subcommand).
    #[arg(short, long, global = true, action = clap::ArgAction::SetTrue)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Show the count of available, in-flight and completed tasks.
    Status,
    /// Claim and finalize: one task (`--id`), one generic task, or loop (`--fg`).
    Run(RunArgs),
    /// List tasks by state with pagination.
    List(ListArgs),
    /// Show every field of one task by id (no queue filter).
    Inspect(InspectArgs),
    /// Stop the `--bg` daemon: SIGTERM, wait >=35 s and SIGKILL when needed.
    /// Idempotent (no daemon => exit 0) and never touches the DB.
    Stop,
    /// Show the last N lines of `PILOTWAVE_LOG_FILE` (tail).
    ///
    /// Without `-f` print and exit; with `-f` print and follow appends
    /// (~500 ms poll) until SIGTERM/SIGINT (exit 0). On rotation
    /// (different inode) or truncation warn on stderr and reopen from 0.
    /// Never touches the DB.
    Log(LogArgs),
}

/// Arguments for `run`: one specific task, one generic task, loop, or daemon.
/// `--id`, `--fg` and `--bg` are mutually exclusive (`--bg-child` is internal).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RunArgs {
    /// Id (UUID) of the task to claim. Only claims when runnable:
    /// READY + due + in scope (queue/backend/task_path).
    #[arg(long, value_name = "UUID", conflicts_with_all = ["fg", "bg"])]
    pub id: Option<String>,
    /// Persistent foreground loop with graceful shutdown (SIGTERM/SIGINT).
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with_all = ["id", "bg"])]
    pub fg: bool,
    /// Same loop as `--fg` but as a detached daemon (survives shell exit).
    /// Writes a PID file with an exclusive lock.
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with_all = ["id", "fg"])]
    pub bg: bool,
    /// Detached child of `--bg` (internal use): acquires the PID-file lock
    /// and runs the loop. Hidden from help; manual mode equals `--fg` + lock.
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, conflicts_with = "id")]
    pub bg_child: bool,
}

/// Arguments for `list`: require a state subcommand; without one print help.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
#[command(arg_required_else_help = true)]
pub struct ListArgs {
    #[command(subcommand)]
    pub command: ListCommand,
}

/// State to list. Every variant accepts `--limit`/`--offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum ListCommand {
    /// Tasks in READY state.
    Ready(Pagination),
    /// Tasks in RUNNING state.
    Running(Pagination),
    /// Tasks in SUCCESSFUL state.
    Done(Pagination),
    /// Tasks in FAILED state.
    ///
    /// Reuse the generic table (`id`, `task_path`, `priority`, `enqueued_at`);
    /// error detail is left to `inspect`.
    Failed(Pagination),
}

impl ListCommand {
    /// DB state + pagination, for dispatch in `main.rs`.
    pub fn status_and_pagination(&self) -> (&'static str, Pagination) {
        match *self {
            ListCommand::Ready(pagination) => ("READY", pagination),
            ListCommand::Running(pagination) => ("RUNNING", pagination),
            ListCommand::Done(pagination) => ("SUCCESSFUL", pagination),
            ListCommand::Failed(pagination) => ("FAILED", pagination),
        }
    }

    /// Validated query object for `db`: maps the variant to `TaskStatus`
    /// and validates `limit`/`offset` in the domain.
    pub fn to_list_query(self) -> Result<ListQuery, String> {
        let (db_str, pagination) = self.status_and_pagination();
        pagination.validate()?;
        let status = match db_str {
            "READY" => TaskStatus::Ready,
            "RUNNING" => TaskStatus::Running,
            "SUCCESSFUL" => TaskStatus::Done,
            _ => TaskStatus::Failed,
        };
        ListQuery::new(status, pagination.limit, pagination.offset)
    }
}

/// Shared pagination: maps directly to the query's `LIMIT`/`OFFSET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct Pagination {
    /// Rows to return.
    #[arg(long, default_value_t = 25)]
    pub limit: i64,
    /// Rows to skip.
    #[arg(long, default_value_t = 0)]
    pub offset: i64,
}

impl Pagination {
    /// Validate ranges before touching the DB.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=200).contains(&self.limit) {
            return Err(format!(
                "--limit debe estar entre 1 y 200, got {}",
                self.limit
            ));
        }
        if self.offset < 0 {
            return Err(format!("--offset debe ser >= 0, got {}", self.offset));
        }
        Ok(())
    }
}

/// Upper bound for `--lines` in `log` (avoids giant outputs).
pub const LOG_LINES_MAX: i64 = 10_000;

/// Arguments for `log`: tail of the daemon's line-oriented log.
///
/// The long contract (follow, rotation, truncation, stdout/stderr) lives in
/// the `Command::Log` variant docs, which is what `--help` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct LogArgs {
    /// Trailing lines to show (1-10000).
    #[arg(short = 'n', long, default_value_t = 10, allow_hyphen_values = true)]
    pub lines: i64,
    /// Follow the file after showing the last N lines (~500 ms poll).
    #[arg(short = 'f', long, action = clap::ArgAction::SetTrue)]
    pub follow: bool,
}

/// Validate `--lines`: integer in 1..=10000. Message ready for stderr.
pub fn validate_log_lines(n: i64) -> Result<u64, String> {
    if (1..=LOG_LINES_MAX).contains(&n) {
        Ok(n as u64)
    } else {
        Err(format!(
            "--lines debe estar entre 1 y {LOG_LINES_MAX}, got {n}"
        ))
    }
}

/// Arguments for `inspect`: the id of the row to show.
/// By default PII is trimmed; `--show-pii` is operators-only.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InspectArgs {
    /// Id (UUID) of the task row in `django_tasks_database_dbtaskresult`.
    #[arg(value_name = "UUID")]
    pub id: String,
    /// Show full PII (`to`, bodies and `traceback` untrimmed).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub show_pii: bool,
}

/// Visible `subject` characters in trimmed mode (default).
pub const INSPECT_SUBJECT_MAX_CHARS: usize = 80;
/// Visible `traceback` characters in trimmed mode (default).
pub const INSPECT_TRACEBACK_MAX_CHARS: usize = 1000;
/// Visible characters of a non-JSON `args_kwargs` in trimmed mode.
pub const INSPECT_RAW_MAX_CHARS: usize = 1000;

/// Truncate to N characters (never splitting UTF-8); when trimming, tell how
/// to see everything.
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let truncated: String = value.chars().take(max).collect();
    format!("{truncated}… [truncado, usa --show-pii]")
}

/// Mask an email showing only the domain (`***@domain`); without a valid
/// at-sign return `[redacted]`.
fn mask_email(value: &str) -> String {
    let single_at = value.chars().filter(|&c| c == '@').count() == 1;
    match value.rsplit_once('@') {
        Some((_, domain)) if single_at && !domain.is_empty() => format!("***@{domain}"),
        _ => "[redacted]".to_owned(),
    }
}

/// Trim `args_kwargs` PII for the default `inspect` mode: `to` shows only
/// the domain, `subject` is truncated and bodies are replaced by presence
/// (`[present: N chars]`). Applies both to the real format
/// (`{"args", "kwargs"}`) and to a flat object; remaining keys are kept
/// for operations. When not JSON, truncate the text.
fn redact_args_kwargs(raw: &str) -> String {
    let mut value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return truncate_chars(raw, INSPECT_RAW_MAX_CHARS),
    };
    let target = match &mut value {
        serde_json::Value::Object(root) => match root.get_mut("kwargs") {
            Some(serde_json::Value::Object(kwargs)) => Some(kwargs),
            _ => Some(root),
        },
        _ => None,
    };
    if let Some(map) = target {
        if let Some(to) = map.get("to").and_then(|v| v.as_str()).map(str::to_owned) {
            map.insert("to".to_owned(), serde_json::Value::String(mask_email(&to)));
        }
        if let Some(subject) = map
            .get("subject")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
        {
            map.insert(
                "subject".to_owned(),
                serde_json::Value::String(truncate_chars(&subject, INSPECT_SUBJECT_MAX_CHARS)),
            );
        }
        for key in ["text_body", "html_body"] {
            if let Some(body) = map.get(key).and_then(|v| v.as_str()).map(str::to_owned) {
                let len = body.chars().count();
                map.insert(
                    key.to_owned(),
                    serde_json::Value::String(format!("[presente: {len} chars]")),
                );
            }
        }
    }
    serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| truncate_chars(raw, INSPECT_RAW_MAX_CHARS))
}

/// Render the `inspect` detail as `key: value` lines.
///
/// By default (`show_pii = false`) PII is trimmed: `to` shows only the
/// domain, bodies appear as presence and `subject`/`traceback` are
/// truncated. With `show_pii = true` (`--show-pii` flag, operators only)
/// everything is shown: full `traceback` and `args_kwargs` reindented as
/// JSON. `None` values render as `-`.
pub fn render_inspected_task(task: &InspectedTask, show_pii: bool) -> String {
    fn opt(value: &Option<String>) -> &str {
        value.as_deref().unwrap_or("-")
    }
    let (traceback, args_kwargs) = if show_pii {
        (
            task.traceback.clone(),
            pretty_json_or_raw(&task.args_kwargs),
        )
    } else {
        (
            truncate_chars(&task.traceback, INSPECT_TRACEBACK_MAX_CHARS),
            redact_args_kwargs(&task.args_kwargs),
        )
    };
    let mut out = String::new();
    for (key, value) in [
        ("id", task.id.as_str()),
        ("status", task.status.as_str()),
        ("task_path", task.task_path.as_str()),
        ("queue_name", task.queue_name.as_str()),
        ("backend_name", task.backend_name.as_str()),
        ("priority", &task.priority.to_string()),
        ("run_after", task.run_after.as_str()),
        ("enqueued_at", task.enqueued_at.as_str()),
        ("started_at", opt(&task.started_at)),
        ("finished_at", opt(&task.finished_at)),
        ("worker_ids", task.worker_ids.as_str()),
        ("return_value", opt(&task.return_value)),
        ("exception_class_path", task.exception_class_path.as_str()),
        ("traceback", &traceback),
        ("args_kwargs", &args_kwargs),
    ] {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Print the `inspect` detail to stdout.
pub fn print_inspected_task(task: &InspectedTask, show_pii: bool) {
    print!("{}", render_inspected_task(task, show_pii));
}

/// Reindent `raw` when it is valid JSON; otherwise return the text as-is.
fn pretty_json_or_raw(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_owned()),
        Err(_) => raw.to_owned(),
    }
}

/// Detect the Postgres error raised when the id is not a valid UUID.
pub fn is_uuid_syntax_error(db_message: &str) -> bool {
    db_message.contains("invalid input syntax for type uuid")
}

/// Clean (usage-style) message for a non-UUID id.
pub fn render_inspect_uuid_error(id: &str) -> String {
    format!("id inválido: '{id}' no es un UUID válido; uso: inspect <UUID>")
}

/// Map an `inspect` DB error to a user message:
/// invalid UUID => clean message; anything else => error as-is.
pub fn format_inspect_error(id: &str, db_message: &str) -> String {
    if is_uuid_syntax_error(db_message) {
        render_inspect_uuid_error(id)
    } else {
        format!("error al inspeccionar la tarea {id}: {db_message}")
    }
}
///
/// Keep rendering separate from `println!` for testing without capturing
/// stdout. Render the `status` table as text.
///
/// Separa el render del `println!` para poder probarlo sin capturar stdout.
pub fn render_status_table(status: &QueueStatus) -> String {
    let rows = [
        ("disponibles", status.ready),
        ("en proceso", status.running),
        ("completadas", status.done),
    ];
    let label_width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .max("estado".len());
    let value_width = rows
        .iter()
        .map(|(_, value)| value.to_string().len())
        .max()
        .unwrap_or(1)
        .max("total".len());

    let border = format!(
        "+-{}-+-{}-+",
        "-".repeat(label_width),
        "-".repeat(value_width)
    );
    let mut out = String::new();
    out.push_str(&border);
    out.push('\n');
    out.push_str(&format!(
        "| {:<label_width$} | {:>value_width$} |",
        "estado", "total"
    ));
    out.push('\n');
    out.push_str(&border);
    out.push('\n');
    for (label, value) in rows {
        out.push_str(&format!(
            "| {label:<label_width$} | {value:>value_width$} |"
        ));
        out.push('\n');
    }
    out.push_str(&border);
    out
}

/// Print the `status` table to stdout.
pub fn print_status_table(status: &QueueStatus) {
    println!("{}", render_status_table(status));
}

/// Daemon state for the `daemon:` section of `status`.
///
/// `Running` carries the already-resolved best-effort data (`None` = `/proc`
/// unreadable => rendered `n/a`, never failing).
pub enum DaemonInfo {
    /// Without `PILOTWAVE_PID_FILE`: the daemon could never be configured.
    NotConfigured,
    /// A pidfile exists but no lock is held (missing, stale or unlocked).
    NotRunning,
    /// Lock held by a process with valid identity.
    Running {
        pid: u32,
        uptime: Option<u64>,
        rss_kb: Option<u64>,
    },
}

/// Render the `status` daemon section (one line).
///
/// Separa el render del `println!` para poder probarlo sin capturar stdout.
/// Examples:
/// - `daemon: running pid=1234 uptime=1h2m3s rss=4192 kB (4.1 MB)`
/// - `daemon: running pid=1234 uptime=n/a rss=n/a` (best-effort failure)
/// - `daemon: not running (pidfile stale/unlocked)`
/// - `daemon: not configured (PILOTWAVE_PID_FILE missing)`
pub fn render_daemon_status(state: &DaemonInfo) -> String {
    match state {
        DaemonInfo::NotConfigured => {
            "daemon: no configurado (PILOTWAVE_PID_FILE ausente)".to_owned()
        }
        DaemonInfo::NotRunning => "daemon: no corriendo (pidfile stale/unlocked)".to_owned(),
        DaemonInfo::Running {
            pid,
            uptime,
            rss_kb,
        } => {
            let up = uptime
                .map(crate::proc::format_uptime)
                .unwrap_or_else(|| "n/a".to_owned());
            let rss = rss_kb
                .map(crate::proc::format_rss)
                .unwrap_or_else(|| "n/a".to_owned());
            format!("daemon: running pid={pid} uptime={up} rss={rss}")
        }
    }
}

/// Render the `status` `heartbeat:` line from the DB row.
///
/// `metadata` is parsed best-effort: when missing or garbage, fields render
/// `n/a`/`-` instead of failing (the heartbeat never breaks `status`).
/// Ejemplos:
/// - `heartbeat: fresh last_seen=2026-11-19T12:00:00Z (hace 3s) tasks_processed=42 pid=1234 uptime=1h2m3s rss=4192 kB (4.1 MB) last_task=… last_error=-`
/// - `heartbeat: stale …` (same shape, the worker died or got stuck)
pub fn render_heartbeat_status(age_secs: i64, last_seen: &str, metadata: &str) -> String {
    let fresh = crate::heartbeat::freshness(age_secs);
    let v: serde_json::Value = serde_json::from_str(metadata).unwrap_or(serde_json::Value::Null);
    let num = |key: &str| v.get(key).and_then(|n| n.as_u64());
    let text = |key: &str| v.get(key).and_then(|n| n.as_str()).unwrap_or("-");
    let up = num("uptime_secs")
        .map(crate::proc::format_uptime)
        .unwrap_or_else(|| "n/a".to_owned());
    let rss = num("rss_kb")
        .map(crate::proc::format_rss)
        .unwrap_or_else(|| "n/a".to_owned());
    let tasks = num("tasks_processed")
        .map(|n| n.to_string())
        .unwrap_or_else(|| "n/a".to_owned());
    let pid = num("pid")
        .map(|n| n.to_string())
        .unwrap_or_else(|| "n/a".to_owned());
    format!(
        "heartbeat: {fresh} last_seen={last_seen} (hace {age_secs}s) \
         tasks_processed={tasks} pid={pid} uptime={up} rss={rss} \
         last_task={} last_error={}",
        text("last_task_id"),
        text("last_error_code"),
    )
}

/// `stop` message when no daemon is alive.
///
/// Idempotent (cron-safe): missing, stale or unlocked pidfile, or an
/// already-dead PID => exit 0 with this line on stdout.
pub fn render_stop_not_running() -> String {
    "daemon: no corriendo".to_owned()
}

/// `stop` success message after killing the daemon.
///
/// `forced` tells the paths apart: `false` = SIGTERM was enough, `true` =
/// SIGKILL was needed. Keep rendering separate from `println!` for testing
/// without capturing stdout.
pub fn render_stop_done(pid: u32, forced: bool) -> String {
    if forced {
        format!("daemon detenido pid={pid} (SIGKILL)")
    } else {
        format!("daemon detenido pid={pid}")
    }
}

/// Render the `list <state>` table as text.
pub fn render_task_list(tasks: &[ListedTask]) -> String {
    const HEADERS: [&str; 4] = ["id", "task_path", "priority", "enqueued_at"];
    let mut widths = [
        HEADERS[0].len(),
        HEADERS[1].len(),
        HEADERS[2].len(),
        HEADERS[3].len(),
    ];
    for task in tasks {
        widths[0] = widths[0].max(task.id.len());
        widths[1] = widths[1].max(task.task_path.len());
        widths[2] = widths[2].max(task.priority.to_string().len());
        widths[3] = widths[3].max(task.enqueued_at.len());
    }
    let [w_id, w_path, w_prio, w_enq] = widths;
    let border = format!(
        "+-{}-+-{}-+-{}-+-{}-+",
        "-".repeat(w_id),
        "-".repeat(w_path),
        "-".repeat(w_prio),
        "-".repeat(w_enq)
    );
    let mut out = String::new();
    out.push_str(&border);
    out.push('\n');
    out.push_str(&format!(
        "| {:<w_id$} | {:<w_path$} | {:>w_prio$} | {:<w_enq$} |",
        HEADERS[0], HEADERS[1], HEADERS[2], HEADERS[3]
    ));
    out.push('\n');
    out.push_str(&border);
    out.push('\n');
    for task in tasks {
        out.push_str(&format!(
            "| {:<w_id$} | {:<w_path$} | {:>w_prio$} | {:<w_enq$} |",
            task.id, task.task_path, task.priority, task.enqueued_at
        ));
        out.push('\n');
    }
    out.push_str(&border);
    out
}

/// Print the `list <state>` table to stdout.
pub fn print_task_list(tasks: &[ListedTask]) {
    println!("{}", render_task_list(tasks));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_table_contains_labels_and_values() {
        let table = render_status_table(&QueueStatus {
            ready: 3,
            running: 1,
            done: 42,
        });
        assert!(table.contains("disponibles"));
        assert!(table.contains("en proceso"));
        assert!(table.contains("completadas"));
        assert!(table.contains('3'));
        assert!(table.contains("42"));
    }

    #[test]
    fn sin_subcomando_o_sin_estado_falla_con_ayuda() {
        // `arg_required_else_help(true)` => sin subcomando clap devuelve error,
        // no un `Cli` con comando por defecto.
        let err =
            Cli::try_parse_from(["pilotwave-worker"]).expect_err("debe fallar sin subcomando");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        let err = Cli::try_parse_from(["pilotwave-worker", "list"])
            .expect_err("list sin estado debe fallar con ayuda");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn parsea_subcomandos_basicos_status_run_inspect_stop() {
        assert!(matches!(
            Cli::try_parse_from(["pilotwave-worker", "status"])
                .unwrap()
                .command,
            Command::Status
        ));
        let Command::Run(args) = Cli::try_parse_from(["pilotwave-worker", "run"])
            .unwrap()
            .command
        else {
            panic!("se esperaba Command::Run");
        };
        assert_eq!(args.id, None);
        assert!(!args.fg);
        assert!(!args.bg);
        assert!(!args.bg_child);
        // inspect with valid UUID (trims PII by default).
        let uuid = "4b5ab53d-c51d-4845-9daa-3f88b401ac07";
        let cli = Cli::try_parse_from(["pilotwave-worker", "inspect", uuid]).unwrap();
        let Command::Inspect(args) = cli.command else {
            panic!("se esperaba Command::Inspect");
        };
        assert_eq!(args.id, uuid);
        assert!(!args.show_pii);
        // --show-pii opta a la salida completa (solo operadores).
        let cli = Cli::try_parse_from(["pilotwave-worker", "inspect", uuid, "--show-pii"]).unwrap();
        let Command::Inspect(args) = cli.command else {
            panic!("se esperaba Command::Inspect");
        };
        assert_eq!(args.id, uuid);
        assert!(args.show_pii);
        // stop no lleva args.
        assert!(matches!(
            Cli::try_parse_from(["pilotwave-worker", "stop"])
                .unwrap()
                .command,
            Command::Stop
        ));
    }

    #[test]
    fn run_flags_exclusivos_fg_bg_bg_child_e_id() {
        let fg = Cli::try_parse_from(["pilotwave-worker", "run", "--fg"]).unwrap();
        let Command::Run(args) = fg.command else {
            panic!("se esperaba Command::Run");
        };
        assert!(args.fg);
        assert_eq!(args.id, None);

        let id = "4b5ab53d-c51d-4845-9daa-3f88b401ac07";
        let by_id = Cli::try_parse_from(["pilotwave-worker", "run", "--id", id]).unwrap();
        let Command::Run(args) = by_id.command else {
            panic!("se esperaba Command::Run");
        };
        assert_eq!(args.id.as_deref(), Some(id));
        assert!(!args.fg);
        // --id excluye cualquier modo loop.
        assert!(Cli::try_parse_from(["pilotwave-worker", "run", "--id", id, "--fg"]).is_err());
        assert!(Cli::try_parse_from(["pilotwave-worker", "run", "--id", id, "--bg"]).is_err());
        assert!(
            Cli::try_parse_from(["pilotwave-worker", "run", "--id", id, "--bg-child"]).is_err()
        );

        let bg = Cli::try_parse_from(["pilotwave-worker", "run", "--bg"]).unwrap();
        let Command::Run(args) = bg.command else {
            panic!("se esperaba Command::Run");
        };
        assert!(args.bg);
        assert!(!args.fg);
        assert_eq!(args.id, None);
        assert!(Cli::try_parse_from(["pilotwave-worker", "run", "--fg", "--bg"]).is_err());

        // --bg-child oculto en la ayuda pero parseable (lo usa el padre --bg).
        let child = Cli::try_parse_from(["pilotwave-worker", "run", "--bg-child"]).unwrap();
        let Command::Run(args) = child.command else {
            panic!("se esperaba Command::Run");
        };
        assert!(args.bg_child);
    }

    #[test]
    fn verbose_flag_por_defecto_off_y_formas_corta_larga() {
        for argv in [
            vec!["pilotwave-worker", "status"],
            vec!["pilotwave-worker", "run"],
            vec!["pilotwave-worker", "list", "ready"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(!cli.verbose);
        }
        for argv in [
            vec!["pilotwave-worker", "-v", "status"],
            vec!["pilotwave-worker", "--verbose", "status"],
            vec!["pilotwave-worker", "-v", "run"],
            vec!["pilotwave-worker", "--verbose", "run"],
            vec!["pilotwave-worker", "-v", "list", "ready"],
            vec!["pilotwave-worker", "--verbose", "list", "done"],
            // Global flag: also valid after the subcommand and in log/stop.
            vec!["pilotwave-worker", "status", "-v"],
            vec!["pilotwave-worker", "run", "--verbose"],
            vec!["pilotwave-worker", "-v", "log"],
            vec!["pilotwave-worker", "log", "--verbose"],
            vec!["pilotwave-worker", "-v", "stop"],
            vec!["pilotwave-worker", "stop", "--verbose"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(cli.verbose);
        }
    }

    #[test]
    fn list_parsea_estado_paginacion_defaults_y_validacion() {
        use crate::db::models::TaskStatus;
        // ready: defaults limit 25 / offset 0.
        let cli = Cli::try_parse_from(["pilotwave-worker", "list", "ready"]).unwrap();
        let Command::List(args) = cli.command else {
            panic!("se esperaba Command::List");
        };
        let (status, pagination) = args.command.status_and_pagination();
        assert_eq!(status, "READY");
        assert_eq!(
            pagination,
            Pagination {
                limit: 25,
                offset: 0
            }
        );
        // done with explicit pagination.
        let cli = Cli::try_parse_from([
            "pilotwave-worker",
            "list",
            "done",
            "--limit",
            "10",
            "--offset",
            "40",
        ])
        .unwrap();
        let Command::List(args) = cli.command else {
            panic!("se esperaba Command::List");
        };
        let (status, pagination) = args.command.status_and_pagination();
        assert_eq!(status, "SUCCESSFUL");
        assert_eq!(pagination.limit, 10);
        assert_eq!(pagination.offset, 40);
        // failed maps to FAILED and validates limits.
        let cli = Cli::try_parse_from([
            "pilotwave-worker",
            "list",
            "failed",
            "--limit",
            "5",
            "--offset",
            "10",
        ])
        .unwrap();
        let Command::List(args) = cli.command else {
            panic!("se esperaba Command::List");
        };
        let query = args.command.to_list_query().unwrap();
        assert_eq!(query.status, TaskStatus::Failed);
        assert_eq!(query.limit, 5);
        assert_eq!(query.offset, 10);
        // running mapea y valida.
        let cli = Cli::try_parse_from(["pilotwave-worker", "list", "running"]).unwrap();
        let Command::List(args) = cli.command else {
            panic!("se esperaba Command::List");
        };
        let query = args.command.to_list_query().unwrap();
        assert_eq!(query.status, TaskStatus::Running);
        // Fuera de rango: 0, >200 y offset negativo.
        for bad in [
            Pagination {
                limit: 0,
                offset: 0,
            },
            Pagination {
                limit: 201,
                offset: 0,
            },
            Pagination {
                limit: 10,
                offset: -1,
            },
        ] {
            assert!(bad.validate().is_err());
            assert!(
                ListCommand::Failed(Pagination {
                    limit: bad.limit,
                    offset: bad.offset
                })
                .to_list_query()
                .is_err()
            );
        }
    }

    #[test]
    fn inspect_con_show_pii_muestra_todo_sin_recortar() {
        let traceback = "Traceback (most recent call last):\n  File \"x.py\", line 1, in <module>\nValueError: malo\n".repeat(50);
        let task = InspectedTask {
            id: "4b5ab53d-c51d-4845-9daa-3f88b401ac07".to_owned(),
            status: "FAILED".to_owned(),
            task_path: "pilotwave.tasks.mail_notification".to_owned(),
            queue_name: "pilotwave.email".to_owned(),
            backend_name: "db".to_owned(),
            priority: 0,
            run_after: "2026-11-17T10:00:00Z".to_owned(),
            enqueued_at: "2026-11-17T10:00:01Z".to_owned(),
            started_at: Some("2026-11-17T10:00:02Z".to_owned()),
            finished_at: Some("2026-11-17T10:00:03Z".to_owned()),
            worker_ids: "[\"w1\"]".to_owned(),
            return_value: Some("null".to_owned()),
            exception_class_path: "ValueError".to_owned(),
            traceback: traceback.clone(),
            args_kwargs: r#"{"args":[],"kwargs":{"to":"a@example.com","subject":"hola","text_body":"cuerpo secreto"}}"#.to_owned(),
        };
        let out = render_inspected_task(&task, true);
        let keys = [
            "id",
            "status",
            "task_path",
            "queue_name",
            "backend_name",
            "priority",
            "run_after",
            "enqueued_at",
            "started_at",
            "finished_at",
            "worker_ids",
            "return_value",
            "exception_class_path",
            "traceback",
            "args_kwargs",
        ];
        let mut last = 0;
        for key in keys {
            let pos = out
                .find(&format!("{key}: "))
                .unwrap_or_else(|| panic!("falta la clave {key}"));
            assert!(pos >= last, "la clave {key} está desordenada");
            last = pos;
        }
        // Traceback completo, sin truncar.
        assert!(out.contains(&traceback));
        // args_kwargs reindentado como JSON, con la PII intacta.
        assert!(out.contains("\"to\": \"a@example.com\""));
        assert!(out.contains("cuerpo secreto"));
    }

    #[test]
    fn inspect_por_defecto_recorta_pii() {
        let traceback = "x".repeat(2000);
        let subject = "s".repeat(200);
        let task = InspectedTask {
            id: "4b5ab53d-c51d-4845-9daa-3f88b401ac07".to_owned(),
            status: "FAILED".to_owned(),
            task_path: "pilotwave.tasks.mail_notification".to_owned(),
            queue_name: "pilotwave.email".to_owned(),
            backend_name: "db".to_owned(),
            priority: 0,
            run_after: "2026-11-17T10:00:00Z".to_owned(),
            enqueued_at: "2026-11-17T10:00:01Z".to_owned(),
            started_at: Some("2026-11-17T10:00:02Z".to_owned()),
            finished_at: Some("2026-11-17T10:00:03Z".to_owned()),
            worker_ids: "[\"w1\"]".to_owned(),
            return_value: Some("null".to_owned()),
            exception_class_path: "ValueError".to_owned(),
            traceback,
            args_kwargs: format!(
                r#"{{"args":[],"kwargs":{{"to":"victima@example.com","subject":"{subject}","text_body":"cuerpo secreto","html_body":"<p>secreto</p>"}}}}"#
            ),
        };
        let out = render_inspected_task(&task, false);
        // `to` muestra solo el dominio; el local no sale.
        assert!(out.contains("***@example.com"));
        assert!(!out.contains("victima@example.com"));
        assert!(!out.contains("victima@"));
        // Cuerpos como presencia, sin el contenido.
        assert!(out.contains("[presente:"));
        assert!(!out.contains("cuerpo secreto"));
        assert!(!out.contains("<p>secreto</p>"));
        // `subject` y `traceback` truncados con aviso de `--show-pii`.
        assert!(!out.contains(&"s".repeat(200)));
        assert!(!out.contains(&"x".repeat(2000)));
        assert!(out.contains("usa --show-pii"));
    }

    #[test]
    fn inspect_recorta_to_sin_arroba_y_args_planos() {
        // Without a valid at-sign => [redacted]; flat object (no `kwargs`) too
        // se recorta.
        let task = InspectedTask {
            id: "11111111-2222-4333-8444-555555555555".to_owned(),
            status: "READY".to_owned(),
            task_path: "pilotwave.tasks.mail_notification".to_owned(),
            queue_name: "pilotwave.email".to_owned(),
            backend_name: "db".to_owned(),
            priority: 5,
            run_after: "2026-11-17T10:00:00Z".to_owned(),
            enqueued_at: "2026-11-17T10:00:01Z".to_owned(),
            started_at: None,
            finished_at: None,
            worker_ids: "[]".to_owned(),
            return_value: None,
            exception_class_path: String::new(),
            traceback: String::new(),
            args_kwargs: r#"{"to":"no-es-email","subject":"s","text_body":"secreto"}"#.to_owned(),
        };
        let out = render_inspected_task(&task, false);
        assert!(out.contains("[redacted]"));
        assert!(!out.contains("no-es-email\""));
        assert!(!out.contains("secreto"));
        // Con --show-pii sale tal cual.
        let full = render_inspected_task(&task, true);
        assert!(full.contains("\"to\": \"no-es-email\""));
        assert!(full.contains("secreto"));
    }

    #[test]
    fn inspect_renders_none_as_dash_and_raw_args_kwargs() {
        let task = InspectedTask {
            id: "11111111-2222-4333-8444-555555555555".to_owned(),
            status: "READY".to_owned(),
            task_path: "pilotwave.tasks.mail_notification".to_owned(),
            queue_name: "pilotwave.email".to_owned(),
            backend_name: "db".to_owned(),
            priority: 5,
            run_after: "2026-11-17T10:00:00Z".to_owned(),
            enqueued_at: "2026-11-17T10:00:01Z".to_owned(),
            started_at: None,
            finished_at: None,
            worker_ids: "[]".to_owned(),
            return_value: None,
            exception_class_path: String::new(),
            traceback: String::new(),
            args_kwargs: "no-es-json{".to_owned(),
        };
        let out = render_inspected_task(&task, false);
        assert!(out.contains("started_at: -\n"));
        assert!(out.contains("finished_at: -\n"));
        assert!(out.contains("return_value: -\n"));
        assert!(out.contains("args_kwargs: no-es-json{"));
    }

    #[test]
    fn garbage_id_maps_to_clean_uuid_error() {
        // Valid UUID v4 + foreign error: passed through, no clean message.
        let other =
            format_inspect_error("550e8400-e29b-41d4-a716-446655440000", "connection refused");
        assert!(other.contains("connection refused"));
        assert!(!other.contains("no es un UUID válido"));
        // Basura + error de sintaxis uuid de Postgres: mensaje limpio.
        let clean = format_inspect_error(
            "no-es-uuid",
            r#"db error: ERROR: invalid input syntax for type uuid: "no-es-uuid""#,
        );
        assert!(clean.contains("no-es-uuid"));
        assert!(clean.contains("no es un UUID válido"));
        assert!(clean.contains("uso: inspect <UUID>"));
        assert!(is_uuid_syntax_error(
            r#"invalid input syntax for type uuid: "x""#
        ));
        assert!(!is_uuid_syntax_error("connection refused"));
    }

    #[test]
    fn task_list_renders_headers_and_rows() {
        let table = render_task_list(&[ListedTask {
            id: "11111111-2222-3333-4444-555555555555".to_owned(),
            task_path: "pilotwave.tasks.mail_notification".to_owned(),
            priority: 0,
            enqueued_at: "2026-11-17T10:00:00Z".to_owned(),
        }]);
        assert!(table.contains("task_path"));
        assert!(table.contains("enqueued_at"));
        assert!(table.contains("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn parses_log_defaults_y_flags() {
        let cli = Cli::try_parse_from(["pilotwave-worker", "log"]).unwrap();
        let Command::Log(args) = cli.command else {
            panic!("se esperaba Command::Log");
        };
        assert_eq!(args.lines, 10);
        assert!(!args.follow);

        let cli = Cli::try_parse_from(["pilotwave-worker", "log", "-n", "20"]).unwrap();
        let Command::Log(args) = cli.command else {
            panic!("se esperaba Command::Log");
        };
        assert_eq!(args.lines, 20);

        let cli =
            Cli::try_parse_from(["pilotwave-worker", "log", "--lines", "5", "--follow"]).unwrap();
        let Command::Log(args) = cli.command else {
            panic!("se esperaba Command::Log");
        };
        assert_eq!(args.lines, 5);
        assert!(args.follow);

        let cli = Cli::try_parse_from(["pilotwave-worker", "log", "-f"]).unwrap();
        let Command::Log(args) = cli.command else {
            panic!("se esperaba Command::Log");
        };
        assert!(args.follow);
    }

    #[test]
    fn validate_log_lines_acepta_1_a_10000() {
        assert_eq!(validate_log_lines(1), Ok(1));
        assert_eq!(validate_log_lines(10), Ok(10));
        assert_eq!(validate_log_lines(10_000), Ok(10_000));
        for bad in [0, -1, -100, 10_001, i64::MAX, i64::MIN] {
            let err = validate_log_lines(bad).expect_err("debe fallar alto");
            assert!(err.contains("--lines"), "mensaje claro, got: {err}");
            assert!(err.contains(&bad.to_string()), "eco del valor, got: {err}");
        }
    }

    #[test]
    fn stop_renders_mensajes_en_espanol() {
        let idle = render_stop_not_running();
        assert!(idle.contains("no corriendo"));
        let term = render_stop_done(1234, false);
        assert!(term.contains("detenido"));
        assert!(term.contains("pid=1234"));
        assert!(!term.contains("SIGKILL"));
        let kill = render_stop_done(1234, true);
        assert!(kill.contains("detenido"));
        assert!(kill.contains("pid=1234"));
        assert!(kill.contains("SIGKILL"));
    }

    #[test]
    fn daemon_no_configurado_no_corriendo_y_running_con_o_sin_proc() {
        let out = render_daemon_status(&DaemonInfo::NotConfigured);
        assert!(out.contains("no configurado"));
        assert!(out.contains("PILOTWAVE_PID_FILE"));

        let out = render_daemon_status(&DaemonInfo::NotRunning);
        assert!(out.contains("no corriendo"));
        assert!(out.contains("stale/unlocked"));

        let out = render_daemon_status(&DaemonInfo::Running {
            pid: 1234,
            uptime: Some(3723),
            rss_kb: Some(4192),
        });
        assert!(out.contains("running"));
        assert!(out.contains("pid=1234"));
        assert!(out.contains("uptime=1h2m3s"));
        assert!(out.contains("rss=4192 kB (4.1 MB)"));

        // Best-effort: /proc ilegible => n/a sin romper.
        let out = render_daemon_status(&DaemonInfo::Running {
            pid: 1234,
            uptime: None,
            rss_kb: None,
        });
        assert!(out.contains("running"));
        assert!(out.contains("uptime=n/a"));
        assert!(out.contains("rss=n/a"));
    }

    #[test]
    fn heartbeat_render_fresh_stale_y_metadata_basura() {
        let meta = serde_json::json!({
            "version": 1,
            "queue_depth_seen": 3,
            "tasks_processed": 42,
            "last_task_id": "11111111-2222-3333-4444-555555555555",
            "last_error_code": null,
            "pid": 1234,
            "uptime_secs": 3723,
            "rss_kb": 4192,
        })
        .to_string();
        let out = render_heartbeat_status(3, "2026-11-19T12:00:00Z", &meta);
        assert!(out.contains("heartbeat: fresh"));
        assert!(out.contains("2026-11-19T12:00:00Z"));
        assert!(out.contains("tasks_processed=42"));
        assert!(out.contains("pid=1234"));
        assert!(out.contains("uptime=1h2m3s"));
        assert!(out.contains("rss=4192 kB (4.1 MB)"));
        assert!(out.contains("last_task=11111111-2222-3333-4444-555555555555"));
        assert!(out.contains("last_error=-"));
        // Stale + metadata basura: n/a sin romper.
        let out = render_heartbeat_status(3600, "2026-11-19T11:00:00Z", "no-es-json{");
        assert!(out.contains("heartbeat: stale"));
        assert!(out.contains("tasks_processed=n/a"));
        assert!(out.contains("pid=n/a"));
        assert!(out.contains("uptime=n/a"));
        assert!(out.contains("rss=n/a"));
        assert!(out.contains("last_task=-"));
    }
}
