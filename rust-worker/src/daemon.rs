//! PID file with an exclusive lock for the `--bg` daemon.
//!
//! Protocol (leaves the design ready for `stop` and `status`):
//! - The detached child opens the file, takes a non-blocking exclusive
//!   `flock`, writes its PID and **keeps the fd open for its whole life**.
//!   Any death (exit, SIGKILL, OOM) releases the lock on its own: locks are
//!   never stale.
//! - The PID text may go stale; the lock is authoritative: `status` reports
//!   "alive" only when the lock is held by a process whose cmdline contains
//!   `pilotwave-worker` (identity check).
//! - `stop`: SIGTERM the PID, wait >=35 s, then SIGKILL; releasing the lock
//!   confirms the death.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Config;

/// Resolve the `status` daemon section without touching the DB.
///
/// Without `PILOTWAVE_PID_FILE` => `NotConfigured` (no log required, unlike
/// `paths()`); with a pidfile, probe the lock + identity and resolve
/// uptime/rss best-effort (`None` = unreadable `/proc` => `n/a`).
pub fn describe(config: &Config) -> crate::cli::DaemonInfo {
    match config.pid_file.as_ref() {
        None => crate::cli::DaemonInfo::NotConfigured,
        // Note: `config.pid_file` directly, NOT `paths()`
        // (that one also requires the log; here only the pidfile matters).
        Some(pid_path) => match check(pid_path) {
            LockState::Free => crate::cli::DaemonInfo::NotRunning,
            LockState::Running(pid) => crate::cli::DaemonInfo::Running {
                pid,
                uptime: crate::proc::uptime_secs(pid),
                rss_kb: crate::proc::read_vmrss_kb(pid),
            },
        },
    }
}

/// `stop`: SIGTERM + wait + SIGKILL the daemon, without touching the DB.
///
/// - Without `PILOTWAVE_PID_FILE` => explicit error (exit 1): without a
///   pidfile there is no daemon to stop (uses `config.pid_file` directly,
///   NOT `paths()`, which would additionally require the log).
/// - Pidfile with no live holder => `daemon: not running`, exit 0
///   (idempotent).
/// - The wait derives from `task_timeout` + 5 s with a 35 s floor and the
///   SIGKILL phase waits 5 s more (see `STOP_*`).
pub fn run_stop(config: &Config) -> Result<StopOutcome, Box<dyn std::error::Error>> {
    let Some(pid_path) = config.pid_file.as_ref() else {
        let msg = "PILOTWAVE_PID_FILE not configured: without a pidfile there is no daemon to stop";
        return Err(
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
                as Box<dyn std::error::Error>,
        );
    };
    let term_timeout = config
        .task_timeout
        .checked_add(Duration::from_secs(5))
        .unwrap_or(Duration::MAX)
        .max(STOP_TERM_TIMEOUT);
    match stop_daemon(pid_path, term_timeout, STOP_KILL_TIMEOUT) {
        Ok(outcome) => Ok(outcome),
        Err(msg) => Err(Box::new(std::io::Error::other(msg)) as Box<dyn std::error::Error>),
    }
}

/// Outcome of the `--bg` parent: idempotent when already alive, or a newly
/// launched PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgOutcome {
    /// A prior probe saw the owner alive: nothing was touched.
    AlreadyRunning(u32),
    /// Newly launched detached child.
    Started(u32),
}

/// `--bg` parent: idempotent probe + detached spawn, without touching the DB.
/// Already alive => no-op exit 0. Owned dirs are created (0700); any failure
/// carries path and cause.
///
/// Concurrency: concurrent starters serialize on a `<pid>.lock` startup
/// lock held across the probe, spawn and wait for the child to publish.
/// The parent never writes the PID file itself (only the child writes it
/// while holding the PID lock via [`acquire`]): an earlier parent write
/// used to truncate the file after the winning child had published, so
/// later probes and `stop` no longer identified the running daemon.
pub fn run_bg_parent(
    config: &Config,
    verbose: bool,
) -> Result<BgOutcome, Box<dyn std::error::Error>> {
    let (pid_path, log_path) =
        paths(config).map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
    // Preflight BEFORE spawning: owned dirs, writable pidfile, 0600.
    // Without this the child started orphaned and the parent died with
    // `os error 2`. This also creates the pidfile (empty) when missing,
    // so the absence of a child write below still leaves a `Free` file
    // behind instead of a missing path.
    ensure_parent_dir(&pid_path).map_err(|err| std::io::Error::other(err.to_string()))?;
    ensure_parent_dir(&log_path).map_err(|err| std::io::Error::other(err.to_string()))?;
    ensure_pid_file_writable(&pid_path).map_err(std::io::Error::other)?;
    // Serialize concurrent starters: two overlapping cron ticks must not
    // both probe `Free` and spawn. The lock is never stale (`flock`
    // releases on death) and is held until the child publishes.
    let lock_path = startup_lock_path(&pid_path);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|err| std::io::Error::other(open_failed(&lock_path, "startup lock file", &err)))?;
    match try_flock(&lock_file) {
        Err(raw) if raw == libc::EWOULDBLOCK || raw == libc::EAGAIN => {
            // Another `run --bg` is spawning right now: wait for its daemon
            // instead of spawning a duplicate. A timeout means the other
            // starter died without publishing; the next cron tick retries.
            if let Some(pid) = wait_for_running(&pid_path, STARTUP_WAIT) {
                return Ok(BgOutcome::AlreadyRunning(pid));
            }
            return Err(Box::new(std::io::Error::other(format!(
                "another run --bg startup in progress (startup lock {}): \
                 no daemon appeared; retry on the next tick",
                lock_path.display()
            ))) as Box<dyn std::error::Error>);
        }
        Err(raw) => {
            return Err(Box::new(std::io::Error::other(format!(
                "flock {}: {raw}",
                lock_path.display()
            ))) as Box<dyn std::error::Error>);
        }
        Ok(()) => {}
    }
    // Held until return: probe + spawn + wait are atomic w.r.t. other
    // `run --bg` parents.
    let _startup_guard = lock_file;
    // Idempotent probe first: when already alive, touch nothing (cron-friendly).
    if let LockState::Running(pid) = check(&pid_path) {
        return Ok(BgOutcome::AlreadyRunning(pid));
    }
    let spawned = spawn_detached(&log_path, verbose)?;
    // The child publishes its own PID when it takes the PID lock (see
    // `acquire`); the parent must not touch the file afterwards. Wait
    // briefly so a concurrent starter serialized behind us observes
    // `Running` instead of spawning a duplicate. On timeout preserve the
    // old fire-and-forget semantics: report the spawn and let the next
    // probe retry (the file stays `Free`: stale text + unlocked).
    if let Some(pid) = wait_for_running(&pid_path, STARTUP_WAIT) {
        if pid == spawned {
            return Ok(BgOutcome::Started(pid));
        }
        // Another daemon won the PID lock (e.g. a manual `--bg-child`):
        // idempotent, report the owner instead of the dead duplicate.
        return Ok(BgOutcome::AlreadyRunning(pid));
    }
    Ok(BgOutcome::Started(spawned))
}

/// Wait after SIGTERM before escalating to SIGKILL (graceful-shutdown
/// margin; callers usually derive it from `task_timeout` + 5 s, with a 35 s
/// floor). The default covers a 30 s `TASK_TIMEOUT_SECS`.
pub const STOP_TERM_TIMEOUT: Duration = Duration::from_secs(35);
/// Additional wait after SIGKILL before giving up with an error.
pub const STOP_KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the daemon to die.
pub const STOP_POLL: Duration = Duration::from_millis(500);
/// How long a `run --bg` parent waits for the freshly spawned child to
/// take the PID lock before reporting the spawn fire-and-forget. Long
/// enough for fork+exec on a loaded host, short enough to stay
/// cron-friendly (the 5-minute tick retries anyway).
pub const STARTUP_WAIT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the child to publish its PID.
pub const STARTUP_POLL: Duration = Duration::from_millis(50);

/// Startup serialization lock: `<pidfile>.lock` next to the PID file.
/// Held by a `run --bg` parent across its probe, spawn and wait so two
/// overlapping starters cannot both observe `Free` and spawn. Never stale:
/// `flock` releases it when the holder dies.
pub fn startup_lock_path(pid_path: &Path) -> PathBuf {
    let mut raw = pid_path.as_os_str().to_owned();
    raw.push(".lock");
    PathBuf::from(raw)
}

/// Poll [`check`] until the daemon shows `Running` or `timeout` elapses.
/// `Some(pid)` when the child published in time; `None` on timeout.
/// With a zero `timeout` probe once, without sleeping (handy in tests).
pub fn wait_for_running(path: &Path, timeout: Duration) -> Option<u32> {
    let start = Instant::now();
    loop {
        if let LockState::Running(pid) = check(path) {
            return Some(pid);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(STARTUP_POLL.min(timeout.saturating_sub(start.elapsed())));
    }
}

/// Daemon paths. Both must be configured (absolute) for `--bg` /
/// `--bg-child`; every other subcommand ignores them.
pub fn paths(config: &Config) -> Result<(PathBuf, PathBuf), String> {
    let pid = config.pid_file.clone().ok_or_else(|| {
        "PILOTWAVE_PID_FILE must be set to an absolute path to use --bg".to_owned()
    })?;
    let log = config.log_file.clone().ok_or_else(|| {
        "PILOTWAVE_LOG_FILE must be set to an absolute path to use --bg".to_owned()
    })?;
    Ok((pid, log))
}

/// PID-file state without modifying it (a lock taken here is released on
/// exit: the `File` drops right away).
pub enum LockState {
    /// No live holder (file missing, stale text, or no lock).
    Free,
    /// A live process holds the lock and its cmdline matches.
    Running(u32),
}

/// Pre-spawn probe: creates and writes nothing.
pub fn check(path: &Path) -> LockState {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return LockState::Free,
    };
    if try_flock(&file).is_err()
        && let Some(pid) = read_pid(path).filter(|pid| is_alive_with_identity(*pid))
    {
        return LockState::Running(pid);
    }
    LockState::Free
}

/// [`acquire`] error.
#[derive(Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// Another live daemon holds the lock.
    AlreadyRunning(u32),
    /// The file is locked by a foreign process, or an I/O failure.
    Io(String),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::AlreadyRunning(pid) => {
                write!(f, "already running pid={pid}")
            }
            AcquireError::Io(msg) => write!(f, "pid file error: {msg}"),
        }
    }
}

/// Take the PID file's exclusive lock and write our own PID.
/// Return the open `File`: **the caller must keep it alive** (dropping it
/// releases the lock).
pub fn acquire(path: &Path) -> Result<File, AcquireError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir_0700(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|err| AcquireError::Io(format!("open {}: {err}", path.display())))?;
    match try_flock(&file) {
        Ok(()) => {}
        Err(raw) if raw == libc::EWOULDBLOCK || raw == libc::EAGAIN => {
            if let Some(pid) = read_pid(path).filter(|pid| is_alive_with_identity(*pid)) {
                return Err(AcquireError::AlreadyRunning(pid));
            }
            return Err(AcquireError::Io(format!(
                "{} is locked by a foreign process",
                path.display()
            )));
        }
        Err(raw) => {
            return Err(AcquireError::Io(format!("flock {}: {raw}", path.display())));
        }
    }
    file.set_len(0)
        .map_err(|err| AcquireError::Io(format!("truncate {}: {err}", path.display())))?;
    writeln!(file, "{}", std::process::id())
        .map_err(|err| AcquireError::Io(format!("write {}: {err}", path.display())))?;
    file.flush()
        .map_err(|err| AcquireError::Io(format!("flush {}: {err}", path.display())))?;
    Ok(file)
}

/// Read the written PID (text; may be stale). `None` when missing or garbage.
pub fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `true` when the process exists (`kill -0`) and its cmdline contains
/// `pilotwave-worker`.
pub fn is_alive_with_identity(pid: u32) -> bool {
    // EPERM (foreign process) counts as not-ours: the daemon always runs
    // as our user.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    alive && cmdline_matches(pid)
}

fn cmdline_matches(pid: u32) -> bool {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    identity_matches(&raw)
}

/// The binary name shows up in the cmdline's argv[0] (NUL-separated).
/// Pure function for `/proc`-free testing.
fn identity_matches(cmdline: &[u8]) -> bool {
    let needle = b"pilotwave-worker";
    cmdline.windows(needle.len()).any(|window| window == needle)
}

/// Non-blocking exclusive `flock`. `Ok` when taken; `Err(errno)` when not.
fn try_flock(file: &File) -> Result<(), libc::c_int> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` on a valid owned fd is async-signal-safe and touches
    // no foreign memory; the only effect is the file lock.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

/// Ensure the parent directory of `path` (create with 0700 when missing).
/// Never touch permissions of existing dirs (e.g. `/tmp`).
/// Errors already carry the path (`mkdir …`, `chmod …`).
pub fn ensure_parent_dir(path: &Path) -> Result<(), AcquireError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir_0700(parent)?;
    }
    Ok(())
}

/// Human-readable cause for an open/create failure on `path`: which file,
/// which operation and an actionable hint (a bare `os error N` says neither).
fn open_failed(path: &Path, what: &str, err: &std::io::Error) -> String {
    use std::io::ErrorKind as Kind;
    let hint = match err.kind() {
        Kind::NotFound => "missing path or broken symlink",
        Kind::PermissionDenied => "permission denied: check file and directory owner and mode",
        Kind::IsADirectory => "is a directory, not a file",
        Kind::AlreadyExists => "already exists with another type",
        _ => "I/O failure",
    };
    format!("{what} {}: {hint}: {err}", path.display())
}

/// `--bg` parent preflight: the PID file must be creatable/openable
/// read+write, be a regular file and end up 0600.
/// Every failure includes path and actionable cause.
pub fn ensure_pid_file_writable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|err| open_failed(path, "pid file", &err))?;
    let meta = file
        .metadata()
        .map_err(|err| open_failed(path, "pid file (metadata)", &err))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "pid file {}: is not a regular file",
            path.display()
        ));
    }
    // The mode only applies at creation: when pre-existing and more open,
    // harden it.
    if meta.permissions().mode() & 0o7777 & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|err| {
            format!(
                "pid file {}: could not harden to 0600: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Create the directory with 0700. Only apply chmod when the directory did
/// not exist: never touch permissions of system dirs (e.g. `/tmp` in tests).
fn ensure_dir_0700(dir: &Path) -> Result<(), AcquireError> {
    use std::os::unix::fs::PermissionsExt;
    if dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .map_err(|err| AcquireError::Io(format!("mkdir {}: {err}", dir.display())))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| AcquireError::Io(format!("chmod 0700 {}: {err}", dir.display())))?;
    Ok(())
}

/// [`stop_daemon`] outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// No daemon was alive (pidfile missing/stale/unlocked, or the PID
    /// died between `check` and the signal). Idempotent: the caller exits 0.
    AlreadyStopped,
    /// The daemon died after the signal. `forced` = SIGKILL was needed.
    Stopped { pid: u32, forced: bool },
}

/// `true` when the `pid` daemon no longer runs: free lock OR lost identity.
///
/// Releasing the lock confirms the death even when the PID is reused
/// (bare `kill -0` would not suffice: another process could inherit it).
pub fn is_stopped(path: &Path, pid: u32) -> bool {
    matches!(check(path), LockState::Free) || !is_alive_with_identity(pid)
}

/// Send `sig` to `pid`. `Ok` when delivered; `Err(errno)` when not.
///
/// Call only after checking `is_alive_with_identity(pid)`: between that
/// check and the `kill` the process may die (ESRCH) without it being an
/// error.
fn send_signal(pid: u32, sig: libc::c_int) -> Result<(), libc::c_int> {
    // SAFETY: `kill` with a valid signal is async-signal-safe and touches no
    // memory; the PID already passed the caller's identity check.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

/// Wait up to `timeout` for the daemon to die (see [`is_stopped`]).
/// `true` when stopped, `false` when still alive at timeout.
///
/// With a zero `timeout` only probe once, without sleeping (handy in tests).
pub fn wait_for_stop(path: &Path, pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if is_stopped(path, pid) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(STOP_POLL.min(timeout.saturating_sub(start.elapsed())));
    }
}

/// Stop the `pid_path` daemon: SIGTERM, wait `term_timeout` polling
/// [`is_stopped`], and when still alive SIGKILL + `kill_timeout` wait.
///
/// - No live holder => `AlreadyStopped` (idempotent, no signaling).
/// - ESRCH while signaling => the process died in a race => `AlreadyStopped`.
/// - EPERM => `Err` with pidfile, PID and hint (usually another user).
/// - Still alive after SIGTERM+SIGKILL => `Err` with pidfile and PID.
///   Never touches the DB; the caller (`main.rs`) only resolves the pidfile
///   and prints.
pub fn stop_daemon(
    path: &Path,
    term_timeout: Duration,
    kill_timeout: Duration,
) -> Result<StopOutcome, String> {
    let pid = match check(path) {
        LockState::Free => return Ok(StopOutcome::AlreadyStopped),
        LockState::Running(pid) => pid,
    };
    // Re-verify right before signaling: it may have died between `check`
    // and here.
    if !is_alive_with_identity(pid) {
        return Ok(StopOutcome::AlreadyStopped);
    }
    match send_signal(pid, libc::SIGTERM) {
        Ok(()) => {}
        Err(errno) if errno == libc::ESRCH => {
            return Ok(StopOutcome::AlreadyStopped);
        }
        Err(errno) if errno == libc::EPERM => {
            return Err(format!(
                "could not stop pid={pid} (pid file {}): permission denied (EPERM): check that the daemon runs as your user",
                path.display()
            ));
        }
        Err(errno) => {
            return Err(format!(
                "could not send SIGTERM to pid={pid} (pid file {}): kill returned errno {errno}",
                path.display()
            ));
        }
    }
    if wait_for_stop(path, pid, term_timeout) {
        return Ok(StopOutcome::Stopped { pid, forced: false });
    }
    // Still alive after the grace period: SIGKILL and a short final wait.
    match send_signal(pid, libc::SIGKILL) {
        Ok(()) => {}
        Err(errno) if errno == libc::ESRCH => {
            // Died racing the SIGKILL: confirm via lock/identity.
            if is_stopped(path, pid) {
                return Ok(StopOutcome::Stopped { pid, forced: false });
            }
        }
        Err(errno) if errno == libc::EPERM => {
            return Err(format!(
                "could not force pid={pid} (pid file {}): permission denied (EPERM): check that the daemon runs as your user",
                path.display()
            ));
        }
        Err(errno) => {
            return Err(format!(
                "could not send SIGKILL to pid={pid} (pid file {}): kill returned errno {errno}",
                path.display()
            ));
        }
    }
    if wait_for_stop(path, pid, kill_timeout) {
        return Ok(StopOutcome::Stopped { pid, forced: true });
    }
    Err(format!(
        "daemon pid={pid} still alive after SIGTERM+SIGKILL (pid file {}): not responding; check it manually",
        path.display()
    ))
}

/// Launch the detached child: the same binary with `run --bg-child`, in a
/// new session (`setsid`, survives shell exit), stdin `/dev/null` and
/// stdout+stderr appended to the log (created 0600). Return the child PID.
/// Never touches the DB: call before connecting.
pub fn spawn_detached(log_path: &Path, verbose: bool) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    if let Some(parent) = log_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir_0700(parent).map_err(|err| err.to_string())?;
    }
    // With `umask 0077` the log is born 0600. The parent exits right after
    // the spawn, so changing the mask here affects nothing else.
    // SAFETY: single-threaded at this point (current_thread runtime with no
    // live tasks, no pool created); the change is process-local.
    unsafe { libc::umask(0o077) };
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path)
        .map_err(|err| open_failed(log_path, "log file", &err))?;
    let stdout = log
        .try_clone()
        .map_err(|err| format!("dup {}: {err}", log_path.display()))?;
    let null = File::open("/dev/null").map_err(|err| format!("open /dev/null: {err}"))?;
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let mut child = std::process::Command::new(exe);
    child.arg("run").arg("--bg-child");
    if verbose {
        child.arg("--verbose");
    }
    // SAFETY: the closure runs after `fork`, before `exec`, in an
    // async-signal-safe context; it only calls `setsid(2)`, touching no Rust
    // memory.
    unsafe {
        child.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = child
        .stdin(Stdio::from(null))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|err| format!("spawn detached child: {err}"))?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp file per test (no `tempfile`: std only).
    fn tmp_pid(name: &str) -> PathBuf {
        let unique = format!(
            "pilotwave-worker-test-{}-{}-{name}.pid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn identity_matches_busca_el_nombre_en_cmdline_nul() {
        assert!(identity_matches(b"pilotwave-worker\0run\0--bg-child\0"));
        assert!(identity_matches(
            b"/opt/app/bin/pilotwave-worker\0run\0--fg\0"
        ));
        assert!(!identity_matches(b"python\0db_worker\0"));
        assert!(!identity_matches(b""));
        // A similar prefix does not count.
        assert!(!identity_matches(b"pilotwave-worke\0"));
    }

    #[test]
    fn read_pid_acepta_numero_y_rechaza_basura() {
        let path = tmp_pid("read");
        assert_eq!(read_pid(&path), None);
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(read_pid(&path), Some(12345));
        std::fs::write(&path, "basura\n").unwrap();
        assert_eq!(read_pid(&path), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn pid_inexistente_no_esta_vivo() {
        assert!(!is_alive_with_identity(u32::MAX));
    }

    #[test]
    fn acquire_escribe_pid_y_segundo_intento_ve_dueno() {
        let path = tmp_pid("acquire");
        let guard = acquire(&path).expect("primer acquire");
        assert_eq!(read_pid(&path), Some(std::process::id()));
        // Same process, second fd: the lock is held. When the test cmdline
        // contains the name (typical in this checkout) we see the owner;
        // si no, cae al error de lock ajeno. Ambas ramas son deterministas
        // dado el entorno.
        if is_alive_with_identity(std::process::id()) {
            match acquire(&path) {
                Err(AcquireError::AlreadyRunning(pid)) => {
                    assert_eq!(pid, std::process::id())
                }
                other => panic!("se esperaba AlreadyRunning, got {other:?}"),
            }
        } else {
            assert!(matches!(acquire(&path), Err(AcquireError::Io(_))));
        }
        drop(guard);
        // Sin holder el lock se libera: texto stale, pero adquirible.
        std::fs::write(&path, "999999999\n").unwrap();
        assert!(acquire(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn pid_writable_crea_0600_endurece_y_rechaza_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_pid("writable");
        ensure_pid_file_writable(&path).expect("crea el archivo");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Pre-existing and more open => harden to 0600 instead of failing.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        ensure_pid_file_writable(&path).expect("endurece");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A directory as pidfile => explicit error carrying the path.
        let dir = tmp_pid("writable-dir");
        std::fs::create_dir(&dir).unwrap();
        match ensure_pid_file_writable(&dir) {
            Err(msg) => {
                assert!(msg.contains(&dir.display().to_string()));
                assert!(msg.contains("pid file"));
            }
            Ok(()) => panic!("un directorio no es pidfile válido"),
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn ensure_parent_dir_crea_0700_y_no_toca_existentes() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "pilotwave-worker-test-{}-{}-parent",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let target = base.join("sub").join("x.pid");
        ensure_parent_dir(&target).expect("crea la cadena");
        assert_eq!(
            std::fs::metadata(base.join("sub"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // Existing: untouched even when more open.
        std::fs::set_permissions(base.join("sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_parent_dir(&target).expect("no falla en existente");
        assert_eq!(
            std::fs::metadata(base.join("sub"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn stop_daemon_idempotente_sin_pidfile_o_stale() {
        // No pidfile: nothing to stop.
        let path = tmp_pid("stop-missing");
        assert!(!path.exists());
        assert_eq!(
            stop_daemon(&path, Duration::ZERO, Duration::ZERO),
            Ok(StopOutcome::AlreadyStopped)
        );
        // Valid text but no process or lock: Free => signal nothing.
        let path = tmp_pid("stop-stale");
        std::fs::write(&path, "999999999\n").unwrap();
        assert!(matches!(check(&path), LockState::Free));
        assert_eq!(
            stop_daemon(&path, Duration::ZERO, Duration::ZERO),
            Ok(StopOutcome::AlreadyStopped)
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn is_stopped_y_wait_son_true_para_pid_inexistente() {
        let path = tmp_pid("stop-falso");
        assert!(!path.exists());
        // Numerically valid but impossible PID (see previous test):
        // dead without sleeping.
        assert!(is_stopped(&path, 999_999_999));
        assert!(wait_for_stop(&path, 999_999_999, Duration::ZERO));
        assert!(wait_for_stop(&path, 999_999_999, Duration::from_secs(5)));
    }

    #[test]
    fn send_signal_a_pid_inexistente_devuelve_esrch() {
        // `kill` on an impossible PID delivers nothing: only maps the errno.
        // Note: `u32::MAX` as `pid_t` would be `-1` (broadcast to all our
        // own processes), so use a numerically valid PID above `pid_max`
        // (always ESRCH, never delivers).
        assert_eq!(send_signal(999_999_999, libc::SIGTERM), Err(libc::ESRCH));
    }

    #[test]
    fn check_no_crea_ni_modifica() {
        let path = tmp_pid("check");
        assert!(matches!(check(&path), LockState::Free));
        assert!(!path.exists(), "check no debe crear el archivo");
        std::fs::write(&path, "texto-stale\n").unwrap();
        assert!(matches!(check(&path), LockState::Free));
        let _guard = acquire(&path).expect("acquire");
        // Con el lock tomado por nosotros: Running si el identity check nos
        // reconoce (ver test anterior), Free si no. El archivo conserva el
        // PID escrito por acquire en ambos casos.
        match check(&path) {
            LockState::Running(pid) => assert_eq!(pid, std::process::id()),
            LockState::Free => assert!(!is_alive_with_identity(std::process::id())),
        }
        assert_eq!(read_pid(&path), Some(std::process::id()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn startup_lock_path_anade_suffix_lock() {
        let pid = PathBuf::from("/run/pilotwave/worker.pid");
        assert_eq!(
            startup_lock_path(&pid),
            PathBuf::from("/run/pilotwave/worker.pid.lock")
        );
        // Sin extensión también: sufijo simple, mismo directorio.
        let pid = PathBuf::from("/tmp/worker-pid");
        assert_eq!(
            startup_lock_path(&pid),
            PathBuf::from("/tmp/worker-pid.lock")
        );
    }

    #[test]
    fn wait_for_running_ve_guard_y_expira_en_stale() {
        // Sin holder: expira sin dormir cuando el timeout es cero, y con
        // timeout corto sobre texto stale.
        let path = tmp_pid("wait-free");
        assert_eq!(wait_for_running(&path, Duration::ZERO), None);
        std::fs::write(&path, "999999999\n").unwrap();
        assert_eq!(wait_for_running(&path, Duration::from_millis(10)), None);
        // Con el lock tomado por nosotros: Running solo cuando el identity
        // check reconoce al proceso de test; si no, sigue siendo Free.
        let _guard = acquire(&path).expect("acquire");
        match wait_for_running(&path, Duration::from_millis(10)) {
            Some(pid) => assert_eq!(pid, std::process::id()),
            None => assert!(!is_alive_with_identity(std::process::id())),
        }
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(startup_lock_path(&path));
    }
}
