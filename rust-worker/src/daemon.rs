//! PID file con lock exclusivo para el daemon `--bg` (PLAN-003 §3.8).
//!
//! Protocolo (deja listo el diseño para `stop` y `status`):
//! - El hijo destacamentado abre el archivo, toma `flock` exclusivo no
//!   bloqueante, escribe su PID y **mantiene el fd abierto toda su vida**.
//!   Cualquier muerte (exit, SIGKILL, OOM) suelta el lock sola: nunca hay
//!   locks stale.
//! - El texto del PID sí puede quedar stale; el lock es lo autoritativo:
//!   `status` (paso posterior) dirá "vivo" solo si el lock está tomado por un
//!   proceso cuyo cmdline contiene `pilotwave-worker` (identity check §3.8).
//! - `stop` (paso posterior): SIGTERM al PID, espera ≥35 s (§3.9), luego
//!   SIGKILL; la liberación del lock confirma la muerte.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Config;

/// Resuelve la sección daemon de `status` sin tocar la BD.
///
/// Sin `PILOTWAVE_PID_FILE` => `NotConfigured` (sin exigir el log, a
/// diferencia de `paths()`); con pidfile, sonda el lock + identidad y
/// resuelve uptime/rss best-effort (`None` = `/proc` ilegible => `n/a`).
pub fn describe(config: &Config) -> crate::cli::DaemonInfo {
    match config.pid_file.as_ref() {
        None => crate::cli::DaemonInfo::NotConfigured,
        // Nótese: `config.pid_file` directo, NO `paths()`
        // (ese exige también el log; aquí solo importa el pidfile).
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

/// `stop`: SIGTERM + espera + SIGKILL al daemon, sin tocar la BD.
///
/// - Sin `PILOTWAVE_PID_FILE` => error explícito (exit 1): sin pidfile no hay
///   daemon que detener (usa `config.pid_file` directo, NO `paths()`,
///   que además exigiría el log).
/// - Pidfile sin holder vivo => `daemon: no corriendo`, exit 0 (idempotente).
/// - La espera deriva de `task_timeout` + 5 s con suelo de 35 s (§3.9) y la
///   fase SIGKILL espera 5 s más (ver `STOP_*`).
pub fn run_stop(config: &Config) -> Result<StopOutcome, Box<dyn std::error::Error>> {
    let Some(pid_path) = config.pid_file.as_ref() else {
        let msg = "PILOTWAVE_PID_FILE no configurado: sin pidfile no hay daemon que detener";
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

/// Resultado del padre `--bg`: idempotente si ya vive, o PID recién lanzado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgOutcome {
    /// Sonda previa vio al dueño vivo: no se tocó nada.
    AlreadyRunning(u32),
    /// Hijo destacamentado recién lanzado.
    Started(u32),
}

/// Padre `--bg`: sonda idempotente + spawn destacamentado, sin tocar la BD.
/// Ya vivo => no-op exit 0 (como la receta `just worker start`, §3.8).
/// Los dirs propios se crean (0700); cualquier fallo trae ruta y causa.
pub fn run_bg_parent(
    config: &Config,
    verbose: bool,
) -> Result<BgOutcome, Box<dyn std::error::Error>> {
    let (pid_path, log_path) =
        paths(config).map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
    // Sonda idempotente primero: si ya vive, no tocamos nada (cron-friendly).
    if let LockState::Running(pid) = check(&pid_path) {
        return Ok(BgOutcome::AlreadyRunning(pid));
    }
    // Preflight ANTES de spawnear: dirs propios, pidfile escribible y 0600.
    // Sin esto el hijo arrancaba huérfano y el padre moría con `os error 2`.
    ensure_parent_dir(&pid_path).map_err(|err| std::io::Error::other(err.to_string()))?;
    ensure_parent_dir(&log_path).map_err(|err| std::io::Error::other(err.to_string()))?;
    ensure_pid_file_writable(&pid_path).map_err(std::io::Error::other)?;
    let pid = spawn_detached(&log_path, verbose)?;
    // El hijo re-escribe su PID al tomar el lock; esto deja el archivo listo
    // desde ya (texto stale + unlocked = Free si el hijo no arrancara).
    std::fs::write(&pid_path, format!("{pid}\n")).map_err(|err| {
        std::io::Error::other(format!("write pid file {}: {err}", pid_path.display()))
    })?;
    Ok(BgOutcome::Started(pid))
}

/// Espera tras SIGTERM antes de escalar a SIGKILL (§3.9: da margen al
/// shutdown graceful; el caller suele derivarla de `task_timeout` + 5 s,
/// con suelo de 35 s). El valor por defecto cubre el `TASK_TIMEOUT_SECS` 30.
pub const STOP_TERM_TIMEOUT: Duration = Duration::from_secs(35);
/// Espera adicional tras SIGKILL antes de rendirse con error.
pub const STOP_KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// Intervalo de sondeo mientras se espera que el daemon muera.
pub const STOP_POLL: Duration = Duration::from_millis(500);

/// Rutas del daemon. Ambas deben estar configuradas (absolutas) para
/// `--bg` / `--bg-child`; el resto de subcomandos las ignora.
pub fn paths(config: &Config) -> Result<(PathBuf, PathBuf), String> {
    let pid = config.pid_file.clone().ok_or_else(|| {
        "PILOTWAVE_PID_FILE must be set to an absolute path to use --bg".to_owned()
    })?;
    let log = config.log_file.clone().ok_or_else(|| {
        "PILOTWAVE_LOG_FILE must be set to an absolute path to use --bg".to_owned()
    })?;
    Ok((pid, log))
}

/// Estado del PID file sin modificarlo (el lock, si se toma, se suelta al
/// salir: el `File` se dropea aquí mismo).
pub enum LockState {
    /// Sin holder vivo (falta el archivo, texto stale, o sin lock).
    Free,
    /// Un proceso vivo tiene el lock y su cmdline coincide.
    Running(u32),
}

/// Sonda previa al spawn: no crea ni escribe nada.
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

/// Error de [`acquire`].
#[derive(Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// Otro daemon vivo tiene el lock.
    AlreadyRunning(u32),
    /// El archivo está bloqueado por un proceso ajeno, o fallo de E/S.
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

/// Toma el lock exclusivo del PID file y escribe el PID propio.
/// Devuelve el `File` abierto: **el caller debe mantenerlo vivo** (al
/// dropearse se suelta el lock).
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

/// Lee el PID escrito (texto; puede ser stale). `None` si falta o es basura.
pub fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `true` si el proceso existe (`kill -0`, misma semántica que la receta
/// §3.8) y su cmdline contiene `pilotwave-worker`.
pub fn is_alive_with_identity(pid: u32) -> bool {
    // EPERM (proceso ajeno) cuenta como no-nuestro: el daemon siempre corre
    // con nuestro usuario.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    alive && cmdline_matches(pid)
}

fn cmdline_matches(pid: u32) -> bool {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    identity_matches(&raw)
}

/// El nombre del binario aparece en argv[0] del cmdline (separado por NUL).
/// Función pura para poder probarla sin `/proc`.
fn identity_matches(cmdline: &[u8]) -> bool {
    let needle = b"pilotwave-worker";
    cmdline.windows(needle.len()).any(|window| window == needle)
}

/// `flock` exclusivo no bloqueante. `Ok` si se tomó; `Err(errno)` si no.
fn try_flock(file: &File) -> Result<(), libc::c_int> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` sobre un fd propio válido es async-signal-safe y no
    // toca memoria ajena; el único efecto es el lock del archivo.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

/// Asegura el directorio padre de `path` (crea con 0700 si falta).
/// Nunca toca permisos de dirs existentes (p. ej. `/tmp`).
/// Los errores ya traen la ruta (`mkdir …`, `chmod …`).
pub fn ensure_parent_dir(path: &Path) -> Result<(), AcquireError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir_0700(parent)?;
    }
    Ok(())
}

/// Causa legible para un fallo abriendo/creando `path`: qué archivo, qué
/// operación y pista accionable (el `os error N` a secas no dice ni cuál).
fn open_failed(path: &Path, what: &str, err: &std::io::Error) -> String {
    use std::io::ErrorKind as Kind;
    let hint = match err.kind() {
        Kind::NotFound => "ruta inexistente o symlink roto",
        Kind::PermissionDenied => "sin permiso: revisa dueño y modo del archivo y sus directorios",
        Kind::IsADirectory => "es un directorio, no un archivo",
        Kind::AlreadyExists => "ya existe con otro tipo",
        _ => "fallo de E/S",
    };
    format!("{what} {}: {hint}: {err}", path.display())
}

/// Preflight del padre `--bg`: el PID file debe poder crearse/abrirse en
/// lectura+escritura, ser un archivo regular y quedar en 0600.
/// Todo fallo incluye ruta y causa accionable.
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
            "pid file {}: no es un archivo regular",
            path.display()
        ));
    }
    // El modo solo aplica al crear: si ya existía más abierto, endurecer.
    if meta.permissions().mode() & 0o7777 & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|err| {
            format!(
                "pid file {}: no se pudo endurecer a 0600: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Crea el directorio con 0700 (como la receta §3.8). Solo aplica el chmod
/// si el directorio no existía: nunca tocamos los permisos de dirs del
/// sistema (p. ej. `/tmp` en tests).
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

/// Resultado de [`stop_daemon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// No había daemon vivo (pidfile ausente/stale/desbloqueado, o el PID
    /// murió entre `check` y la señal). Idempotente: el caller sale con 0.
    AlreadyStopped,
    /// El daemon murió tras la señal. `forced` = hizo falta SIGKILL.
    Stopped { pid: u32, forced: bool },
}

/// `true` si el daemon `pid` ya no corre: lock libre O identidad perdida.
///
/// La liberación del lock confirma la muerte aunque se reutilice el PID
/// (`kill -0` solo no bastaría: otro proceso podría heredar el número).
pub fn is_stopped(path: &Path, pid: u32) -> bool {
    matches!(check(path), LockState::Free) || !is_alive_with_identity(pid)
}

/// Envía `sig` a `pid`. `Ok` si se entregó; `Err(errno)` si no.
///
/// Llamar solo tras verificar `is_alive_with_identity(pid)`: entre esa
/// comprobación y el `kill` el proceso puede morir (ESRCH) sin que sea un
/// error.
fn send_signal(pid: u32, sig: libc::c_int) -> Result<(), libc::c_int> {
    // SAFETY: `kill` con señal válida es async-signal-safe y no toca
    // memoria; el PID ya pasó el identity check del caller.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

/// Espera hasta `timeout` a que el daemon muera (ver [`is_stopped`]).
/// `true` si paró, `false` si sigue vivo al agotar el tiempo.
///
/// Con `timeout` cero solo sondea una vez, sin dormir (útil en tests).
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

/// Detiene al daemon del `pid_path`: SIGTERM, espera `term_timeout`
/// sondeando [`is_stopped`], y si sigue vivo SIGKILL + espera `kill_timeout`.
///
/// - Sin holder vivo => `AlreadyStopped` (idempotente, sin señalizar).
/// - ESRCH al señalizar => el proceso murió en carrera => `AlreadyStopped`.
/// - EPERM => `Err` con pidfile, PID y pista (suele ser otro usuario).
/// - Si tras SIGTERM+SIGKILL sigue vivo => `Err` con pidfile y PID.
///   No toca la BD; el caller (`main.rs`) solo resuelve el pidfile e imprime.
pub fn stop_daemon(
    path: &Path,
    term_timeout: Duration,
    kill_timeout: Duration,
) -> Result<StopOutcome, String> {
    let pid = match check(path) {
        LockState::Free => return Ok(StopOutcome::AlreadyStopped),
        LockState::Running(pid) => pid,
    };
    // Re-verifica justo antes de señalizar: pudo morir entre `check` y aquí.
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
                "no se pudo detener pid={pid} (pid file {}): sin permiso (EPERM): revisa que el daemon corra con tu usuario",
                path.display()
            ));
        }
        Err(errno) => {
            return Err(format!(
                "no se pudo enviar SIGTERM a pid={pid} (pid file {}): kill devolvió errno {errno}",
                path.display()
            ));
        }
    }
    if wait_for_stop(path, pid, term_timeout) {
        return Ok(StopOutcome::Stopped { pid, forced: false });
    }
    // Sigue vivo tras el grace: SIGKILL y espera corta final.
    match send_signal(pid, libc::SIGKILL) {
        Ok(()) => {}
        Err(errno) if errno == libc::ESRCH => {
            // Murió en carrera con el SIGKILL: confirma por lock/identidad.
            if is_stopped(path, pid) {
                return Ok(StopOutcome::Stopped { pid, forced: false });
            }
        }
        Err(errno) if errno == libc::EPERM => {
            return Err(format!(
                "no se pudo forzar pid={pid} (pid file {}): sin permiso (EPERM): revisa que el daemon corra con tu usuario",
                path.display()
            ));
        }
        Err(errno) => {
            return Err(format!(
                "no se pudo enviar SIGKILL a pid={pid} (pid file {}): kill devolvió errno {errno}",
                path.display()
            ));
        }
    }
    if wait_for_stop(path, pid, kill_timeout) {
        return Ok(StopOutcome::Stopped { pid, forced: true });
    }
    Err(format!(
        "daemon pid={pid} sigue vivo tras SIGTERM+SIGKILL (pid file {}): no responde; revísalo manualmente",
        path.display()
    ))
}

/// Lanza el hijo destacamentado: el mismo binario con `run --bg-child`,
/// en nueva sesión (`setsid`, sobrevive al cierre del shell), stdin
/// `/dev/null` y stdout+stderr en append al log (creado 0600).
/// Devuelve el PID del hijo. No toca la BD: llamar antes de conectar.
pub fn spawn_detached(log_path: &Path, verbose: bool) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    if let Some(parent) = log_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir_0700(parent).map_err(|err| err.to_string())?;
    }
    // `umask 0077` de la receta §3.8: el log nace 0600. El padre sale
    // justo después del spawn, así que cambiar la máscara aquí no afecta
    // a nada más.
    // SAFETY: single-threaded en este punto (runtime current_thread sin
    // tareas vivas, sin pool creado); el cambio es proceso-local.
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
    // SAFETY: el closure corre tras `fork`, antes de `exec`, en contexto
    // async-signal-safe; solo llama a `setsid(2)`, sin tocar memoria Rust.
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

    /// Archivo temporal único por test (sin `tempfile`: solo std).
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
        // Prefijo parecido no vale.
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
        // Mismo proceso, segundo fd: el lock está tomado. Si el cmdline del
        // test contiene el nombre (típico en este checkout) vemos al dueño;
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
        // Existente más abierto => endurece a 0600 en vez de fallar.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        ensure_pid_file_writable(&path).expect("endurece");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Un directorio como pidfile => error explícito con la ruta.
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
        // Existente: no lo toca aunque venga más abierto.
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
        // Sin pidfile: nada que detener.
        let path = tmp_pid("stop-missing");
        assert!(!path.exists());
        assert_eq!(
            stop_daemon(&path, Duration::ZERO, Duration::ZERO),
            Ok(StopOutcome::AlreadyStopped)
        );
        // Texto válido pero sin proceso ni lock: Free => no señaliza nada.
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
        // PID numéricamente válido pero imposible (ver test anterior):
        // muerto sin dormir.
        assert!(is_stopped(&path, 999_999_999));
        assert!(wait_for_stop(&path, 999_999_999, Duration::ZERO));
        assert!(wait_for_stop(&path, 999_999_999, Duration::from_secs(5)));
    }

    #[test]
    fn send_signal_a_pid_inexistente_devuelve_esrch() {
        // `kill` a un PID imposible no entrega nada: solo mapea el errno.
        // Nótese: `u32::MAX` como `pid_t` sería `-1` (broadcast a todos los
        // procesos propios), así que se usa un PID numéricamente válido pero
        // por encima de `pid_max` (siempre ESRCH, nunca entrega nada).
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
}
