//! Runtime configuration.
//!
//! The sidecar parses `repo/.env` itself, but **only**
//! copies the variables it needs. We deliberately avoid `dotenvy::dotenv()`,
//! which would load the whole file (SECRET_KEY, IMAP_*, TURNSTILE_*, ...) into
//! the process environment and therefore into `/proc/<pid>/environ`.
//!
//! Precedence: real environment variable > `.env` value > default.

use std::path::PathBuf;
use std::time::Duration;

/// The `.env` file, parsed into memory but never exported to the environment.
pub struct Env {
    values: Vec<(String, String)>,
    /// Read the real process environment before `.env` values.
    /// Production: `true` (real env > `.env` > default).
    /// Tests: `false` (hermetic: `cargo test` runs threads in parallel and
    /// `just test` exports `.env` via `dotenv-load`; otherwise the real
    /// `.env` would win over in-memory values and tests would stop being
    /// deterministic).
    read_process_env: bool,
}

impl Env {
    /// Parse the nearest `.env` (walking up from the current directory).
    /// A missing file is fine: production may inject real env vars instead.
    pub fn load() -> Result<Self, String> {
        let values = match dotenvy::dotenv_iter() {
            Ok(iter) => iter
                .map(|item| item.map_err(|err| format!("invalid .env entry: {err}")))
                .collect::<Result<Vec<_>, _>>()?,
            Err(_) => Vec::new(),
        };
        Ok(Self {
            values,
            read_process_env: true,
        })
    }

    fn get(&self, name: &str) -> Option<String> {
        if self.read_process_env
            && let Ok(value) = std::env::var(name)
        {
            return Some(value);
        }
        // Last declaration wins, like dotenv's own semantics.
        self.values
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn required(&self, name: &str) -> Result<String, String> {
        self.get(name)
            .ok_or_else(|| format!("missing required env var {name}"))
    }

    fn optional(&self, name: &str, default: &str) -> String {
        self.get(name).unwrap_or_else(|| default.to_owned())
    }

    fn u64(&self, name: &str, default: u64) -> Result<u64, String> {
        match self.get(name) {
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| format!("{name} must be a non-negative integer, got {raw:?}")),
            None => Ok(default),
        }
    }

    /// `django-environ`-style boolean: case-insensitive.
    fn boolean(&self, name: &str, default: bool) -> Result<bool, String> {
        match self.get(name) {
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "y" | "on" => Ok(true),
                "false" | "0" | "no" | "n" | "off" => Ok(false),
                _ => Err(format!("{name} must be a boolean, got {raw:?}")),
            },
            None => Ok(default),
        }
    }
}

/// Everything the worker needs to run.
///
/// `smtp_password` is redacted in `Debug`: neither logs nor panics expose it.
pub struct Config {
    /// Least-privilege DSN (`aiph_worker` role), never the app's `DATABASE_URL`.
    pub database_url: String,
    /// Read-only DSN for the CLI (`status`/`list`/`inspect`/`is-alive`, see roadmap R-7).
    /// `PILOTWAVE_DATABASE_URL_RO`; absent/empty falls back to `database_url`.
    pub database_url_ro: Option<String>,
    /// Value appended to `worker_ids` on every claim; `[A-Za-z0-9_-]{1,64}`.
    pub worker_id: String,
    pub queue_name: String,
    pub backend_name: String,
    /// Task paths the worker is allowed to claim (allowlist).
    pub task_paths: Vec<String>,
    pub poll_min: Duration,
    pub poll_max: Duration,
    /// Per-send SMTP ceiling (`TASK_TIMEOUT_SECS`).
    pub task_timeout: Duration,
    /// `len(worker_ids)` cap: past this point a transient becomes `FAILED`.
    pub max_attempts: usize,
    /// Self-recycling: exit 75 after processing N tasks.
    /// `PILOTWAVE_RECYCLE_N`, default 5000.
    pub recycle_n: u64,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_use_tls: bool,
    /// `None` = no AUTH (only allowed loopback + plaintext opt-ins, dev).
    pub smtp_user: Option<String>,
    /// `None` = no AUTH. Redacted in `Debug` either way.
    pub smtp_password: Option<String>,
    /// Allowed prefixes for attachments. Fail-closed: without
    /// `PILOTWAVE_ATTACHMENT_ROOTS` the list is empty and every file send
    /// is rejected. No `MEDIA_ROOT` fallback (not even in dev):
    /// wiring explicit roots is the Developer/Integrator's responsibility.
    pub attachment_roots: Vec<PathBuf>,
    /// Sender identity; parsed at startup so a bad value fails loud.
    pub from: lettre::message::Mailbox,
    /// Skip the least-privilege check at startup
    /// (`PILOTWAVE_SKIP_GRANT_CHECK`, default `false`). Only for ad-hoc
    /// probes; keep the check enabled in production (see roadmap R-6).
    pub skip_grant_check: bool,
    /// PID file of the `--bg` daemon. Only `--bg`/`--bg-child` require it;
    /// other subcommands ignore it. Must be an absolute path.
    pub pid_file: Option<PathBuf>,
    /// Log file of the `--bg` daemon (child stdout+stderr, append). Same as above.
    pub log_file: Option<PathBuf>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"<redacted>")
            .field(
                "database_url_ro",
                &self.database_url_ro.as_ref().map(|_| "<redacted>"),
            )
            .field("worker_id", &self.worker_id)
            .field("queue_name", &self.queue_name)
            .field("backend_name", &self.backend_name)
            .field("task_paths", &self.task_paths)
            .field("poll_min", &self.poll_min)
            .field("poll_max", &self.poll_max)
            .field("task_timeout", &self.task_timeout)
            .field("max_attempts", &self.max_attempts)
            .field("recycle_n", &self.recycle_n)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_use_tls", &self.smtp_use_tls)
            .field("smtp_user", &smtp_user_display(&self.smtp_user))
            .field("smtp_password", &"<redacted>")
            .field("attachment_roots", &self.attachment_roots)
            .field("from", &self.from)
            .field("skip_grant_check", &self.skip_grant_check)
            .field("pid_file", &self.pid_file)
            .field("log_file", &self.log_file)
            .finish()
    }
}

impl Config {
    /// DSN for the read-only subcommands (`status`/`list`/`inspect`/`is-alive`,
    /// roadmap R-7): the reader role when configured, else the worker DSN.
    pub fn read_database_url(&self) -> &str {
        self.database_url_ro
            .as_deref()
            .unwrap_or(&self.database_url)
    }

    pub fn from_env(env: &Env) -> Result<Self, String> {
        let database_url = env.required("PILOTWAVE_DATABASE_URL")?;
        // Optional: empty or absent => `None` (the CLI reuses `database_url`).
        let database_url_ro = env
            .get("PILOTWAVE_DATABASE_URL_RO")
            .filter(|v| !v.trim().is_empty());
        // Security: require `sslmode=require` outside loopback. Loopback
        // (localhost/dev) is allowed with an explicit WARNING; anything
        // else fails loud so credentials never travel in cleartext.
        if !dsn_requires_ssl(&database_url) {
            if dsn_host_is_loopback(&database_url) {
                eprintln!(
                    "WARNING pilotwave-worker: PILOTWAVE_DATABASE_URL lacks sslmode=require; \
                     allowed only because the host is loopback (dev/localhost). \
                     Production DSNs must append ?sslmode=require."
                );
            } else {
                return Err(
                    "PILOTWAVE_DATABASE_URL must include sslmode=require outside localhost \
                     (A02: credentials would travel in cleartext)"
                        .into(),
                );
            }
        }
        let queue_name = env.optional("PILOTWAVE_QUEUE_NAME", "pilotwave.email");
        let backend_name = env.optional("PILOTWAVE_BACKEND_NAME", "default");

        let task_paths = env
            .optional("PILOTWAVE_TASK_PATHS", "pilotwave.tasks.mail_notification")
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if task_paths.is_empty() {
            return Err("PILOTWAVE_TASK_PATHS must list at least one task path".into());
        }

        // A stable, safe id: it is written into jsonb and into logs, so no
        // quotes, spaces or slashes are allowed.
        let worker_id = env
            .get("PILOTWAVE_ID")
            .unwrap_or_else(|| format!("pilotwave-{}", std::process::id()));
        if worker_id.is_empty()
            || worker_id.len() > 64
            || !worker_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err("PILOTWAVE_ID must match [A-Za-z0-9_-]{1,64}".into());
        }

        let poll_min = Duration::from_millis(env.u64("PILOTWAVE_POLL_MIN_MS", 1_500)?);
        let poll_max = Duration::from_millis(env.u64("PILOTWAVE_POLL_MAX_MS", 5_000)?);
        if poll_min > poll_max {
            return Err("PILOTWAVE_POLL_MIN_MS must be <= PILOTWAVE_POLL_MAX_MS".into());
        }
        let task_timeout = Duration::from_secs(env.u64("PILOTWAVE_TASK_TIMEOUT_SECS", 30)?);
        let max_attempts = env.u64("PILOTWAVE_MAX_ATTEMPTS", 5)? as usize;
        if max_attempts == 0 {
            return Err("PILOTWAVE_MAX_ATTEMPTS must be >= 1".into());
        }
        let recycle_n = env.u64("PILOTWAVE_RECYCLE_N", 5_000)?;
        if recycle_n == 0 {
            return Err("PILOTWAVE_RECYCLE_N must be >= 1".into());
        }

        let smtp_host = env.required("EMAIL_HOST")?.trim().to_owned();
        if smtp_host.is_empty() {
            return Err("EMAIL_HOST must not be empty".into());
        }
        let smtp_port = match env.get("EMAIL_PORT") {
            Some(raw) => raw
                .parse::<u16>()
                .map_err(|_| format!("EMAIL_PORT must be a TCP port (1-65535), got {raw:?}"))?,
            None => 587,
        };
        if smtp_port == 0 {
            return Err("EMAIL_PORT must be a TCP port (1-65535), got 0".into());
        }
        let smtp_use_tls = env.boolean("EMAIL_USE_TLS", true)?;
        // Plaintext SMTP only with explicit opt-in; otherwise fail loud:
        // without `PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true` fail startup
        // (SMTP credentials would travel in cleartext). Dev-only, never prod.
        if !smtp_use_tls {
            if !env.boolean("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", false)? {
                return Err("EMAIL_USE_TLS=false sends SMTP credentials in cleartext; \
                     set PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true to acknowledge \
                     (dev-only, never in production)"
                    .into());
            }
            eprintln!(
                "WARNING pilotwave-worker: EMAIL_USE_TLS=false with \
                 PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true; SMTP credentials travel \
                 in cleartext (dev-only, never in production)."
            );
        }
        let smtp_user = env
            .get("EMAIL_HOST_USER")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let smtp_password = env
            .get("EMAIL_HOST_PASSWORD")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        match (&smtp_user, &smtp_password) {
            (Some(_), Some(_)) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "EMAIL_HOST_USER and EMAIL_HOST_PASSWORD must both be set or both \
                    be absent (got only one)"
                        .into(),
                );
            }
            (None, None) => {
                // No AUTH: only loopback + plaintext, both explicitly opted in.
                if !smtp_host_is_loopback(&smtp_host) {
                    return Err(
                        "SMTP without credentials is only allowed against a loopback host \
                        (localhost/127.*/::1) with EMAIL_USE_TLS=false, \
                        PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true and \
                        PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true"
                            .into(),
                    );
                }
                if smtp_use_tls {
                    return Err(
                        "SMTP without credentials requires EMAIL_USE_TLS=false with \
                        PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true and \
                        PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true (dev/loopback only)"
                            .into(),
                    );
                }
                if !env.boolean("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", false)? {
                    return Err("SMTP without credentials requires \
                        PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true (dev/loopback only, \
                        with EMAIL_USE_TLS=false and PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true)"
                        .into());
                }
                eprintln!(
                    "WARNING pilotwave-worker: SMTP without AUTH against {smtp_host} \
                     (PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true, dev/loopback only)."
                );
            }
        }
        // Comma-separated list from `PILOTWAVE_ATTACHMENT_ROOTS`. No
        // `MEDIA_ROOT` fallback, not even in dev: empty means deny-all
        // (fail-closed). Wiring explicit roots is the
        // Developer/Integrator's responsibility (see install docs). Not
        // canonicalized here: directories may not exist in tests; the
        // attachment loader canonicalizes `root` + candidate before
        // comparing prefixes.
        let attachment_roots: Vec<PathBuf> = match env.get("PILOTWAVE_ATTACHMENT_ROOTS") {
            Some(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            None => Vec::new(),
        };
        if attachment_roots.is_empty() {
            eprintln!(
                "WARNING pilotwave-worker: PILOTWAVE_ATTACHMENT_ROOTS is missing or empty; \
                 attachment sends will fail closed (path_policy_violation). \
                 Wiring explicit absolute roots is the Developer/Integrator's responsibility."
            );
        }
        // A relative root would resolve against the process cwd: any later
        // `starts_with` check would silently deny-all or, worse, depend on
        // the cwd. Fail loud at startup.
        if attachment_roots.iter().any(|r| !r.is_absolute()) {
            return Err("PILOTWAVE_ATTACHMENT_ROOTS must be absolute paths".into());
        }
        let from = env
            .required("DEFAULT_FROM_EMAIL")?
            .parse::<lettre::message::Mailbox>()
            .map_err(|err| format!("DEFAULT_FROM_EMAIL must parse as a mailbox, got: {err}"))?;
        // Only `--bg`/`--bg-child` require them (via `daemon::paths`): the
        // rest of the subcommands work without them. Absolute or nothing.
        let pid_file = optional_absolute(env, "PILOTWAVE_PID_FILE")?;
        let log_file = optional_absolute(env, "PILOTWAVE_LOG_FILE")?;
        let skip_grant_check = env.boolean("PILOTWAVE_SKIP_GRANT_CHECK", false)?;

        Ok(Self {
            database_url,
            database_url_ro,
            worker_id,
            queue_name,
            backend_name,
            task_paths,
            poll_min,
            poll_max,
            task_timeout,
            max_attempts,
            recycle_n,
            smtp_host,
            smtp_port,
            smtp_use_tls,
            smtp_user,
            smtp_password,
            attachment_roots,
            from,
            skip_grant_check,
            pid_file,
            log_file,
        })
    }
}

/// `true` when the DSN query carries `sslmode=require` (case-insensitive).
fn dsn_requires_ssl(url: &str) -> bool {
    let query = match url.split_once('?') {
        Some((_, query)) => query,
        None => return false,
    };
    query.split('&').any(|pair| {
        let mut parts = pair.splitn(2, '=');
        matches!(parts.next(), Some(k) if k.eq_ignore_ascii_case("sslmode"))
            && matches!(parts.next(), Some(v) if v.eq_ignore_ascii_case("require"))
    })
}

/// DSN host (no userinfo, no port) for the loopback bypass.
fn dsn_host(url: &str) -> &str {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = after_scheme.rsplit('@').next().unwrap_or(after_scheme);
    let hostport = authority.split(['/', '?']).next().unwrap_or("");
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => hostport,
    }
}

/// Loopback only: the single no-TLS case (dev).
fn dsn_host_is_loopback(url: &str) -> bool {
    let host = dsn_host(url).to_ascii_lowercase();
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// SMTP host loopback check for the no-AUTH case (dev, Mailcrab).
fn smtp_host_is_loopback(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    let bare = lower
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&lower);
    if bare == "localhost" || bare == "::1" {
        return true;
    }
    // 127.0.0.0/8, digits and dots only (no DNS suffixes).
    bare.strip_prefix("127.").is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    })
}

/// Never leak the SMTP user: redacted when set, explicit marker when absent.
fn smtp_user_display(user: &Option<String>) -> &'static str {
    if user.is_some() {
        "<redacted>"
    } else {
        "(no-auth)"
    }
}

/// Optional absolute-path variable: absent/empty => `None`;
/// present but relative => loud error (fail-closed).
fn optional_absolute(env: &Env, name: &str) -> Result<Option<PathBuf>, String> {
    match env.get(name) {
        None => Ok(None),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => {
            let path = PathBuf::from(raw.trim());
            if !path.is_absolute() {
                return Err(format!("{name} must be an absolute path"));
            }
            Ok(Some(path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid in-memory `Env` (never touches the real environment).
    fn base_vars() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PILOTWAVE_DATABASE_URL", "postgres://u:p@127.0.0.1/db"),
            ("PILOTWAVE_TASK_PATHS", "pilotwave.tasks.mail_notification"),
            ("EMAIL_HOST", "smtp.example.com"),
            ("EMAIL_HOST_USER", "user"),
            ("EMAIL_HOST_PASSWORD", "secret"),
            ("DEFAULT_FROM_EMAIL", "CRM <crm@example.com>"),
        ]
    }

    /// In-memory `Env`: hermetic, never reads the real process environment
    /// (`read_process_env = false`). Tests run in parallel threads and
    /// `set_var`/`remove_var` are not thread-safe; plus `just test` exports
    /// the real `.env` via `dotenv-load`, which would otherwise win over
    /// these values and make tests non-deterministic.
    fn test_env(extra: &[(&str, &str)]) -> Env {
        let values = base_vars()
            .into_iter()
            .chain(extra.iter().copied())
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Env {
            values,
            read_process_env: false,
        }
    }

    fn config_with(extra: &[(&str, &str)]) -> Result<Config, String> {
        Config::from_env(&test_env(extra))
    }

    #[test]
    fn defaults_smtp_timeout_from_y_sin_roots() {
        let cfg = config_with(&[]).unwrap();
        assert_eq!(cfg.smtp_port, 587);
        assert!(cfg.smtp_use_tls);
        assert_eq!(cfg.smtp_host, "smtp.example.com");
        assert_eq!(cfg.task_timeout, Duration::from_secs(30));
        assert_eq!(cfg.from.email.to_string(), "crm@example.com");
        // Fail-closed: with no vars set, the list is empty (denies everything).
        assert!(cfg.attachment_roots.is_empty());
    }

    #[test]
    fn boolean_parses_django_environ_variants() {
        for (raw, expected) in [
            ("True", true),
            ("true", true),
            ("1", true),
            ("yes", true),
            ("on", true),
            ("False", false),
            ("0", false),
            ("no", false),
            ("off", false),
        ] {
            assert_eq!(
                test_env(&[("PILOTWAVE_B", raw)])
                    .boolean("PILOTWAVE_B", false)
                    .unwrap(),
                expected
            );
        }
        assert!(
            test_env(&[("PILOTWAVE_B", "quizas")])
                .boolean("PILOTWAVE_B", true)
                .is_err()
        );
    }

    #[test]
    fn requeridos_y_rangos_fallan_alto() {
        // Malformed FROM.
        assert!(config_with(&[("DEFAULT_FROM_EMAIL", "not-an-email@@")]).is_err());
        // Missing password.
        let values = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        assert!(
            Config::from_env(&Env {
                values,
                read_process_env: false
            })
            .is_err()
        );
        // Zero or garbage port.
        assert!(config_with(&[("EMAIL_PORT", "0")]).is_err());
        assert!(config_with(&[("EMAIL_PORT", "xx")]).is_err());
        // Poll min > max.
        assert!(
            config_with(&[
                ("PILOTWAVE_POLL_MIN_MS", "5000"),
                ("PILOTWAVE_POLL_MAX_MS", "1000"),
            ])
            .is_err()
        );
    }

    #[test]
    fn intentos_y_reciclaje_defaults_y_rechazan_cero() {
        assert_eq!(config_with(&[]).unwrap().max_attempts, 5);
        assert_eq!(
            config_with(&[("PILOTWAVE_MAX_ATTEMPTS", "3")])
                .unwrap()
                .max_attempts,
            3
        );
        assert!(config_with(&[("PILOTWAVE_MAX_ATTEMPTS", "0")]).is_err());
        assert!(config_with(&[("PILOTWAVE_MAX_ATTEMPTS", "xx")]).is_err());
        assert_eq!(config_with(&[]).unwrap().recycle_n, 5_000);
        assert_eq!(
            config_with(&[("PILOTWAVE_RECYCLE_N", "25")])
                .unwrap()
                .recycle_n,
            25
        );
        assert!(config_with(&[("PILOTWAVE_RECYCLE_N", "0")]).is_err());
        assert!(config_with(&[("PILOTWAVE_RECYCLE_N", "xx")]).is_err());
    }

    #[test]
    fn debug_redacts_secrets() {
        let shown = format!("{:?}", config_with(&[]).unwrap());
        assert!(!shown.contains("secret"));
        assert!(!shown.contains("postgres://u:p@"));
        assert!(shown.contains("smtp.example.com"));
    }

    #[test]
    fn ro_opcional_para_cli_solo_lectura() {
        // Absent => None and the CLI reuses the worker DSN.
        let cfg = config_with(&[]).unwrap();
        assert_eq!(cfg.database_url_ro, None);
        assert_eq!(cfg.read_database_url(), cfg.database_url.as_str());
        // Empty => None (no error).
        let cfg = config_with(&[("PILOTWAVE_DATABASE_URL_RO", "  ")]).unwrap();
        assert_eq!(cfg.database_url_ro, None);
        // Present => the read-only CLI uses the reader role.
        let cfg = config_with(&[(
            "PILOTWAVE_DATABASE_URL_RO",
            "postgres://reader:lector@127.0.0.1/db",
        )])
        .unwrap();
        assert_eq!(
            cfg.database_url_ro.as_deref(),
            Some("postgres://reader:lector@127.0.0.1/db")
        );
        assert_eq!(
            cfg.read_database_url(),
            "postgres://reader:lector@127.0.0.1/db"
        );
        // The reader DSN never shows up in Debug either.
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("lector"));
    }

    #[test]
    fn attachment_roots_sin_fallback_y_relativas_fallan() {
        // Without the var: empty = deny-all (fail-closed). No MEDIA_ROOT
        // fallback, not even in dev: wiring explicit roots is the
        // Developer/Integrator's responsibility.
        assert!(config_with(&[]).unwrap().attachment_roots.is_empty());
        assert!(
            config_with(&[("MEDIA_ROOT", "/srv/media")])
                .unwrap()
                .attachment_roots
                .is_empty()
        );
        // Explicit list.
        let cfg = config_with(&[("PILOTWAVE_ATTACHMENT_ROOTS", "/a, /b ,")]).unwrap();
        assert_eq!(
            cfg.attachment_roots,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        // Relative paths: loud failure.
        assert!(config_with(&[("PILOTWAVE_ATTACHMENT_ROOTS", "media/x")]).is_err());
    }

    #[test]
    fn dsn_sin_sslmode_solo_loopback_con_warning() {
        // Loopback without sslmode: passes (dev bypass with WARNING on stderr).
        assert!(config_with(&[]).is_ok());
        // Remote without sslmode: loud failure.
        assert!(
            config_with(&[("PILOTWAVE_DATABASE_URL", "postgres://u:p@db.example.com/db")]).is_err()
        );
        // Remote with sslmode=require: passes (also `&` and uppercase variants).
        assert!(
            config_with(&[(
                "PILOTWAVE_DATABASE_URL",
                "postgres://u:p@db.example.com/db?sslmode=require"
            )])
            .is_ok()
        );
        assert!(
            config_with(&[(
                "PILOTWAVE_DATABASE_URL",
                "postgres://u:p@db.example.com:5432/db?connect_timeout=5&sslmode=require"
            )])
            .is_ok()
        );
        // Remote `sslmode=disable`: loud failure (not `require`).
        assert!(
            config_with(&[(
                "PILOTWAVE_DATABASE_URL",
                "postgres://u:p@db.example.com/db?sslmode=disable"
            )])
            .is_err()
        );
        // Loopback with port and IPv6: the bypass still applies.
        assert!(
            config_with(&[("PILOTWAVE_DATABASE_URL", "postgres://u:p@localhost:5432/db")]).is_ok()
        );
        assert!(config_with(&[("PILOTWAVE_DATABASE_URL", "postgres://u:p@[::1]/db")]).is_ok());
    }

    #[test]
    fn smtp_sin_tls_exige_opt_in_explicito() {
        // No TLS and no opt-in: loud failure (plaintext was never silent).
        assert!(config_with(&[("EMAIL_USE_TLS", "false")]).is_err());
        // With explicit opt-in: passes but without TLS (dev-only).
        let cfg = config_with(&[
            ("EMAIL_USE_TLS", "false"),
            ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
        ])
        .unwrap();
        assert!(!cfg.smtp_use_tls);
        // TLS stays the default.
        assert!(config_with(&[]).unwrap().smtp_use_tls);
    }

    /// `Env` without SMTP credentials (both absent), plus `extra` overrides.
    fn config_sin_auth(extra: &[(&str, &str)]) -> Result<Config, String> {
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_USER" && *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        Config::from_env(&Env {
            values,
            read_process_env: false,
        })
    }

    /// Loopback dev combo: no AUTH, no TLS, both opt-ins.
    fn loopback_sin_auth(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = vec![
            ("EMAIL_HOST", "localhost"),
            ("EMAIL_USE_TLS", "false"),
            ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
            ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "true"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        out.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        out
    }

    #[test]
    fn smtp_con_auth_ok_y_sin_auth_loopback_ok() {
        // With AUTH (both set): ok, values kept.
        let cfg = config_with(&[]).unwrap();
        assert_eq!(cfg.smtp_user.as_deref(), Some("user"));
        assert_eq!(cfg.smtp_password.as_deref(), Some("secret"));
        // No AUTH + loopback + both opt-ins: ok, both None.
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_USER" && *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(loopback_sin_auth(&[]));
        let cfg = Config::from_env(&Env {
            values,
            read_process_env: false,
        })
        .unwrap();
        assert_eq!(cfg.smtp_user, None);
        assert_eq!(cfg.smtp_password, None);
    }

    #[test]
    fn smtp_solo_uno_falla_alto() {
        // Only user, no password: fatal (remote host, TLS on).
        assert!(config_sin_auth(&[("EMAIL_HOST_USER", "solo-uno")]).is_err());
        // Only password, no user: fatal.
        assert!(config_sin_auth(&[("EMAIL_HOST_PASSWORD", "solo-una")]).is_err());
        // Same on loopback without TLS: still fatal (half credentials).
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(loopback_sin_auth(&[]));
        assert!(
            Config::from_env(&Env {
                values,
                read_process_env: false
            })
            .is_err()
        );
    }

    #[test]
    fn smtp_sin_auth_remoto_o_con_tls_falla() {
        // Remote host without AUTH: fatal even with both opt-ins.
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "smtp.example.com"),
                ("EMAIL_USE_TLS", "false"),
                ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
                ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "true"),
            ])
            .is_err()
        );
        // Loopback without AUTH but TLS on: fatal.
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "localhost"),
                ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "true"),
            ])
            .is_err()
        );
        // Loopback + plaintext but anonymous opt-in missing: fatal.
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "localhost"),
                ("EMAIL_USE_TLS", "false"),
                ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
            ])
            .is_err()
        );
        // Loopback + anonymous but plaintext opt-in missing: fatal.
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "localhost"),
                ("EMAIL_USE_TLS", "false"),
                ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "true"),
            ])
            .is_err()
        );
    }

    #[test]
    fn smtp_credenciales_en_blanco_equivalen_a_ausentes() {
        // Whitespace-only on both sides == absent: loopback combo passes.
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_USER" && *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(loopback_sin_auth(&[
            ("EMAIL_HOST_USER", "   "),
            ("EMAIL_HOST_PASSWORD", "\t "),
        ]));
        let cfg = Config::from_env(&Env {
            values,
            read_process_env: false,
        })
        .unwrap();
        assert_eq!(cfg.smtp_user, None);
        // Whitespace user + real password: half credentials, fatal.
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST_USER", "   "),
                ("EMAIL_HOST_PASSWORD", "secreta"),
            ])
            .is_err()
        );
    }

    #[test]
    fn smtp_host_loopback_acepta_variantes() {
        assert!(smtp_host_is_loopback("localhost"));
        assert!(smtp_host_is_loopback("LOCALHOST"));
        assert!(smtp_host_is_loopback("  localhost  "));
        assert!(smtp_host_is_loopback("127.0.0.1"));
        assert!(smtp_host_is_loopback("127.1.2.3"));
        assert!(smtp_host_is_loopback("::1"));
        assert!(smtp_host_is_loopback("[::1]"));
        assert!(smtp_host_is_loopback("[127.0.0.1]"));
        assert!(!smtp_host_is_loopback("smtp.example.com"));
        assert!(!smtp_host_is_loopback("127.0.0.1.evil.com"));
        assert!(!smtp_host_is_loopback(""));
        // Bordes: puerto/scheme/sufijos no son loopback puro.
        assert!(!smtp_host_is_loopback("localhost:1025"));
        assert!(!smtp_host_is_loopback("smtp://localhost"));
        assert!(!smtp_host_is_loopback("localhost."));
        assert!(!smtp_host_is_loopback("127."));
        assert!(!smtp_host_is_loopback("127.a"));
        assert!(!smtp_host_is_loopback("0.0.0.0"));
    }

    #[test]
    fn smtp_sin_auth_borde_bandera_y_puerto() {
        // Flag anónimo con valor no booleano: error (estilo django-environ).
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "localhost"),
                ("EMAIL_USE_TLS", "false"),
                ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
                ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "quizas"),
            ])
            .is_err()
        );
        // EMAIL_PORT con whitespace: error (no se hace trim, falla alto).
        assert!(
            config_sin_auth(&[
                ("EMAIL_HOST", "localhost"),
                ("EMAIL_PORT", " 1025 "),
                ("EMAIL_USE_TLS", "false"),
                ("PILOTWAVE_ALLOW_PLAINTEXT_SMTP", "true"),
                ("PILOTWAVE_ALLOW_ANONYMOUS_SMTP", "true"),
            ])
            .is_err()
        );
        // EMAIL_HOST con espacios alrededor hace trim y pasa en loopback.
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_USER" && *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(loopback_sin_auth(&[("EMAIL_HOST", "  localhost  ")]));
        let cfg = Config::from_env(&Env {
            values,
            read_process_env: false,
        })
        .unwrap();
        assert_eq!(cfg.smtp_host, "localhost");
        // EMAIL_HOST vacío o solo-blancos: error.
        assert!(config_sin_auth(&[("EMAIL_HOST", "   ")]).is_err());
    }

    #[test]
    fn debug_sin_auth_no_filtra_y_marca() {
        // With AUTH: user redacted, password never shown.
        let shown = format!("{:?}", config_with(&[]).unwrap());
        assert!(!shown.contains("secret"));
        assert!(!shown.contains("\"user\""));
        // Without AUTH: explicit marker, still no password.
        let mut values: Vec<(String, String)> = base_vars()
            .into_iter()
            .filter(|(k, _)| *k != "EMAIL_HOST_USER" && *k != "EMAIL_HOST_PASSWORD")
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        values.extend(loopback_sin_auth(&[]));
        let shown = format!(
            "{:?}",
            Config::from_env(&Env {
                values,
                read_process_env: false
            })
            .unwrap()
        );
        assert!(shown.contains("(no-auth)"));
        assert!(!shown.contains("secret"));
    }

    #[test]
    fn pid_y_log_son_opcionales_y_deben_ser_absolutos() {
        let cfg = config_with(&[]).unwrap();
        assert_eq!(cfg.pid_file, None);
        assert_eq!(cfg.log_file, None);
        let cfg = config_with(&[
            ("PILOTWAVE_PID_FILE", "/run/pilotwave-worker.pid"),
            ("PILOTWAVE_LOG_FILE", "/var/log/pilotwave-worker.log"),
        ])
        .unwrap();
        assert_eq!(
            cfg.pid_file,
            Some(PathBuf::from("/run/pilotwave-worker.pid"))
        );
        assert_eq!(
            cfg.log_file,
            Some(PathBuf::from("/var/log/pilotwave-worker.log"))
        );
        assert!(config_with(&[("PILOTWAVE_PID_FILE", "relativa.pid")]).is_err());
        assert!(config_with(&[("PILOTWAVE_LOG_FILE", "logs/x.log")]).is_err());
    }

    #[test]
    fn skip_grant_check_default_off_y_acepta_opt_in() {
        // The least-privilege check (roadmap R-6) is on by default.
        assert!(!config_with(&[]).unwrap().skip_grant_check);
        assert!(
            config_with(&[("PILOTWAVE_SKIP_GRANT_CHECK", "true")])
                .unwrap()
                .skip_grant_check
        );
        assert!(config_with(&[("PILOTWAVE_SKIP_GRANT_CHECK", "quizas")]).is_err());
    }
}
