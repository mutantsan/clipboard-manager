// ============================================================================
// SINGLE-INSTANCE UI LOCK
// ============================================================================
//
// Pressing the hotkey (Super+V) runs `trigger.sh`, which spawns a terminal
// running `clipboard-manager --ui`. Without a guard, hammering the hotkey opens
// N stacked windows. This module lets the UI process claim an exclusive lock so
// only one window is ever live; later launches detect the running instance,
// nudge focus back to it, and exit.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::UI_LOCK_FILE;

/// RAII guard for the UI lock file. Dropping it removes the file, so the lock is
/// released on normal exit, on `?` propagation, and while unwinding a panic.
pub struct UiLock {
    path: PathBuf,
}

impl Drop for UiLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Outcome of trying to become the sole UI instance.
pub enum UiLockResult {
    /// This process now owns the lock; hold the guard for its whole lifetime.
    Acquired(UiLock),
    /// Another live UI instance already holds the lock; the caller should exit.
    AlreadyRunning,
    /// The lock file could not be used (e.g. permissions); proceed without it.
    Unavailable,
}

fn pid_is_alive(pid: i32) -> bool {
    pid > 0 && Path::new(&format!("/proc/{pid}")).exists()
}

/// Try to claim the single-instance UI lock stored in `data_dir`.
pub fn acquire_ui_lock(data_dir: &Path) -> UiLockResult {
    let path = data_dir.join(UI_LOCK_FILE);

    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = write!(file, "{}", std::process::id());
                return UiLockResult::Acquired(UiLock { path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A lock file exists. Honour it only if the recorded PID is a
                // live process; otherwise it is stale (e.g. the UI was killed)
                // and we clear it and retry.
                let holder_alive = fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .map(pid_is_alive)
                    .unwrap_or(false);

                if holder_alive {
                    return UiLockResult::AlreadyRunning;
                }

                if fs::remove_file(&path).is_err() {
                    return UiLockResult::Unavailable;
                }
                // Loop: another starter may win the race on the next create_new.
            }
            Err(_) => return UiLockResult::Unavailable,
        }
    }
}

/// Best-effort: pull focus back to the already-open clipboard window. Only
/// Hyprland is handled (the documented target); elsewhere this is a no-op.
pub fn focus_existing_ui() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }

    // Hyprland's Lua config (e.g. Omarchy) rejects the classic
    // `dispatch focuswindow class:...` syntax and errors out, so try the Lua
    // dispatcher first and fall back to the classic one for stock Hyprland.
    let lua_focused = Command::new("hyprctl")
        .arg("dispatch")
        .arg(r#"hl.dsp.focus({ window = "class:floating-clipboard" })"#)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !lua_focused {
        let _ = Command::new("hyprctl")
            .args(["dispatch", "focuswindow", "class:floating-clipboard"])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_blocked_then_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("cm-lock-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let first = match acquire_ui_lock(&dir) {
            UiLockResult::Acquired(g) => g,
            _ => panic!("first acquire should succeed"),
        };

        assert!(matches!(
            acquire_ui_lock(&dir),
            UiLockResult::AlreadyRunning
        ));

        drop(first);

        assert!(matches!(
            acquire_ui_lock(&dir),
            UiLockResult::Acquired(_)
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_lock_from_dead_pid_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!("cm-lock-stale-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // PID 2^31-1 is effectively guaranteed not to be a running process.
        fs::write(dir.join(UI_LOCK_FILE), "2147483647").unwrap();

        assert!(matches!(
            acquire_ui_lock(&dir),
            UiLockResult::Acquired(_)
        ));

        let _ = fs::remove_dir_all(&dir);
    }
}
