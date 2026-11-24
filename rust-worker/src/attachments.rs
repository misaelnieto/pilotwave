//! On-disk attachment policy.
//!
//! `payload::parse` already validated the *declared* shape and sizes; here the
//! *real* values are validated before opening each file, in this order:
//! 1. path charset (no `\r \n \0`; defense in depth on top of parsing),
//!    absolute path (relative ones would depend on the cwd);
//! 2. no component is a symlink (`symlink_metadata` per ancestor);
//! 3. `canonicalize` (`ENOENT`/`EACCES` -> `Missing`, anything else -> `PathPolicy`);
//! 4. the canonicalized path is not under `/proc /sys /dev /etc`;
//! 5. the canonicalized path sits under some canonicalized root;
//! 6. real size (`metadata`) within caps, with accumulated total;
//! 7. read with `take(MAX+1)`: when the file grew between `stat` and `read`
//!    (local TOCTOU), it is rejected instead of loading too much.
//!
//! Everything here is permanent: `PathPolicy` ->
//! `pilotwave_worker.path_policy_violation`, `Missing` ->
//! `pilotwave_worker.attachment_missing`. Never retried.
//!
//! TOCTOU note: the check is check-then-read (best-effort). That is enough
//! because the threat is a planted DB row, not a local process racing the
//! worker; an attacker with local execution already owns the FS.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::contract::{MAX_FILE_BYTES, MAX_TOTAL_BYTES};

/// File ready to attach: base name + already-bounded bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFile {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// Already-classified policy error.
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

/// Sensitive directories: denied even when a misconfigured root would
/// include them (defense in depth).
const DENIED_PREFIXES: &[&str] = &["/proc", "/sys", "/dev", "/etc"];

/// Load and validate every attachment. `paths` comes from `EmailContent`
/// (shape already validated by `payload::parse`).
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

        // `take` + 1: when the file grew after `stat`, it is detected here.
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

/// Reject when the path or any of its ancestors is a symlink. A missing
/// ancestor is not a symlink: leave it to the later `canonicalize`
/// (which will classify it as `Missing`).
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

/// Canonicalize the roots; missing ones are dropped (then no file matches:
/// deny-all). Never fails: a broken configuration denies, never opens.
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

    /// Unique temp dir with no extra dependencies.
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
        // Declared sizes never reach this point: real bytes rule. Simulate
        // a big file without writing 6 MB: truncate sparse.
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
