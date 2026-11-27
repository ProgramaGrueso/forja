//! Utilidades de procesos: grupos de procesos de los agentes e identidad de
//! procesos para reconciliar ejecuciones tras un apagado.
//!
//! Cada agente se lanza en su propio grupo de procesos (`setpgid(0, 0)`): así
//! cancelar, agotar el tiempo o terminar mata también a sus hijos y nietos.

/// Mata con SIGKILL el grupo de procesos `pgid`.
///
/// Nunca actúa sobre los grupos 0 o 1 ni sobre el grupo o el pid del propio
/// proceso de Forja (ni, por tanto, sobre la shell que lo lanzó). Devuelve `true`
/// si la señal se envió.
pub fn kill_process_group(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pgid) = libc::pid_t::try_from(pgid) else {
            return false;
        };
        if pgid <= 1 {
            return false;
        }
        // SAFETY: llamadas POSIX sin punteros ni estado compartido.
        let (own_pgrp, own_pid) = unsafe { (libc::getpgrp(), libc::getpid()) };
        if pgid == own_pgrp || pgid == own_pid {
            return false;
        }
        // SAFETY: `killpg` con un pgid > 1 que no es el nuestro.
        unsafe { libc::killpg(pgid, libc::SIGKILL) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Estado y momento de arranque del proceso (campos 3 y 22 de
/// `/proc/<pid>/stat`). El arranque (ticks desde el boot) distingue un pid
/// reciclado del proceso original.
fn proc_stat(pid: u32) -> Option<(char, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` va entre paréntesis y puede contener espacios: cortar tras el último ')'.
    let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace();
    let state = fields.next()?.chars().next()?;
    Some((state, fields.nth(18)?.parse().ok()?))
}

/// Identidad estable de un proceso: `pid:arranque` (`pid:0` sin `/proc`).
pub fn process_identity(pid: u32) -> String {
    let start = proc_stat(pid).map_or(0, |(_, ticks)| ticks);
    format!("{pid}:{start}")
}

/// Identidad del proceso actual.
pub fn current_process_identity() -> String {
    process_identity(std::process::id())
}

/// `true` si el proceso de `identity` (ver [`process_identity`]) sigue vivo (un
/// zombi ya terminó) y no es otro proceso que reutilizó el mismo pid.
pub fn identity_alive(identity: &str) -> bool {
    let Some((pid, start)) = identity.split_once(':') else {
        return false;
    };
    let (Ok(pid), Ok(start)) = (pid.parse::<u32>(), start.parse::<u64>()) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    if std::path::Path::new("/proc/self/stat").exists() {
        return proc_stat(pid).is_some_and(|(state, ticks)| {
            !matches!(state, 'Z' | 'X' | 'x') && (start == 0 || ticks == start)
        });
    }
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: señal 0 solo comprueba existencia.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Uid efectivo del proceso actual (None fuera de Unix).
pub fn current_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` no falla ni toca memoria.
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_self_is_alive_and_bogus_ones_are_not() {
        let me = current_process_identity();
        assert!(identity_alive(&me), "{me}");
        assert!(!identity_alive("0:0"));
        assert!(!identity_alive("basura"));
        let (pid, _) = me.split_once(':').unwrap();
        // Mismo pid con otro arranque: pid reciclado, no es el dueño.
        assert!(!identity_alive(&format!("{pid}:1")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_is_not_alive() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let identity = process_identity(child.id());
        assert!(identity_alive(&identity), "{identity}");
        child.kill().expect("kill");
        // Sin `wait` queda zombi: su entrada en /proc sigue ahí.
        let zombie = (0..200).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            proc_stat(child.id()).is_some_and(|(state, _)| state == 'Z')
        });
        let alive = identity_alive(&identity);
        child.wait().expect("wait");
        assert!(zombie, "no llegó a zombi");
        assert!(!alive, "un zombi no es un dueño vivo");
    }

    #[test]
    fn never_kills_own_group_or_init() {
        assert!(!kill_process_group(0));
        assert!(!kill_process_group(1));
        assert!(!kill_process_group(std::process::id()));
        #[cfg(unix)]
        {
            let own = unsafe { libc::getpgrp() } as u32;
            assert!(!kill_process_group(own));
        }
    }
}
