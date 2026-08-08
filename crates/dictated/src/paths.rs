//! Runtime paths, and the rule that keeps two daemons off each other's toes.
//!
//! The Python reference daemon stays installed and runnable until the v1.0
//! parity gate, so for the whole of this rebuild there are two programs on this
//! machine that both want to be "the dictation daemon". Today they *already*
//! collide: both write `~/.config/dictate-agent/dictate.pid` and both open
//! `~/.local/share/dictate-agent/history.db`. Whoever starts last wins the PID
//! file, and `dictate-toggle` then signals whichever process that was.
//!
//! This module gives `dictated` its own identity everywhere it keeps runtime
//! state:
//!
//! | | `dictated` (Rust) | reference daemon (Python) |
//! |---|---|---|
//! | socket | `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock` | *(none)* |
//! | PID | `$XDG_RUNTIME_DIR/dictate-agent/dictated.pid` | `~/.config/dictate-agent/dictate.pid` |
//! | history DB | `$XDG_DATA_HOME/dictated/history.db` | `~/.local/share/dictate-agent/history.db` |
//!
//! The PID file moves to `$XDG_RUNTIME_DIR` rather than just changing its name
//! in `~/.config`, because that is where a PID file belongs: the runtime
//! directory is cleared on logout, so a PID file left by a crashed daemon
//! cannot outlive the boot that made it meaningless.
//!
//! # The legacy PID file, and why `dictate-toggle` still works
//!
//! `scripts/dictate-toggle` reads the legacy path and signals what it finds
//! there. It must keep working unchanged, and it must not become a way for one
//! daemon to signal the other.
//!
//! [`PidFile::claim_legacy`] resolves that with **claim-if-free**: `dictated`
//! takes the legacy path only when nothing live already holds it, and releases
//! it only if it still owns it. So:
//!
//! - Python running, `dictated` starts → legacy file is live, `dictated` leaves
//!   it alone. `dictate-toggle` drives Python, which is correct: Python owns it.
//! - `dictated` running alone → it holds the legacy file, and `dictate-toggle`
//!   drives `dictated` through the signal shim. Jake's keybinding is unchanged.
//! - Either one crashes → the survivor's next start finds a stale PID and
//!   claims the file.
//!
//! Neither process ever overwrites a PID file that a living process owns, which
//! is the property that matters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Directory name shared by both daemons for user-visible config.
const CONFIG_DIR: &str = "dictate-agent";
/// Subdirectory of the runtime dir. Shared with the reference daemon's
/// namespace on purpose: the *files* are distinct, and one directory keeps
/// `ls $XDG_RUNTIME_DIR` legible.
const RUNTIME_DIR: &str = "dictate-agent";
/// The new daemon's socket.
const SOCKET_FILE: &str = "dictated.sock";
/// The new daemon's PID file.
const PID_FILE: &str = "dictated.pid";
/// The PID file `scripts/dictate-toggle` and `scripts/dictate-cancel` read.
const LEGACY_PID_FILE: &str = "dictate.pid";
/// Data subdirectory for the new daemon's history database.
const DATA_DIR: &str = "dictated";

/// Where the daemon keeps its runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    /// The control-plane socket.
    pub socket: PathBuf,
    /// This daemon's PID file.
    pub pid: PathBuf,
    /// The PID file the legacy toggle scripts read.
    pub legacy_pid: PathBuf,
}

impl RuntimePaths {
    /// Resolve paths from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let runtime = runtime_dir();
        Self {
            socket: runtime.join(SOCKET_FILE),
            pid: runtime.join(PID_FILE),
            legacy_pid: config_dir().join(LEGACY_PID_FILE),
        }
    }

    /// Resolve paths rooted at `base`, for tests and for running two daemons
    /// side by side deliberately.
    #[must_use]
    pub fn under(base: &Path) -> Self {
        Self {
            socket: base.join(SOCKET_FILE),
            pid: base.join(PID_FILE),
            legacy_pid: base.join(LEGACY_PID_FILE),
        }
    }

    /// Create the directories these paths live in.
    ///
    /// # Errors
    ///
    /// If a parent directory cannot be created.
    pub fn ensure_dirs(&self) -> Result<()> {
        for path in [&self.socket, &self.pid, &self.legacy_pid] {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        Ok(())
    }
}

/// `$XDG_RUNTIME_DIR/dictate-agent`, falling back to a per-user `/tmp` path on
/// systems that do not set it (a bare `ssh` session, most containers).
#[must_use]
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Per-user rather than shared, so two accounts on one box do not
            // fight over the same socket path.
            let uid = std::env::var("UID").unwrap_or_else(|_| "user".into());
            PathBuf::from(format!("/tmp/dictate-agent-{uid}"))
        })
        .join(RUNTIME_DIR)
}

/// `$XDG_CONFIG_HOME/dictate-agent`, shared with the reference daemon.
#[must_use]
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join(CONFIG_DIR)
}

/// The new daemon's default history database.
///
/// Deliberately *not* the reference daemon's `history.db`. The schema is the
/// same and sharing would technically work under WAL, but a shared database
/// makes "which daemon produced this latency number" unanswerable — and S12's
/// parity measurements depend on being able to answer it. Point `history.db_path`
/// at the legacy file if you want them merged.
#[must_use]
pub fn default_history_db() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join(DATA_DIR)
        .join("history.db")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Whether a process with this id exists.
///
/// `kill(pid, 0)` is the portable liveness probe: it performs the permission
/// checks and then returns without delivering a signal.
///
/// # The zero that is not a process
///
/// `kill(0, sig)` does **not** mean "process 0" — POSIX defines it as *every
/// process in the caller's process group*, so it succeeds unconditionally.
/// Left unguarded, a PID file containing `0` (a truncated write, or a file
/// zeroed by a crash) would read as a live daemon and lock the real one out of
/// its own socket forever. Only strictly positive ids are real processes here.
#[must_use]
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 has no effect beyond returning whether the
    // process exists and is signalable. `pid` is strictly positive, so this
    // cannot address a process group. No memory is touched.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Read a PID file, returning the id only if it names a living process.
///
/// A file holding a dead PID reads as `None`, which is what makes a crashed
/// daemon's leftovers reclaimable rather than a permanent lockout.
#[must_use]
pub fn live_pid_in(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    process_is_alive(pid).then_some(pid)
}

/// An owned PID file, removed on drop.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
    legacy: Option<PathBuf>,
    pid: u32,
}

impl PidFile {
    /// Take ownership of `path`, refusing if a living daemon already holds it.
    ///
    /// # Errors
    ///
    /// If another live process owns the file, or the file cannot be written.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(existing) = live_pid_in(path) {
            anyhow::bail!(
                "dictated is already running as pid {existing} (per {})",
                path.display()
            );
        }
        let pid = std::process::id();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, pid.to_string())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            legacy: None,
            pid,
        })
    }

    /// Also claim the legacy PID file, but only if nothing living holds it.
    ///
    /// This is what keeps `dictate-toggle` working against `dictated` without
    /// ever stealing the reference daemon's control channel. Returns whether
    /// the claim succeeded; a refusal is normal, not an error.
    pub fn claim_legacy(&mut self, path: &Path) -> bool {
        if let Some(existing) = live_pid_in(path) {
            tracing::info!(
                "legacy PID file {} is held by live pid {existing}; \
                 leaving it alone (dictate-toggle will drive that process)",
                path.display()
            );
            return false;
        }
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        if std::fs::write(path, self.pid.to_string()).is_err() {
            return false;
        }
        tracing::info!(
            "claimed legacy PID file {} — dictate-toggle/dictate-cancel now drive this daemon",
            path.display()
        );
        self.legacy = Some(path.to_path_buf());
        true
    }

    /// The pid recorded in the file.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Release both files, removing each only if we still own it.
    pub fn release(&mut self) {
        for path in [Some(&self.path), self.legacy.as_ref()].into_iter().flatten() {
            // Re-read before deleting: if another daemon reclaimed the file
            // while we were running, removing it would strand *them*.
            let ours = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .is_some_and(|p| p == self.pid);
            if ours {
                let _ = std::fs::remove_file(path);
            }
        }
        self.legacy = None;
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dictated-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_new_daemon_never_shares_a_path_with_the_python_daemon() {
        let p = RuntimePaths::from_env();
        let python_pid = config_dir().join("dictate.pid");
        let python_db = home().join(".local/share/dictate-agent/history.db");

        assert_ne!(p.pid, python_pid, "PID files must not collide");
        assert_ne!(p.socket, python_pid);
        assert_ne!(default_history_db(), python_db, "history DBs must not collide");
        assert!(p.pid.ends_with("dictated.pid"));
        assert!(p.socket.ends_with("dictated.sock"));
        // The legacy path is *known* to the daemon, but only as something it
        // may claim when free — never as its own identity.
        assert_eq!(p.legacy_pid, python_pid);
    }

    #[test]
    fn acquire_writes_our_pid_and_drop_removes_it() {
        let dir = tempdir("acquire");
        let path = dir.join("dictated.pid");
        {
            let f = PidFile::acquire(&path).unwrap();
            assert_eq!(f.pid(), std::process::id());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap().trim(),
                std::process::id().to_string()
            );
        }
        assert!(!path.exists(), "the PID file must not outlive the daemon");
    }

    #[test]
    fn acquire_refuses_a_pid_file_held_by_a_live_process() {
        let dir = tempdir("refuse");
        let path = dir.join("dictated.pid");
        // Our own pid is, definitionally, alive.
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        let err = PidFile::acquire(&path).unwrap_err();
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn acquire_reclaims_a_stale_pid_file() {
        let dir = tempdir("stale");
        let path = dir.join("dictated.pid");
        // PID 0 is never a live user process, so this stands in for the
        // leftovers of a daemon that was killed.
        std::fs::write(&path, "0").unwrap();
        let f = PidFile::acquire(&path).expect("a stale PID file must not lock us out");
        assert_eq!(f.pid(), std::process::id());
    }

    #[test]
    fn garbage_in_a_pid_file_is_not_a_live_process() {
        let dir = tempdir("garbage");
        let path = dir.join("dictated.pid");
        std::fs::write(&path, "not-a-pid\n").unwrap();
        assert_eq!(live_pid_in(&path), None);
        assert!(PidFile::acquire(&path).is_ok());
    }

    #[test]
    fn the_legacy_file_is_claimed_when_free() {
        let dir = tempdir("legacy-free");
        let mut f = PidFile::acquire(&dir.join("dictated.pid")).unwrap();
        let legacy = dir.join("dictate.pid");
        assert!(f.claim_legacy(&legacy));
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap().trim(),
            std::process::id().to_string(),
            "dictate-toggle must find this daemon"
        );
    }

    #[test]
    fn the_legacy_file_is_left_alone_when_another_daemon_holds_it() {
        // The constraint that matters: the Python daemon is running, and
        // `dictated` must not hijack the signal channel it owns.
        let dir = tempdir("legacy-held");
        let legacy = dir.join("dictate.pid");
        std::fs::write(&legacy, std::process::id().to_string()).unwrap();
        let held_by = std::fs::read_to_string(&legacy).unwrap();

        let mut f = PidFile::acquire(&dir.join("dictated.pid")).unwrap();
        assert!(!f.claim_legacy(&legacy), "must not steal a live PID file");
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            held_by,
            "the other daemon's PID file must be byte-identical afterwards"
        );
    }

    #[test]
    fn releasing_does_not_delete_a_file_another_daemon_reclaimed() {
        let dir = tempdir("reclaim");
        let path = dir.join("dictated.pid");
        let mut f = PidFile::acquire(&path).unwrap();
        // Someone else took over while we were running.
        std::fs::write(&path, "999999").unwrap();
        f.release();
        assert!(
            path.exists(),
            "releasing must not strand the daemon that reclaimed the file"
        );
    }

    #[test]
    fn release_is_idempotent() {
        let dir = tempdir("idempotent");
        let path = dir.join("dictated.pid");
        let mut f = PidFile::acquire(&path).unwrap();
        f.release();
        f.release();
        assert!(!path.exists());
    }

    #[test]
    fn paths_under_a_base_are_all_distinct() {
        let p = RuntimePaths::under(Path::new("/tmp/x"));
        assert_ne!(p.socket, p.pid);
        assert_ne!(p.pid, p.legacy_pid);
        assert_ne!(p.socket, p.legacy_pid);
    }

    #[test]
    fn our_own_process_is_alive_and_pid_zero_is_not() {
        assert!(process_is_alive(std::process::id()));
        assert!(
            !process_is_alive(0),
            "kill(0, 0) signals our own process group and always succeeds; \
             a zeroed PID file must not read as a live daemon"
        );
    }

    #[test]
    fn a_zeroed_pid_file_is_reclaimable_rather_than_a_permanent_lockout() {
        let dir = tempdir("zeroed");
        let path = dir.join("dictated.pid");
        std::fs::write(&path, "0").unwrap();
        assert_eq!(live_pid_in(&path), None);
        assert!(PidFile::acquire(&path).is_ok());
    }
}
