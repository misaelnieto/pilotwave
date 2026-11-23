//! `log`: tail de `PILOTWAVE_LOG_FILE` (PLAN-003 §3.10.6, un solo archivo
//! line-oriented).
//!
//! - Sin `-f`: imprime las últimas N líneas a stdout y sale.
//! - Con `-f`: imprime las últimas N líneas y sigue mostrando los appends
//!   (poll cada [`FOLLOW_POLL`]) hasta SIGTERM/SIGINT (exit 0).
//! - Ante rotación (inode distinto) o truncado (tamaño < offset) avisa a
//!   stderr (`rotación detectada: reabriendo desde el inicio`) y reabre
//!   desde 0.
//! - La salida es exacta: líneas tal cual, sin numerar ni prefijos.
//!   Stdout lleva solo contenido del log; los avisos van a stderr.
//! - No toca la BD: solo necesita `config.log_file` (ruta absoluta).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::LogArgs;
use crate::config::Config;

/// Intervalo de sondeo de appends en `-f` (~500 ms, dentro del 500 ms-1 s
/// pedido).
pub const FOLLOW_POLL: Duration = Duration::from_millis(500);
/// Aviso a stderr ante rotación o truncado (el log rota poco, pero hay que
/// detectarlo en vez de quedarse colgado en un offset muerto).
pub const ROTATION_NOTICE: &str = "rotación detectada: reabriendo desde el inicio";
/// Bloque de lectura reversa del tail (evita cargar el archivo entero).
const TAIL_BLOCK: u64 = 8 * 1024;
/// Tope por poll en `-f`: si un poll trae más, se avanza por tramos en vez
/// de cargar todo de golpe.
const FOLLOW_MAX_READ: u64 = 16 * 1024 * 1024;

/// Resuelve el archivo a leer: exige `PILOTWAVE_LOG_FILE` absoluta y que el
/// archivo exista (nunca se crea vacío: si falta es fail loud con ruta y
/// causa, exit 1).
pub fn resolve_log_file(config: &Config) -> Result<PathBuf, String> {
    let path = config.log_file.clone().ok_or_else(|| {
        "PILOTWAVE_LOG_FILE no configurado: `log` necesita la ruta absoluta del log del daemon"
            .to_owned()
    })?;
    std::fs::metadata(&path).map_err(|err| open_failed(&path, "log file", &err))?;
    Ok(path)
}

/// Causa legible con ruta, qué operación y pista accionable.
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

/// Últimas `n` líneas de `text` (helper puro, sin E/S).
///
/// Es el núcleo testeable del tail: el binario usa [`tail_file`] (bytes,
/// sin cargar el archivo entero) y este helper expone la misma semántica
/// sobre `&str` para tests y futuros renders.
/// `n == 0` => vacío; `n` mayor que el total => todo. No incluye los `\n`.
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

/// Decide si `-f` debe reabrir desde 0: el inode cambió (rotación) o el
/// tamaño encogió bajo el offset (truncado). Función pura para probarla
/// sin E/S (los tamaños/inodes se inyectan).
pub fn needs_reopen(
    prev_len: u64,
    cur_len: u64,
    prev_ino: Option<u64>,
    cur_ino: Option<u64>,
) -> bool {
    cur_ino != prev_ino || cur_len < prev_len
}

/// Lee las últimas `n` líneas del archivo (bytes exactos, con sus `\n`).
///
/// Escanea hacia atrás por bloques de 8 KB contando `\n`, así que no carga
/// el archivo entero en memoria (el log rota poco pero puede crecer).
/// `n == 0` => vacío; menos de `n` líneas => todo el archivo.
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
    // Si el archivo termina en `\n`, ese byte cierra la última línea y no
    // cuenta como separador extra.
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

/// Lee el rango `[offset, len)` del archivo (un tramo de `-f`).
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

/// Tamaño e inode actual (el inode detecta la rotación aunque el tamaño
/// coincida). `None` si la plataforma no da inode (entonces solo el tamaño
/// decide).
fn stat(path: &Path) -> Result<(u64, Option<u64>), String> {
    let meta = std::fs::metadata(path).map_err(|err| open_failed(path, "log file", &err))?;
    #[cfg(unix)]
    let ino = Some(std::os::unix::fs::MetadataExt::ino(&meta));
    #[cfg(not(unix))]
    let ino: Option<u64> = None;
    Ok((meta.len(), ino))
}

/// Escribe bytes exactos a stdout (contenido del log, sin adornos).
fn write_stdout(data: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(data)?;
    out.flush()
}

/// `log`: tail de `PILOTWAVE_LOG_FILE`. Sin `-f` imprime y sale; con `-f`
/// sigue los appends hasta SIGTERM/SIGINT (exit 0). No toca la BD.
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
    // El tail ya mostró todo lo viejo: el follow parte del tamaño actual
    // y solo trae lo nuevo.
    let (mut offset, mut ino) = match stat(&path) {
        Ok(st) => st,
        // Carrera improbable (acaba de leerse): avisa y parte de 0.
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
                    // Tramo parcial (tope por poll): el siguiente poll trae
                    // el resto sin reabrir.
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

    /// Archivo temporal único por test (sin `tempfile`: solo std).
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
        // Más que el total, cero y vacío.
        let text = numbered(3);
        assert_eq!(last_n_lines(&text, 99).len(), 3);
        assert!(last_n_lines(&text, 0).is_empty());
        assert!(last_n_lines("", 10).is_empty());
        // Sin newline final la última línea cuenta igual.
        assert_eq!(last_n_lines("a\nb", 1), vec!["b"]);
        assert_eq!(last_n_lines("a\nb", 2), vec!["a", "b"]);
        assert_eq!(last_n_lines("sola", 1), vec!["sola"]);
    }

    #[test]
    fn needs_reopen_detecta_truncado_y_rotacion() {
        // Crece con mismo inode: seguir.
        assert!(!needs_reopen(100, 150, Some(7), Some(7)));
        // Igual: seguir.
        assert!(!needs_reopen(100, 100, Some(7), Some(7)));
        // Truncado (tamaño < offset): reabrir.
        assert!(needs_reopen(100, 40, Some(7), Some(7)));
        // Rotación (inode distinto aunque el tamaño coincida): reabrir.
        assert!(needs_reopen(100, 100, Some(7), Some(8)));
        assert!(needs_reopen(100, 150, Some(7), Some(8)));
        // Archivo recreado (tamaño 0): reabrir.
        assert!(needs_reopen(100, 0, Some(7), Some(7)));
    }

    #[test]
    fn tail_file_ultimas_n_y_bordes() {
        let path = tmp_log("tail");
        std::fs::write(&path, numbered(100)).unwrap();
        let out = tail_file(&path, 10).unwrap();
        let expected: String = (91..=100).map(|i| format!("linea-{i}\n")).collect();
        assert_eq!(out, expected.as_bytes());
        // Vacío => vacío.
        std::fs::write(&path, "").unwrap();
        assert!(tail_file(&path, 10).unwrap().is_empty());
        // n mayor que el total => todo.
        std::fs::write(&path, "a\nb\n").unwrap();
        assert_eq!(tail_file(&path, 99).unwrap(), b"a\nb\n");
        // Sin newline final: la última línea cuenta igual.
        std::fs::write(&path, "a\nb").unwrap();
        assert_eq!(tail_file(&path, 1).unwrap(), b"b");
        assert_eq!(tail_file(&path, 2).unwrap(), b"a\nb");
        // Cruza varios bloques de 8 KB (20 KB, una sola línea larga + cola).
        let big = format!("{}\n", "x".repeat(20 * 1024));
        std::fs::write(&path, format!("{big}cola1\ncola2\n")).unwrap();
        assert_eq!(tail_file(&path, 2).unwrap(), b"cola1\ncola2\n");
        assert_eq!(tail_file(&path, 1).unwrap(), b"cola2\n");
        // n == 0 => vacío sin siquiera abrir de más.
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

    /// `Config` mínima en memoria para probar `resolve_log_file` sin
    /// entorno ni BD.
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
        // Sin log configurado: fallo alto con la var.
        let err = resolve_log_file(&test_config(None)).expect_err("fail loud");
        assert!(err.contains("PILOTWAVE_LOG_FILE"), "got: {err}");
        // Inexistente: falla con la ruta y no crea el archivo.
        let path = tmp_log("no-crear");
        assert!(!path.exists());
        let err = resolve_log_file(&test_config(Some(path.clone()))).expect_err("fail loud");
        assert!(
            err.contains(&path.display().to_string()),
            "ruta, got: {err}"
        );
        assert!(!path.exists(), "no debe crear el archivo vacío");
        // Existente: ok.
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
