//! How this `dictated` was started (#5725).
//!
//! The supported launch is `systemctl --user start dictated`. The incident
//! behind this module was a bare `dictated` typed in a terminal: it ran in the
//! terminal's login scope, read no config file (the built-in defaults), and
//! started its own Ollama beside a unit that was inactive. Nothing said so.
//!
//! This only warns. `scripts/smoke-real.sh`, the e2e tests and
//! `tests/control_plane.rs` start isolated daemons on purpose; such a daemon
//! names its own socket (`DICTATE_SOCKET`), which is how it is told apart.

use std::path::{Path, PathBuf};

use dictate_core::config::ConfigReport;

/// The user unit that is the supported launch.
pub const UNIT: &str = "dictated.service";

/// What was observed about this process. Plain data, so the verdict is a pure
/// function with tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFacts {
    /// `/proc/self/cgroup`, when readable.
    pub cgroup: Option<String>,
    /// A `dictated.service` user unit file exists.
    pub unit_installed: bool,
    /// The process named its own socket: a deliberate private daemon.
    pub isolated: bool,
    /// The config file this run loaded (or would have).
    pub config_path: PathBuf,
    pub config_existed: bool,
    /// The user's config file: `$XDG_CONFIG_HOME/dictate-agent/config.toml`.
    pub user_config_path: PathBuf,
}

impl LaunchFacts {
    /// Observe this process. `unit_dirs` are searched for [`UNIT`].
    #[must_use]
    pub fn detect(report: &ConfigReport, unit_dirs: &[PathBuf], cgroup_file: &Path) -> Self {
        Self {
            cgroup: std::fs::read_to_string(cgroup_file).ok(),
            unit_installed: unit_dirs.iter().any(|d| d.join(UNIT).exists()),
            isolated: std::env::var_os("DICTATE_SOCKET").is_some_and(|v| !v.is_empty()),
            config_path: report.path.clone(),
            config_existed: report.existed,
            user_config_path: dictate_core::config::config_dir().join("config.toml"),
        }
    }

    /// [`detect`](Self::detect) on the real machine.
    #[must_use]
    pub fn detect_here(report: &ConfigReport) -> Self {
        let mut dirs = Vec::new();
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            dirs.push(PathBuf::from(home).join(".config/systemd/user"));
        }
        dirs.push("/etc/systemd/user".into());
        dirs.push("/usr/lib/systemd/user".into());
        Self::detect(report, &dirs, Path::new("/proc/self/cgroup"))
    }

    /// Whether this process runs inside [`UNIT`]; `None` when unknown.
    fn in_unit(&self) -> Option<bool> {
        self.cgroup
            .as_ref()
            .map(|c| c.lines().any(|l| l.trim_end().ends_with(UNIT)))
    }
}

/// What deserves a loud line at startup. Empty for a normal launch.
#[must_use]
pub fn launch_notes(f: &LaunchFacts) -> Vec<String> {
    if f.isolated {
        return Vec::new();
    }
    let mut notes = Vec::new();
    let outside_unit = f.in_unit() == Some(false);
    if outside_unit && f.unit_installed {
        notes.push(format!(
            "dictated was started outside systemd while the user unit {UNIT} exists and is not running it; \
             a hand-started daemon runs on whatever config and environment this shell has. \
             Stop it and use `systemctl --user start dictated`, the only supported launch"
        ));
    }
    if !f.config_existed {
        notes.push(format!(
            "no config file at {}: running on the built-in defaults, not your settings",
            f.config_path.display()
        ));
    } else if outside_unit && f.config_path != f.user_config_path {
        notes.push(format!(
            "reading {} instead of your config file {}",
            f.config_path.display(),
            f.user_config_path.display()
        ));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> LaunchFacts {
        LaunchFacts {
            cgroup: Some(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/dictated.service\n"
                    .into(),
            ),
            unit_installed: true,
            isolated: false,
            config_path: "/home/u/.config/dictate-agent/config.toml".into(),
            config_existed: true,
            user_config_path: "/home/u/.config/dictate-agent/config.toml".into(),
        }
    }

    fn from_terminal() -> LaunchFacts {
        LaunchFacts {
            cgroup: Some("0::/user.slice/user-1000.slice/session-2.scope\n".into()),
            ..facts()
        }
    }

    #[test]
    fn the_supported_launch_says_nothing() {
        assert!(launch_notes(&facts()).is_empty());
    }

    #[test]
    fn a_bare_dictated_beside_the_unit_is_called_out() {
        let notes = launch_notes(&from_terminal());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("systemctl --user start dictated"),
            "{notes:?}"
        );
    }

    #[test]
    fn the_stray_on_built_in_defaults_names_both_problems() {
        let notes = launch_notes(&LaunchFacts {
            config_existed: false,
            ..from_terminal()
        });
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].contains("built-in defaults"), "{notes:?}");
    }

    #[test]
    fn another_config_file_outside_the_unit_is_named() {
        let notes = launch_notes(&LaunchFacts {
            config_path: "/tmp/other.toml".into(),
            unit_installed: false,
            ..from_terminal()
        });
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("/tmp/other.toml"), "{notes:?}");
    }

    #[test]
    fn an_isolated_daemon_is_left_alone() {
        let notes = launch_notes(&LaunchFacts {
            isolated: true,
            config_existed: false,
            ..from_terminal()
        });
        assert!(
            notes.is_empty(),
            "smoke and e2e daemons are deliberate: {notes:?}"
        );
    }

    #[test]
    fn an_unreadable_cgroup_is_not_an_accusation() {
        let notes = launch_notes(&LaunchFacts {
            cgroup: None,
            ..from_terminal()
        });
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn no_unit_installed_means_nothing_to_prefer() {
        let notes = launch_notes(&LaunchFacts {
            unit_installed: false,
            ..from_terminal()
        });
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn detection_reads_the_cgroup_and_unit_directories_it_is_given() {
        let dir = std::env::temp_dir().join(format!("launch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("units")).unwrap();
        std::fs::write(dir.join("units").join(UNIT), "[Service]\n").unwrap();
        std::fs::write(dir.join("cgroup"), "0::/user.slice/session-9.scope\n").unwrap();
        let f = LaunchFacts::detect(
            &ConfigReport::default(),
            &[dir.join("units")],
            &dir.join("cgroup"),
        );
        assert!(f.unit_installed);
        assert_eq!(f.in_unit(), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
