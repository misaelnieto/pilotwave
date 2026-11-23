//! Lecturas best-effort de `/proc` para la sección daemon de `status`.
//!
//! v1 sin CPU (decisión de usuario): solo PID, uptime y RAM (VmRSS).
//! Todo lo que toca `/proc` devuelve `Option` (`None` = ilegible o
//! truncado; el caller pinta `n/a` y sigue con exit 0). Las funciones
//! `parse_*` son puras para poder probarlas sin `/proc`.

use std::time::{SystemTime, UNIX_EPOCH};

/// VmRSS del proceso en kB (`/proc/<pid>/status`, línea `VmRSS: <n> kB`).
/// `None` si el archivo falta, es ilegible o no trae la línea.
pub fn read_vmrss_kb(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_vmrss_kb(&text)
}

/// Función pura tras la lectura: busca `VmRSS: <n> kB`.
pub(crate) fn parse_vmrss_kb(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // `VmRSS:` aparece una sola vez en `/proc/<pid>/status`; si su
            // número no parsea, el archivo está truncado => None.
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

/// Campo `starttime` (n.º 22) de `/proc/<pid>/stat`, en ticks de reloj
/// desde el arranque. `None` si el archivo falta o está truncado.
///
/// OJO: `comm` (campo 2, entre paréntesis) puede contener espacios y
/// paréntesis; por eso se busca el ÚLTIMO `)` y se cuenta desde ahí
/// (starttime = campo 22 global = índice 20 tras `)`).
pub fn read_starttime_ticks(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_starttime_ticks(&text)
}

/// Función pura tras la lectura.
pub(crate) fn parse_starttime_ticks(text: &str) -> Option<u64> {
    let after = text.rfind(')')?;
    let fields: Vec<&str> = text[after + 1..].split_whitespace().collect();
    // Campos 3..=22 tras `)`: 20 tokens; starttime es el último (índice 19).
    if fields.len() < 20 {
        return None;
    }
    fields[19].parse::<u64>().ok()
}

/// Segundos epoch del arranque (`/proc/stat`, línea `btime <n>`).
/// `None` si el archivo falta o no trae la línea.
pub fn boot_time_secs() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    parse_boot_time(&text)
}

/// Función pura tras la lectura.
pub(crate) fn parse_boot_time(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

/// Ticks de reloj por segundo (`sysconf(_SC_CLK_TCK)`); 100 si falla.
pub fn clk_ticks_per_sec() -> u64 {
    // SAFETY: `sysconf` con `_SC_CLK_TCK` es pura consulta, sin efectos.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 { 100 } else { hz as u64 }
}

/// Segundos desde que arrancó `pid`: `now - (btime + starttime/hz)`.
/// Saturante; `None` si cualquier lectura falla (best-effort => `n/a`).
pub fn uptime_secs(pid: u32) -> Option<u64> {
    let start_ticks = read_starttime_ticks(pid)?;
    let boot = boot_time_secs()?;
    let start_epoch = boot.saturating_add(start_ticks / clk_ticks_per_sec());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(start_epoch))
}

/// `45s` / `2m5s` / `1h2m3s` (`3d1h2m3s` si aplica). Sin dependencias.
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let rest = secs % 60;
    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{days}d"));
    }
    if hours > 0 || days > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if mins > 0 || hours > 0 || days > 0 {
        out.push_str(&format!("{mins}m"));
    }
    out.push_str(&format!("{rest}s"));
    out
}

/// `4192 kB (4.1 MB)`: entero exacto + decimal aproximado.
pub fn format_rss(kb: u64) -> String {
    format!("{kb} kB ({:.1} MB)", kb as f64 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmrss_parsea_kb_y_rechaza_ausente_o_basura() {
        assert_eq!(
            parse_vmrss_kb("Name:\trust\nVmRSS:\t    4192 kB\nThreads:\t1\n"),
            Some(4192)
        );
        assert_eq!(parse_vmrss_kb("Name:\trust\nThreads:\t1\n"), None);
        assert_eq!(parse_vmrss_kb("VmRSS:\t   no-num kB\n"), None);
        assert_eq!(parse_vmrss_kb(""), None);
    }

    #[test]
    fn starttime_cuenta_desde_el_ultimo_parentesis() {
        // Campos 3..=22 tras `)`: starttime (22) = token 20 => valor 2200.
        let after: Vec<String> = (3..=22).map(|n| (n * 100).to_string()).collect();
        let stat = format!("1234 (worker) {}", after.join(" "));
        assert_eq!(parse_starttime_ticks(&stat), Some(2200));
        // comm con espacios y paréntesis: solo el ÚLTIMO `)` vale.
        let stat = format!("1234 (my app (test) v2) {}", after.join(" "));
        assert_eq!(parse_starttime_ticks(&stat), Some(2200));
        // Truncado (menos de 20 tokens tras `)`): None, no panic.
        assert_eq!(parse_starttime_ticks("1234 (x) R 1 2"), None);
        assert_eq!(parse_starttime_ticks("sin parentesis"), None);
    }

    #[test]
    fn boot_time_parsea_btime() {
        assert_eq!(
            parse_boot_time("cpu  1 2 3\nbtime 1763000000\nintr 4\n"),
            Some(1_763_000_000)
        );
        assert_eq!(parse_boot_time("cpu  1 2 3\n"), None);
    }

    #[test]
    fn format_uptime_casos() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(125), "2m5s");
        assert_eq!(format_uptime(3723), "1h2m3s");
        assert_eq!(format_uptime(90_061), "1d1h1m1s");
    }

    #[test]
    fn format_rss_muestra_kb_y_mb() {
        assert_eq!(format_rss(4192), "4192 kB (4.1 MB)");
        assert_eq!(format_rss(0), "0 kB (0.0 MB)");
    }

    #[test]
    fn lecturas_del_propio_pid_funcionan() {
        let pid = std::process::id();
        assert!(read_vmrss_kb(pid).is_some());
        assert!(read_starttime_ticks(pid).is_some());
        assert!(boot_time_secs().is_some());
        assert!(clk_ticks_per_sec() > 0);
        // Recién arrancado en el test: uptime pequeño pero definido.
        assert!(uptime_secs(pid).is_some());
    }

    #[test]
    fn pid_inexistente_da_none() {
        assert_eq!(read_vmrss_kb(u32::MAX), None);
        assert_eq!(read_starttime_ticks(u32::MAX), None);
        assert_eq!(uptime_secs(u32::MAX), None);
    }
}
