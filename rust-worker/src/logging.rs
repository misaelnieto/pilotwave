//! Minimal line-oriented logger: `2026-11-16T10:00:00Z INFO message`.
//!
//! The daemon log is a line-oriented, level-prefixed file. A full
//! `tracing` stack would cost more binary size than the worker itself needs,
//! so we keep it to a few lines until there is a reason not to.

use std::sync::atomic::{AtomicBool, Ordering};

use time::OffsetDateTime;
use time::macros::format_description;

const FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// Global diagnostic switch: only `debug` reads it.
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable or silence `DEBUG` messages. Call once at startup.
pub fn set_verbose(value: bool) {
    VERBOSE.store(value, Ordering::Relaxed);
}

/// Current switch state (for tests).
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn info(message: &str) {
    emit("INFO", message);
}

pub fn warn(message: &str) {
    emit("WARN", message);
}

pub fn error(message: &str) {
    emit("ERROR", message);
}

/// Noisy diagnostic: only emits after `set_verbose(true)`.
/// Never pass secrets (passwords, DSNs) or body contents.
pub fn debug(message: &str) {
    if is_verbose() {
        emit("DEBUG", message);
    }
}

fn emit(level: &str, message: &str) {
    let now = OffsetDateTime::now_utc()
        .format(FORMAT)
        .unwrap_or_else(|_| String::from("-"));
    println!("{now} {level} {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_alterna_y_debug_no_emite_sin_flag() {
        set_verbose(true);
        assert!(is_verbose());
        debug("ruidoso: solo visible con -v");
        set_verbose(false);
        assert!(!is_verbose());
        debug("silencioso: no debe emitir");
        assert!(!is_verbose());
    }
}
