//! Definición del CLI (`clap` derive).
//!
//! PLAN-003 (fuera del plan original): el binario expone subcomandos.
//! Todo lo relacionado con `clap` vive aquí; `main.rs` solo despacha y
//! `db/` solo accede a datos.
//!
//! Comportamiento:
//! - Sin subcomando: se muestra la ayuda (`arg_required_else_help`).
//! - `status`: imprime la tabla de conteos (ver [`render_status_table`]).
//! - `run`: ejecuta tareas disponibles (lógica en `main.rs` + `db/`).
//! - `stop`: detiene el daemon `--bg` (SIGTERM + espera + SIGKILL,
//!   idempotente; no toca la BD).
//! - `list <ready|running|done|failed> [--limit N] [--offset M]`: lista tareas con
//!   paginación (`LIMIT`/`OFFSET` en el query).
//! - `inspect <UUID> [--show-pii]`: muestra todos los campos de una fila por id
//!   (sin filtro de cola; solo lectura). Por defecto recorta la PII (`to`
//!   solo dominio, cuerpos como presencia, `traceback` truncado); con
//!   `--show-pii` muestra todo (solo operadores).
//! - `log [-n N] [-f]`: últimas N líneas de `PILOTWAVE_LOG_FILE` y, con `-f`,
//!   sigue los appends (tail); no toca la BD.

use clap::{Args, Parser, Subcommand};

use crate::db::models::{InspectedTask, ListQuery, ListedTask, QueueStatus, TaskStatus};

/// Sidecar que drena la cola `email` de `django-tasks-db`.
#[derive(Debug, Parser)]
#[command(
    name = "pilotwave-worker",
    version,
    about = "Drena la cola email de django-tasks-db",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Muestra mensajes de diagnóstico `DEBUG` (vale antes o después del subcomando).
    #[arg(short, long, global = true, action = clap::ArgAction::SetTrue)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Muestra el conteo de tareas disponibles, en proceso y completadas.
    Status,
    /// Reclama y finaliza: una tarea (`--id`), una genérica, o loop (`--fg`).
    Run(RunArgs),
    /// Lista tareas por estado con paginación.
    List(ListArgs),
    /// Muestra todos los campos de una tarea por id (sin filtro de cola).
    Inspect(InspectArgs),
    /// Detiene el daemon `--bg`: SIGTERM, espera ≥35 s y SIGKILL si hace
    /// falta (§3.9). Idempotente (sin daemon => exit 0) y sin tocar la BD.
    Stop,
    /// Muestra las últimas N líneas de `PILOTWAVE_LOG_FILE` (tail).
    ///
    /// Sin `-f` imprime y sale; con `-f` imprime y sigue los appends
    /// (poll ~500 ms) hasta SIGTERM/SIGINT (exit 0). Ante rotación
    /// (inode distinto) o truncado avisa a stderr (`rotación detectada:
    /// reabriendo desde el inicio`) y reabre desde 0.
    /// No toca la BD.
    Log(LogArgs),
}

/// Argumentos de `run`: una tarea concreta, una genérica, loop, o daemon.
/// `--id`, `--fg` y `--bg` son excluyentes entre sí (`--bg-child` es interno).
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RunArgs {
    /// Id (UUID) de la tarea a reclamar. Solo reclama si es ejecutable:
    /// READY + vencida + dentro del alcance (cola/backend/task_path).
    #[arg(long, value_name = "UUID", conflicts_with_all = ["fg", "bg"])]
    pub id: Option<String>,
    /// Loop persistente en foreground con shutdown graceful (SIGTERM/SIGINT).
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with_all = ["id", "bg"])]
    pub fg: bool,
    /// Mismo loop que `--fg` pero como daemon destacamentado (sobrevive al
    /// cierre del shell). Escribe PID file con lock exclusivo (§3.8).
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with_all = ["id", "fg"])]
    pub bg: bool,
    /// Hijo destacamentado de `--bg` (uso interno): adquiere el lock del PID
    /// file y corre el loop. Oculto en la ayuda; manual equivale a `--fg` + lock.
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, conflicts_with = "id")]
    pub bg_child: bool,
}

/// Argumentos de `list`: exige un subcomando de estado; sin él imprime ayuda.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
#[command(arg_required_else_help = true)]
pub struct ListArgs {
    #[command(subcommand)]
    pub command: ListCommand,
}

/// Estado a listar. Cada variante acepta `--limit`/`--offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum ListCommand {
    /// Tareas en estado READY.
    Ready(Pagination),
    /// Tareas en estado RUNNING.
    Running(Pagination),
    /// Tareas en estado SUCCESSFUL.
    Done(Pagination),
    /// Tareas en estado FAILED.
    ///
    /// Reusa la tabla genérica (`id`, `task_path`, `priority`, `enqueued_at`);
    /// el detalle del error queda para el futuro subcomando `inspect`.
    Failed(Pagination),
}

impl ListCommand {
    /// Estado en BD + paginación, para el despacho en `main.rs`.
    pub fn status_and_pagination(&self) -> (&'static str, Pagination) {
        match *self {
            ListCommand::Ready(pagination) => ("READY", pagination),
            ListCommand::Running(pagination) => ("RUNNING", pagination),
            ListCommand::Done(pagination) => ("SUCCESSFUL", pagination),
            ListCommand::Failed(pagination) => ("FAILED", pagination),
        }
    }

    /// Query Object validado para `db`: mapea la variante a `TaskStatus`
    /// y valida `limit`/`offset` en el dominio.
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

/// Paginación común: se mapea directo a `LIMIT`/`OFFSET` del query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct Pagination {
    /// Filas a devolver.
    #[arg(long, default_value_t = 25)]
    pub limit: i64,
    /// Filas a saltar.
    #[arg(long, default_value_t = 0)]
    pub offset: i64,
}

impl Pagination {
    /// Valida rangos antes de tocar la BD (la compartimos en OpalStack).
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

/// Límite superior de `--lines` en `log` (evita salidas gigantes).
pub const LOG_LINES_MAX: i64 = 10_000;

/// Argumentos de `log`: tail del log line-oriented del daemon (§3.10.6).
///
/// El contrato largo (follow, rotación, truncado, stdout/stderr) vive en
/// el doc del variante `Command::Log`, que es lo que `--help` muestra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct LogArgs {
    /// Líneas finales a mostrar (1-10000).
    #[arg(short = 'n', long, default_value_t = 10, allow_hyphen_values = true)]
    pub lines: i64,
    /// Sigue el archivo tras mostrar las últimas N líneas (poll ~500 ms).
    #[arg(short = 'f', long, action = clap::ArgAction::SetTrue)]
    pub follow: bool,
}

/// Valida `--lines`: entero en 1..=10000. Mensaje listo para stderr.
pub fn validate_log_lines(n: i64) -> Result<u64, String> {
    if (1..=LOG_LINES_MAX).contains(&n) {
        Ok(n as u64)
    } else {
        Err(format!(
            "--lines debe estar entre 1 y {LOG_LINES_MAX}, got {n}"
        ))
    }
}

/// Argumentos de `inspect`: el id de la fila a mostrar.
/// Por defecto la PII sale recortada; `--show-pii` es solo para operadores.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InspectArgs {
    /// Id (UUID) de la tarea en `django_tasks_database_dbtaskresult`.
    #[arg(value_name = "UUID")]
    pub id: String,
    /// Muestra la PII completa (`to`, cuerpos y `traceback` sin recortar).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub show_pii: bool,
}

/// Caracteres visibles de `subject` en el modo recortado (defecto).
pub const INSPECT_SUBJECT_MAX_CHARS: usize = 80;
/// Caracteres visibles de `traceback` en el modo recortado (defecto).
pub const INSPECT_TRACEBACK_MAX_CHARS: usize = 1000;
/// Caracteres visibles de un `args_kwargs` no-JSON en el modo recortado.
pub const INSPECT_RAW_MAX_CHARS: usize = 1000;

/// Trunca a N caracteres (sin partir UTF-8); si recorta, avisa cómo ver todo.
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let truncated: String = value.chars().take(max).collect();
    format!("{truncated}… [truncado, usa --show-pii]")
}

/// Enmascara un email mostrando solo el dominio (`***@dominio`); sin
/// arroba válida devuelve `[redacted]`.
fn mask_email(value: &str) -> String {
    let single_at = value.chars().filter(|&c| c == '@').count() == 1;
    match value.rsplit_once('@') {
        Some((_, domain)) if single_at && !domain.is_empty() => format!("***@{domain}"),
        _ => "[redacted]".to_owned(),
    }
}

/// Recorta la PII de `args_kwargs` para el modo por defecto de `inspect`:
/// `to` muestra solo el dominio, `subject` se trunca y los cuerpos se
/// sustituyen por presencia (`[presente: N chars]`). Vale tanto para el
/// formato real (`{"args", "kwargs"}`) como para un objeto plano; el resto
/// de claves se conserva para operar. Si no es JSON, trunca el texto.
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

/// Renderiza el detalle de `inspect` como líneas `clave: valor`.
///
/// Por defecto (`show_pii = false`) recorta la PII: `to` muestra solo el
/// dominio, los cuerpos aparecen como presencia y `subject`/`traceback` se
/// truncan. Con `show_pii = true` (flag `--show-pii`, solo operadores) sale
/// todo: `traceback` completo y `args_kwargs` reindentado como JSON.
/// Los `None` se muestran como `-`.
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

/// Imprime el detalle de `inspect` en stdout.
pub fn print_inspected_task(task: &InspectedTask, show_pii: bool) {
    print!("{}", render_inspected_task(task, show_pii));
}

/// Reindenta `raw` si es JSON válido; si no, devuelve el texto tal cual.
fn pretty_json_or_raw(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_owned()),
        Err(_) => raw.to_owned(),
    }
}

/// Detecta el error de Postgres cuando el id no es un UUID válido.
pub fn is_uuid_syntax_error(db_message: &str) -> bool {
    db_message.contains("invalid input syntax for type uuid")
}

/// Mensaje limpio (estilo uso) para un id que no es UUID.
pub fn render_inspect_uuid_error(id: &str) -> String {
    format!("id inválido: '{id}' no es un UUID válido; uso: inspect <UUID>")
}

/// Mapea un error de BD de `inspect` a mensaje para el usuario:
/// UUID inválido => mensaje limpio; resto => error tal cual.
pub fn format_inspect_error(id: &str, db_message: &str) -> String {
    if is_uuid_syntax_error(db_message) {
        render_inspect_uuid_error(id)
    } else {
        format!("error al inspeccionar la tarea {id}: {db_message}")
    }
}
///
/// Separa el render del `println!` para poder probarlo sin capturar stdout.
/// Renderiza la tabla de `status` como texto.
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

/// Imprime la tabla de `status` en stdout.
pub fn print_status_table(status: &QueueStatus) {
    println!("{}", render_status_table(status));
}

/// Estado del daemon para la sección `daemon:` de `status`.
///
/// `Running` lleva los datos best-effort ya resueltos (`None` = `/proc`
/// ilegible => se pinta `n/a`, sin fallar).
pub enum DaemonInfo {
    /// Sin `PILOTWAVE_PID_FILE`: el daemon nunca pudo configurarse.
    NotConfigured,
    /// Hay pidfile pero sin lock vivo (falta, stale o desbloqueado).
    NotRunning,
    /// Lock tomado por un proceso con identidad válida.
    Running {
        pid: u32,
        uptime: Option<u64>,
        rss_kb: Option<u64>,
    },
}

/// Renderiza la sección daemon de `status` (una línea).
///
/// Separa el render del `println!` para poder probarlo sin capturar stdout.
/// Ejemplos:
/// - `daemon: running pid=1234 uptime=1h2m3s rss=4192 kB (4.1 MB)`
/// - `daemon: running pid=1234 uptime=n/a rss=n/a` (fallo best-effort)
/// - `daemon: no corriendo (pidfile stale/unlocked)`
/// - `daemon: no configurado (PILOTWAVE_PID_FILE ausente)`
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

/// Renderiza la línea `heartbeat:` de `status` desde la fila BD (§3.10).
///
/// `metadata` se parsea best-effort: si falta o trae basura, los campos
/// pintan `n/a`/`-` en vez de fallar (el heartbeat nunca rompe `status`).
/// Ejemplos:
/// - `heartbeat: fresh last_seen=2026-11-19T12:00:00Z (hace 3s) tasks_processed=42 pid=1234 uptime=1h2m3s rss=4192 kB (4.1 MB) last_task=… last_error=-`
/// - `heartbeat: stale …` (misma forma, el worker murió o se atascó)
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

/// Mensaje de `stop` cuando no hay daemon vivo.
///
/// Idempotente (apto para cron): pidfile ausente, stale o desbloqueado, o
/// el PID ya muerto => exit 0 con esta línea en stdout.
pub fn render_stop_not_running() -> String {
    "daemon: no corriendo".to_owned()
}

/// Mensaje de éxito de `stop` tras matar al daemon.
///
/// `forced` distingue el camino: `false` = bastó SIGTERM, `true` = hizo
/// falta SIGKILL. Separa el render del `println!` para probarlo sin
/// capturar stdout.
pub fn render_stop_done(pid: u32, forced: bool) -> String {
    if forced {
        format!("daemon detenido pid={pid} (SIGKILL)")
    } else {
        format!("daemon detenido pid={pid}")
    }
}

/// Renderiza la tabla de `list <estado>` como texto.
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

/// Imprime la tabla de `list <estado>` en stdout.
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
        // inspect con UUID válido (por defecto recorta la PII).
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
            // Global: también vale después del subcomando y en log/stop.
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
        // done con paginación explícita.
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
        // failed mapea a FAILED y valida límites.
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
        // Sin arroba válida => [redacted]; objeto plano (sin `kwargs`) también
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
        // UUID v4 válido + error ajeno: pasa tal cual, no mensaje limpio.
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
