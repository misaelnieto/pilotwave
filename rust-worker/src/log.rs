//! `log`: tail of `PILOTWAVE_LOG_FILE` (single line-oriented file).
//!
//! - Without `-f`: print the last N lines to stdout and exit.
//! - With `-f`: print the last N lines, then keep showing appends
//!   (poll every [`FOLLOW_POLL`]) until SIGTERM/SIGINT (exit 0).
//! - On rotation (different inode) or truncation (size < offset) warn on
//!   stderr and reopen from 0.
//! - Output is exact: lines verbatim, no numbering or prefixes.
//!   Stdout carries only log content; warnings go to stderr.
//! - Never touches the DB: only needs `config.log_file` (absolute path).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::LogArgs;
use crate::config::Config;

/// Append poll interval for `-f` (~500 ms).
pub const FOLLOW_POLL: Duration = Duration::from_millis(500);
/// Stderr warning on rotation or truncation (the log rotates rarely, but it
/// must be detected instead of hanging on a dead offset).
pub const ROTATION_NOTICE: &str = "rotación detectada: reabriendo desde el inicio";
/// Reverse-read block size for the tail (avoids loading the whole file).
const TAIL_BLOCK: u64 = 8 * 1024;
/// Cap per poll in `-f`: when a poll brings more, advance in chunks instead
/// of loading everything at once.
const FOLLOW_MAX_READ: u64 = 16 * 1024 * 1024;

/// Resolve the file to read: require an absolute `PILOTWAVE_LOG_FILE` and an
/// existing file (never created empty: when missing, fail loud with path and
/// cause, exit 1).
pub fn resolve_log_file(config: &Config) -> Result<PathBuf, String> {
    let path = config.log_file.clone().ok_or_else(|| {
        "PILOTWAVE_LOG_FILE no configurado: `log` necesita la ruta absoluta del log del daemon"
            .to_owned()
    })?;
    std::fs::metadata(&path).map_err(|err| open_failed(&path, "log file", &err))?;
    Ok(path)
}

/// Human-readable cause with path, operation and actionable hint.
fn open_failed(path: &Path, what: &str, err: &std::io::Error) -> String {
    use std::io::ErrorKind as Kind;
    let hint = match err.kind() {
        Kind::NotFound => "no existe (¿el daemon --bg llegó a arrancar?)",
        Kind::PermissionDenied => "sin permiso: revisa dueño y modo del archivo y sus directorios",
        Kind::IsADirectory => "es un directorio, no un archivo",
        _ => "fallo de E/S",
    };
    format!("{what} {}: {hint}: {err}", path.display())
}

/// Last `n` lines of `text` (pure helper, no I/O).
///
/// The testable core of the tail: the binary uses [`tail_file`] (bytes,
/// without loading the whole file) and this helper exposes the same semantics
/// over `&str` for tests and future renderers.
/// `n == 0` => empty; `n` larger than the total => everything. Excludes `\n`.
#[allow(dead_code)]
pub fn last_n_lines(text: &str, n: usize) -> Vec<&str> {
    if n == 0 {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    if n >= lines.len() {
        return lines;
    }
    lines[lines.len() - n..].to_vec()
}

/// Decide whether `-f` must reopen from 0: the inode changed (rotation) or
/// the size shrank below the offset (truncation). Pure function for testing
/// without I/O (sizes/inodes are injected).
pub fn needs_reopen(
    prev_len: u64,
    cur_len: u64,
    prev_ino: Option<u64>,
    cur_ino: Option<u64>,
) -> bool {
    cur_ino != prev_ino || cur_len < prev_len
}

/// Read the last `n` lines of the file (exact bytes, with their `\n`).
///
/// Scan backwards in 8 KB blocks counting `\n`, so the whole file is never
/// loaded into memory (the log rotates rarely but can grow).
/// `n == 0` => empty; fewer than `n` lines => the whole file.
pub fn tail_file(path: &Path, n: u64) -> Result<Vec<u8>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).map_err(|err| open_failed(path, "log file", &err))?;
    let len = file
        .metadata()
        .map_err(|err| open_failed(path, "log file (metadata)", &err))?
        .len();
    if len == 0 {
        return Ok(Vec::new());
    }
    // When the file ends in `\n`, that byte closes the last line and does
    // not count as an extra separator.
    let ends_with_nl = {
        file.seek(SeekFrom::End(-1))
            .map_err(|err| open_failed(path, "log file (seek)", &err))?;
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte)
            .map_err(|err| open_failed(path, "log file (read)", &err))?;
        byte[0] == b'\n'
    };
    let mut pos = len;
    let mut newlines = 0u64;
    let mut start = 0u64;
    let mut buf = vec![0u8; TAIL_BLOCK as usize];
    'outer: while pos > 0 {
        let take = pos.min(TAIL_BLOCK);
        pos -= take;
        file.seek(SeekFrom::Start(pos))
            .map_err(|err| open_failed(path, "log file (seek)", &err))?;
        let chunk = &mut buf[..take as usize];
        file.read_exact(chunk)
            .map_err(|err| open_failed(path, "log file (read)", &err))?;
        let mut i = chunk.len();
        while i > 0 {
            i -= 1;
            if chunk[i] == b'\n' && !(ends_with_nl && pos + i as u64 + 1 == len) {
                newlines += 1;
                if newlines == n {
                    start = pos + i as u64 + 1;
                    break 'outer;
                }
            }
        }
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|err| open_failed(path, "log file (seek)", &err))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .map_err(|err| open_failed(path, "log file (read)", &err))?;
    Ok(out)
}

/// Read the `[offset, len)` range of the file (one `-f` chunk).
fn read_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    let end = len.min(offset.saturating_add(FOLLOW_MAX_READ));
    let mut file = File::open(path).map_err(|err| open_failed(path, "log file", &err))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| open_failed(path, "log file (seek)", &err))?;
    let mut out = Vec::with_capacity((end - offset).min(64 * 1024) as usize);
    file.take(end - offset)
        .read_to_end(&mut out)
        .map_err(|err| open_failed(path, "log file (read)", &err))?;
    Ok(out)
}

/// Current size and inode (the inode detects rotation even when the size
/// matches). `None` when the platform gives no inode (then size alone
/// decides).
fn stat(path: &Path) -> Result<(u64, Option<u64>), String> {
    let meta = std::fs::metadata(path).map_err(|err| open_failed(path, "log file", &err))?;
    #[cfg(unix)]
    let ino = Some(std::os::unix::fs::MetadataExt::ino(&meta));
    #[cfg(not(unix))]
    let ino: Option<u64> = None;
    Ok((meta.len(), ino))
}

/// Write exact bytes to stdout (log content, no decorations).
fn write_stdout(data: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(data)?;
    out.flush()
}

/// `log`: tail of `PILOTWAVE_LOG_FILE`. Without `-f` print and exit; with
/// `-f` follow appends until SIGTERM/SIGINT (exit 0). Never touches the DB.
pub async fn run_log(config: &Config, args: &LogArgs) -> Result<(), Box<dyn std::error::Error>> {
    let fail = |msg: String| -> Box<dyn std::error::Error> {
        eprintln!("error: {msg}");
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
            as Box<dyn std::error::Error>
    };
    let n = match crate::cli::validate_log_lines(args.lines) {
        Ok(n) => n,
        Err(msg) => return Err(fail(msg)),
    };
    let path = match resolve_log_file(config) {
        Ok(path) => path,
        Err(msg) => return Err(fail(msg)),
    };
    let data = tail_file(&path, n).map_err(|msg| {
        eprintln!("error: {msg}");
        std::io::Error::other(msg)
    })?;
    write_stdout(&data)?;
    if !args.follow {
        return Ok(());
    }
    // The tail already showed everything old: the follow starts from the
    // current size and only brings what is new.
    let (mut offset, mut ino) = match stat(&path) {
        Ok(st) => st,
        // Unlikely race (just read): warn and restart from 0.
        Err(msg) => {
            eprintln!("aviso: {msg}");
            (0, None)
        }
    };
    loop {
        tokio::select! {
            _ = crate::worker::shutdown_signal() => break,
            _ = tokio::time::sleep(FOLLOW_POLL) => {}
        }
        let (len, cur_ino) = match stat(&path) {
            Ok(st) => st,
            Err(msg) => {
                eprintln!("aviso: {msg}");
                continue;
            }
        };
        if needs_reopen(offset, len, ino, cur_ino) {
            eprintln!("{ROTATION_NOTICE}");
            offset = 0;
            ino = cur_ino;
        }
        if len > offset {
            match read_range(&path, offset, len) {
                Ok(data) => {
                    offset += data.len() as u64;
                    if let Err(err) = write_stdout(&data) {
                        eprintln!("aviso: stdout: {err}");
                        break;
                    }
                    // Partial chunk (per-poll cap): the next poll brings
                    // the rest without reopening.
                }
                Err(msg) => eprintln!("aviso: {msg}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp file per test (no `tempfile`: std only).
    fn tmp_log(name: &str) -> PathBuf {
        let unique = format!(
            "pilotwave-worker-test-{}-{}-{name}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        std::env::temp_dir().join(unique)
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("linea-{i}\n")).collect()
    }

    #[test]
    fn last_n_lines_casos() {
        let text = numbered(5);
        assert_eq!(
            last_n_lines(&text, 3),
            vec!["linea-3", "linea-4", "linea-5"]
        );
        assert_eq!(
            last_n_lines(&text, 5),
            vec!["linea-1", "linea-2", "linea-3", "linea-4", "linea-5"]
        );
        // More than the total, zero and empty.
        let text = numbered(3);
        assert_eq!(last_n_lines(&text, 99).len(), 3);
        assert!(last_n_lines(&text, 0).is_empty());
        assert!(last_n_lines("", 10).is_empty());
        // Without a trailing newline the last line still counts.
        assert_eq!(last_n_lines("a\nb", 1), vec!["b"]);
        assert_eq!(last_n_lines("a\nb", 2), vec!["a", "b"]);
        assert_eq!(last_n_lines("sola", 1), vec!["sola"]);
    }

    #[test]
    fn needs_reopen_detecta_truncado_y_rotacion() {
        // Grows with same inode: keep following.
        assert!(!needs_reopen(100, 150, Some(7), Some(7)));
        // Unchanged: keep following.
        assert!(!needs_reopen(100, 100, Some(7), Some(7)));
        // Truncated (size < offset): reopen.
        assert!(needs_reopen(100, 40, Some(7), Some(7)));
        // Rotated (different inode even when the size matches): reopen.
        assert!(needs_reopen(100, 100, Some(7), Some(8)));
        assert!(needs_reopen(100, 150, Some(7), Some(8)));
        // Recreated file (size 0): reopen.
        assert!(needs_reopen(100, 0, Some(7), Some(7)));
    }

    #[test]
    fn tail_file_ultimas_n_y_bordes() {
        let path = tmp_log("tail");
        std::fs::write(&path, numbered(100)).unwrap();
        let out = tail_file(&path, 10).unwrap();
        let expected: String = (91..=100).map(|i| format!("linea-{i}\n")).collect();
        assert_eq!(out, expected.as_bytes());
        // Empty => empty.
        std::fs::write(&path, "").unwrap();
        assert!(tail_file(&path, 10).unwrap().is_empty());
        // n larger than the total => everything.
        std::fs::write(&path, "a\nb\n").unwrap();
        assert_eq!(tail_file(&path, 99).unwrap(), b"a\nb\n");
        // Without a trailing newline: the last line still counts.
        std::fs::write(&path, "a\nb").unwrap();
        assert_eq!(tail_file(&path, 1).unwrap(), b"b");
        assert_eq!(tail_file(&path, 2).unwrap(), b"a\nb");
        // Spans several 8 KB blocks (20 KB, one long line + tail).
        let big = format!("{}\n", "x".repeat(20 * 1024));
        std::fs::write(&path, format!("{big}cola1\ncola2\n")).unwrap();
        assert_eq!(tail_file(&path, 2).unwrap(), b"cola1\ncola2\n");
        assert_eq!(tail_file(&path, 1).unwrap(), b"cola2\n");
        // n == 0 => empty without opening anything extra.
        assert!(tail_file(&path, 0).unwrap().is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn tail_file_inexistente_falla_con_ruta() {
        let path = tmp_log("falta");
        assert!(!path.exists());
        let err = tail_file(&path, 10).expect_err("debe fallar alto");
        assert!(
            err.contains(&path.display().to_string()),
            "ruta, got: {err}"
        );
    }

    /// Minimal in-memory `Config` to test `resolve_log_file` without
    /// environment or DB.
    fn test_config(log: Option<PathBuf>) -> Config {
        Config {
            database_url: "postgres://u:p@127.0.0.1/db".to_owned(),
            database_url_ro: None,
            worker_id: "test".to_owned(),
            queue_name: "pilotwave.email".to_owned(),
            backend_name: "default".to_owned(),
            task_paths: vec!["pilotwave.tasks.mail_notification".to_owned()],
            poll_min: Duration::from_millis(1500),
            poll_max: Duration::from_millis(5000),
            task_timeout: Duration::from_secs(30),
            max_attempts: 5,
            recycle_n: 5_000,
            smtp_host: "smtp.example.com".to_owned(),
            smtp_port: 587,
            smtp_use_tls: true,
            smtp_user: "user".to_owned(),
            smtp_password: "secret".to_owned(),
            attachment_roots: Vec::new(),
            from: "CRM <crm@example.com>".parse().unwrap(),
            skip_grant_check: false,
            pid_file: None,
            log_file: log,
        }
    }

    #[test]
    fn resolve_log_file_fail_loud_y_ok() {
        // Without a configured log: loud failure naming the var.
        let err = resolve_log_file(&test_config(None)).expect_err("fail loud");
        assert!(err.contains("PILOTWAVE_LOG_FILE"), "got: {err}");
        // Missing: fail with the path, never create the file.
        let path = tmp_log("no-crear");
        assert!(!path.exists());
        let err = resolve_log_file(&test_config(Some(path.clone()))).expect_err("fail loud");
        assert!(
            err.contains(&path.display().to_string()),
            "ruta, got: {err}"
        );
        assert!(!path.exists(), "no debe crear el archivo vacío");
        // Existing: ok.
        let path = tmp_log("ok");
        std::fs::write(&path, "x\n").unwrap();
        assert_eq!(
            resolve_log_file(&test_config(Some(path.clone()))),
            Ok(path.clone())
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn follow_poll_esta_en_rango() {
        assert!(FOLLOW_POLL >= Duration::from_millis(500));
        assert!(FOLLOW_POLL <= Duration::from_secs(1));
    }
}
