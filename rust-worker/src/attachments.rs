//! Policy de adjuntos en disco (PLAN-003 §3.4, paso B).
//!
//! `payload::parse` ya validó forma y tamaños *declarados*; aquí se valida lo
//! *real* antes de abrir cada fichero, en este orden:
//! 1. charset de la ruta (sin `\r \n \0`; defensa en profundidad del parse),
//!    ruta absoluta (las relativas dependerían del cwd);
//! 2. ningún componente es symlink (`symlink_metadata` por ancestro);
//! 3. `canonicalize` (`ENOENT`/`EACCES` → `Missing`, resto → `PathPolicy`);
//! 4. la ruta canonizada no cuelga de `/proc /sys /dev /etc`;
//! 5. la ruta canonizada cuelga de alguna root canonizada;
//! 6. tamaño real (`metadata`) dentro de caps, con total acumulado;
//! 7. lectura con `take(MAX+1)`: si entre `stat` y `read` el fichero creció
//!    (TOCTOU local), se rechaza en vez de cargar de más.
//!
//! Todo es permanente: `PathPolicy` → `pilotwave_worker.path_policy_violation`,
//! `Missing` → `pilotwave_worker.attachment_missing`. Nunca retry (§3.4, §3.6).
//!
//! Nota TOCTOU: el chequeo es check-then-read (best-effort). Basta porque la
//! amenaza es una fila plantada en BD, no un proceso local corriendo carreras
//! contra el worker; un atacante con ejecución local ya tiene el FS.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::contract::{MAX_FILE_BYTES, MAX_TOTAL_BYTES};

/// Fichero listo para adjuntar: nombre base + bytes ya acotados.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFile {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// Error de policy ya clasificado según §3.4/§3.6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// `pilotwave_worker.path_policy_violation`.
    PathPolicy(String),
    /// `pilotwave_worker.attachment_missing` (`ENOENT`/`EACCES`).
    Missing(String),
}

impl AttachError {
    pub fn code(&self) -> &'static str {
        match self {
            AttachError::PathPolicy(_) => "pilotwave_worker.path_policy_violation",
            AttachError::Missing(_) => "pilotwave_worker.attachment_missing",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            AttachError::PathPolicy(detail) | AttachError::Missing(detail) => detail,
        }
    }
}

/// Directorios sensibles: denegados aunque una root mal configurada los
/// incluyera (defensa en profundidad, §3.4c).
const DENIED_PREFIXES: &[&str] = &["/proc", "/sys", "/dev", "/etc"];

/// Carga y valida todos los adjuntos. `paths` viene de `EmailContent` (ya
/// validado en forma por `payload::parse`).
pub fn load(roots: &[PathBuf], paths: &[String]) -> Result<Vec<AttachmentFile>, AttachError> {
    let canon_roots = canonicalize_roots(roots);
    let mut files = Vec::with_capacity(paths.len());
    let mut total: u64 = 0;

    for (index, raw) in paths.iter().enumerate() {
        if raw.contains(['\r', '\n', '\0']) {
            return Err(AttachError::PathPolicy(format!(
                "attachment[{index}]: control characters in path"
            )));
        }
        let path = Path::new(raw);
        if !path.is_absolute() {
            return Err(AttachError::PathPolicy(format!(
                "attachment[{index}]: not an absolute path"
            )));
        }
        reject_symlink_components(path, index)?;
        let canon = std::fs::canonicalize(path).map_err(|err| {
            use std::io::ErrorKind::{NotFound, PermissionDenied};
            match err.kind() {
                NotFound | PermissionDenied => {
                    AttachError::Missing(format!("attachment[{index}]: unreadable ({err})"))
                }
                _ => {
                    AttachError::PathPolicy(format!("attachment[{index}]: cannot resolve ({err})"))
                }
            }
        })?;
        deny_system_prefixes(&canon, index)?;
        require_root_prefix(&canon, &canon_roots, index)?;

        let real_len = std::fs::metadata(&canon)
            .map_err(|err| {
                use std::io::ErrorKind::{NotFound, PermissionDenied};
                match err.kind() {
                    NotFound | PermissionDenied => {
                        AttachError::Missing(format!("attachment[{index}]: unreadable ({err})"))
                    }
                    _ => {
                        AttachError::PathPolicy(format!("attachment[{index}]: cannot stat ({err})"))
                    }
                }
            })?
            .len();
        if real_len > MAX_FILE_BYTES {
            return Err(AttachError::PathPolicy(format!(
                "attachment[{index}]: real size {real_len} exceeds per-file cap {MAX_FILE_BYTES}"
            )));
        }
        total = total.saturating_add(real_len);
        if total > MAX_TOTAL_BYTES {
            return Err(AttachError::PathPolicy(format!(
                "total real size {total} exceeds cap {MAX_TOTAL_BYTES}"
            )));
        }

        let filename = canon
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| AttachError::PathPolicy(format!("attachment[{index}]: no file name")))?
            .to_owned();

        // `take` + 1: si el fichero creció tras el `stat`, se detecta aquí.
        let mut bytes = Vec::new();
        std::fs::File::open(&canon)
            .map_err(|err| {
                use std::io::ErrorKind::{NotFound, PermissionDenied};
                match err.kind() {
                    NotFound | PermissionDenied => {
                        AttachError::Missing(format!("attachment[{index}]: unreadable ({err})"))
                    }
                    _ => {
                        AttachError::PathPolicy(format!("attachment[{index}]: cannot open ({err})"))
                    }
                }
            })?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| {
                use std::io::ErrorKind::{NotFound, PermissionDenied};
                match err.kind() {
                    NotFound | PermissionDenied => {
                        AttachError::Missing(format!("attachment[{index}]: unreadable ({err})"))
                    }
                    _ => {
                        AttachError::PathPolicy(format!("attachment[{index}]: cannot read ({err})"))
                    }
                }
            })?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(AttachError::PathPolicy(format!(
                "attachment[{index}]: grew past per-file cap while reading"
            )));
        }
        files.push(AttachmentFile { filename, bytes });
    }
    Ok(files)
}

/// Rechaza si la ruta o cualquiera de sus ancestros es un symlink. Un
/// ancestro inexistente no es symlink: se deja al `canonicalize` posterior
/// (que lo clasificará como `Missing`).
fn reject_symlink_components(path: &Path, index: usize) -> Result<(), AttachError> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(AttachError::PathPolicy(format!(
                    "attachment[{index}]: symlink component: {}",
                    ancestor.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(AttachError::Missing(format!(
                    "attachment[{index}]: unreadable ({err})"
                )));
            }
            Err(err) => {
                return Err(AttachError::PathPolicy(format!(
                    "attachment[{index}]: cannot inspect ({err})"
                )));
            }
        }
        if ancestor.parent().is_none() {
            break;
        }
    }
    Ok(())
}

fn deny_system_prefixes(canon: &Path, index: usize) -> Result<(), AttachError> {
    if DENIED_PREFIXES
        .iter()
        .any(|p| canon == Path::new(p) || canon.starts_with(format!("{p}/")))
    {
        return Err(AttachError::PathPolicy(format!(
            "attachment[{index}]: system path denied: {}",
            canon.display()
        )));
    }
    Ok(())
}

fn require_root_prefix(
    canon: &Path,
    canon_roots: &[PathBuf],
    index: usize,
) -> Result<(), AttachError> {
    if canon_roots.iter().any(|root| canon.starts_with(root)) {
        return Ok(());
    }
    Err(AttachError::PathPolicy(format!(
        "attachment[{index}]: outside attachment roots: {}",
        canon.display()
    )))
}

/// Canoniza las roots; las inexistentes se descartan (entonces ningún fichero
/// matchea: deny-all). Nunca falla: una configuración rota deniega, no abre.
fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Directorio temporal único sin dependencias extra.
    fn tempdir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "worker-att-{}-{}-{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn valid_file_loads() {
        let root = tempdir("ok");
        let p = write(&root, "a.pdf", b"hola");
        let files = load(std::slice::from_ref(&root), &[p]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "a.pdf");
        assert_eq!(files[0].bytes, b"hola");
    }

    #[test]
    fn missing_file_is_missing_not_policy() {
        let root = tempdir("miss");
        let absent = root.join("no-existe.pdf").to_string_lossy().into_owned();
        let err = load(std::slice::from_ref(&root), &[absent]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.attachment_missing");
    }

    #[test]
    fn outside_root_is_policy_violation() {
        let root = tempdir("root");
        let other = tempdir("other");
        let p = write(&other, "b.pdf", b"x");
        let err = load(std::slice::from_ref(&root), &[p]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
    }

    #[test]
    fn etc_passwd_is_denied_even_with_root_slash() {
        for evil in [
            "/etc/hostname",
            "/proc/self/environ",
            "/sys/kernel",
            "/dev/null",
        ] {
            let err = load(&[PathBuf::from("/")], &[evil.to_owned()]).unwrap_err();
            assert_eq!(
                err.code(),
                "pilotwave_worker.path_policy_violation",
                "path={evil}"
            );
        }
    }

    #[test]
    fn direct_file_symlink_is_rejected() {
        let root = tempdir("filelink");
        let target = write(&root, "real.pdf", b"x");
        let link = root.join("alias.pdf");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = load(
            std::slice::from_ref(&root),
            &[link.to_string_lossy().into_owned()],
        )
        .unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
        assert!(err.detail().contains("symlink"));
    }

    #[test]
    fn empty_roots_deny_everything_fail_closed() {
        let root = tempdir("denyall");
        let p = write(&root, "a.pdf", b"x");
        let err = load(&[], &[p]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
    }

    #[test]
    fn symlink_component_is_rejected() {
        let root = tempdir("link");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        write(&real, "a.pdf", b"x");
        let link = root.join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let evil = link.join("a.pdf").to_string_lossy().into_owned();
        let err = load(std::slice::from_ref(&root), &[evil]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
        assert!(err.detail().contains("symlink"));
    }

    #[test]
    fn traversal_outside_root_is_rejected() {
        let root = tempdir("trav");
        let sibling = root.join("hermano.pdf");
        std::fs::write(&sibling, b"x").unwrap();
        // `root/sub/../../hermano.pdf` canoniza fuera de `root/sub` si esa
        // fuera la root; con root=tmpdir pero path con `..` que sale: usar
        // una subdir como root para que el `..` escape de verdad.
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let evil = format!("{}/../hermano.pdf", sub.display());
        let err = load(std::slice::from_ref(&sub), &[evil]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
    }

    #[test]
    fn relative_path_is_rejected() {
        let root = tempdir("rel");
        let err = load(std::slice::from_ref(&root), &["relativo.pdf".to_owned()]).unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
    }

    #[test]
    fn real_size_over_cap_is_rejected() {
        let root = tempdir("big");
        let p = write(&root, "g.pdf", b"declaro poco");
        // Declared sizes ya no viajan hasta aquí: lo real manda. Simular
        // fichero grande sin escribir 6 MB: truncar con sparse.
        let big = root.join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(f);
        let _ = p;
        let err = load(
            std::slice::from_ref(&root),
            &[big.to_string_lossy().into_owned()],
        )
        .unwrap_err();
        assert_eq!(err.code(), "pilotwave_worker.path_policy_violation");
    }
}
