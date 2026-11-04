//! What this machine says about Ollama, read from the filesystem only.
//!
//! Issue #5725: `dictated` started its own `ollama serve` while the system
//! `ollama.service` was down. That child inherited the user's environment, not
//! the unit's `OLLAMA_MODELS`, so it served `~/.ollama/models` (one embedding
//! model) while every chat model sat in `/var/lib/ollama/.ollama/models`, and
//! the formatter, then `dictate doctor`'s `ollama pull` advice, were both wrong.
//!
//! [`OllamaHost`] answers the questions behind the fix without ever running
//! `systemctl` or touching the network:
//!
//! - is there a managed `ollama.service` (system or user), so that dictated
//!   must not start a second server of its own;
//! - which model stores exist (the unit drop-in's `OLLAMA_MODELS` and the
//!   conventional locations), and does one of them hold a given model.
//!
//! Every directory is a field, so tests build a private host out of a temp
//! directory and never depend on this machine's real units or stores.

use std::path::{Path, PathBuf};

/// The unit file name Ollama's packages install.
pub const OLLAMA_UNIT: &str = "ollama.service";

/// Whether a unit belongs to the system manager or the user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitScope {
    System,
    User,
}

/// An `ollama.service` found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedUnit {
    pub scope: UnitScope,
    /// The unit file, or its `.d` drop-in directory when the file itself is
    /// not in the searched directories.
    pub path: PathBuf,
}

impl ManagedUnit {
    /// The command that starts it.
    #[must_use]
    pub fn start_command(&self) -> &'static str {
        match self.scope {
            UnitScope::System => "sudo systemctl start ollama",
            UnitScope::User => "systemctl --user start ollama",
        }
    }

    /// The command that restarts it.
    #[must_use]
    pub fn restart_command(&self) -> &'static str {
        match self.scope {
            UnitScope::System => "sudo systemctl restart ollama",
            UnitScope::User => "systemctl --user restart ollama",
        }
    }
}

/// Unit directories and model-store roots to consult. [`Default`] is empty:
/// no unit, no store. Production uses [`OllamaHost::system`].
#[derive(Debug, Clone, Default)]
pub struct OllamaHost {
    unit_dirs: Vec<(PathBuf, UnitScope)>,
    store_roots: Vec<PathBuf>,
}

impl OllamaHost {
    /// The real machine: the standard systemd unit directories and the
    /// conventional Ollama model stores.
    #[must_use]
    pub fn system() -> Self {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        Self::system_under(home.as_deref())
    }

    fn system_under(home: Option<&Path>) -> Self {
        let mut host = Self::default();
        for dir in [
            "/etc/systemd/system",
            "/run/systemd/system",
            "/usr/lib/systemd/system",
            "/lib/systemd/system",
        ] {
            host = host.with_unit_dir(dir, UnitScope::System);
        }
        if let Some(home) = home {
            host = host.with_unit_dir(home.join(".config/systemd/user"), UnitScope::User);
        }
        for dir in [
            "/etc/systemd/user",
            "/usr/lib/systemd/user",
            "/lib/systemd/user",
        ] {
            host = host.with_unit_dir(dir, UnitScope::User);
        }
        for root in [
            "/var/lib/ollama/.ollama/models",
            "/usr/share/ollama/.ollama/models",
        ] {
            host = host.with_store_root(root);
        }
        if let Some(home) = home {
            host = host.with_store_root(home.join(".ollama/models"));
        }
        host
    }

    /// Also look for `ollama.service` (and `ollama.service.d`) in `dir`.
    #[must_use]
    pub fn with_unit_dir(mut self, dir: impl Into<PathBuf>, scope: UnitScope) -> Self {
        self.unit_dirs.push((dir.into(), scope));
        self
    }

    /// Also look for models in the store at `root` (the directory that holds
    /// `manifests/` and `blobs/`).
    #[must_use]
    pub fn with_store_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.store_roots.push(root.into());
        self
    }

    /// The first managed `ollama.service` found, if any. A unit masked to
    /// `/dev/null` is not one.
    #[must_use]
    pub fn managed_unit(&self) -> Option<ManagedUnit> {
        for (dir, scope) in &self.unit_dirs {
            let file = dir.join(OLLAMA_UNIT);
            let masked = std::fs::read_link(&file).is_ok_and(|t| t == Path::new("/dev/null"));
            if !masked && file.exists() {
                return Some(ManagedUnit {
                    scope: *scope,
                    path: file,
                });
            }
            let dropins = dir.join(format!("{OLLAMA_UNIT}.d"));
            if !masked && dropins.is_dir() {
                return Some(ManagedUnit {
                    scope: *scope,
                    path: dropins,
                });
            }
        }
        None
    }

    /// Every `OLLAMA_MODELS` the unit files and drop-ins set, in directory
    /// order. `Environment=` lines are the only form read; an
    /// `EnvironmentFile=` is not followed.
    #[must_use]
    pub fn unit_model_stores(&self) -> Vec<PathBuf> {
        let mut stores = Vec::new();
        for (dir, _) in &self.unit_dirs {
            let mut files = vec![dir.join(OLLAMA_UNIT)];
            if let Ok(entries) = std::fs::read_dir(dir.join(format!("{OLLAMA_UNIT}.d"))) {
                let mut dropins: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "conf"))
                    .collect();
                dropins.sort();
                files.extend(dropins);
            }
            for file in files {
                if let Ok(text) = std::fs::read_to_string(&file) {
                    stores.extend(parse_ollama_models(&text));
                }
            }
        }
        stores
    }

    /// The unit's model stores first, then the conventional roots, without
    /// duplicates.
    #[must_use]
    pub fn model_stores(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for store in self
            .unit_model_stores()
            .into_iter()
            .chain(self.store_roots.iter().cloned())
        {
            if !out.contains(&store) {
                out.push(store);
            }
        }
        out
    }

    /// The first store that holds `model` (a manifest for it exists), if any.
    /// A store this process cannot read is skipped, not an error.
    #[must_use]
    pub fn find_model_store(&self, model: &str) -> Option<PathBuf> {
        let manifest = manifest_path(model)?;
        self.model_stores()
            .into_iter()
            .find(|store| store.join("manifests").join(&manifest).is_file())
    }

    /// When `model` is absent from the server at `endpoint` but present in
    /// another store: the sentence that names the real cause. It never says
    /// to download the model again.
    #[must_use]
    pub fn wrong_store_explanation(&self, model: &str, endpoint: &str) -> Option<String> {
        let store = self.find_model_store(model)?;
        let remedy = match self.managed_unit() {
            Some(unit) => format!(
                "stop whatever else holds that port (a stray `dictated` or `ollama serve`), then `{}`",
                unit.restart_command()
            ),
            None => format!(
                "start the server that reads that store: `OLLAMA_MODELS={} ollama serve`",
                store.display()
            ),
        };
        Some(format!(
            "'{model}' exists in the Ollama store {} but the server at {endpoint} reads a different store — {remedy}; do not download it again",
            store.display()
        ))
    }
}

/// The manifest path of `model` under a store's `manifests/` directory:
/// `gemma4:e4b` → `registry.ollama.ai/library/gemma4/e4b`.
fn manifest_path(model: &str) -> Option<PathBuf> {
    let model = model.trim();
    if model.is_empty() || model.contains("..") {
        return None;
    }
    // The tag follows the last colon, unless that colon is a registry port.
    let (name, tag) = match model.rsplit_once(':') {
        Some((n, t)) if !t.contains('/') => (n, t),
        _ => (model, "latest"),
    };
    let parts: Vec<&str> = name.split('/').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut path = PathBuf::new();
    match parts.as_slice() {
        [one] => {
            path.push("registry.ollama.ai");
            path.push("library");
            path.push(one);
        }
        [first, rest @ ..] if first.contains('.') || first.contains(':') => {
            path.push(first);
            path.extend(rest);
        }
        all => {
            path.push("registry.ollama.ai");
            path.extend(all);
        }
    }
    path.push(tag);
    Some(path)
}

/// `OLLAMA_MODELS` values set by `Environment=` lines of a unit file or drop-in.
fn parse_ollama_models(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("Environment=") else {
            continue;
        };
        for assignment in split_assignments(rest) {
            if let Some(value) = assignment.strip_prefix("OLLAMA_MODELS=") {
                let value = value.trim();
                if !value.is_empty() {
                    out.push(PathBuf::from(value));
                }
            }
        }
    }
    out
}

/// Split the right side of `Environment=` the way systemd does: words
/// separated by whitespace, each optionally in double quotes.
fn split_assignments(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                any = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if any || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        words.push(cur);
    }
    words
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A scratch directory that removes itself.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "oh-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }

        pub fn write(&self, rel: &str, text: &str) -> PathBuf {
            let p = self.path(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
            p
        }

        /// A model manifest in the store at `store_rel`.
        pub fn manifest(&self, store_rel: &str, model_path: &str) {
            self.write(
                &format!("{store_rel}/manifests/{model_path}"),
                "{\"schemaVersion\":2}",
            );
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Scratch;
    use super::*;

    #[test]
    fn model_names_map_to_manifest_paths() {
        let p = |m: &str| manifest_path(m).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(
            p("gemma4:e4b").as_deref(),
            Some("registry.ollama.ai/library/gemma4/e4b")
        );
        assert_eq!(
            p("nomic-embed-text").as_deref(),
            Some("registry.ollama.ai/library/nomic-embed-text/latest")
        );
        assert_eq!(
            p("someone/tool:7b").as_deref(),
            Some("registry.ollama.ai/someone/tool/7b")
        );
        assert_eq!(
            p("hf.co/bartowski/Lumimaid-v0.2-12B-GGUF:Q5_K_M").as_deref(),
            Some("hf.co/bartowski/Lumimaid-v0.2-12B-GGUF/Q5_K_M")
        );
        assert_eq!(p("").as_deref(), None);
        assert_eq!(p("../x:1").as_deref(), None);
    }

    #[test]
    fn the_drop_in_models_directory_is_read() {
        let text = "[Service]\nEnvironment=\"OLLAMA_MODELS=/var/lib/ollama/.ollama/models\"\n\
                    Environment=OLLAMA_HOST=127.0.0.1:11434 OLLAMA_MODELS=/data/models\n# Environment=OLLAMA_MODELS=/no\n";
        assert_eq!(
            parse_ollama_models(text),
            vec![
                PathBuf::from("/var/lib/ollama/.ollama/models"),
                PathBuf::from("/data/models")
            ]
        );
        assert!(parse_ollama_models("[Service]\nExecStart=/usr/bin/ollama serve\n").is_empty());
    }

    #[test]
    fn an_empty_host_has_no_unit_and_no_store() {
        let host = OllamaHost::default();
        assert!(host.managed_unit().is_none());
        assert!(host.find_model_store("gemma4:e4b").is_none());
    }

    #[test]
    fn a_unit_file_or_only_a_drop_in_directory_is_a_managed_unit() {
        let dir = Scratch::new("unit");
        let host = OllamaHost::default().with_unit_dir(dir.path("sys"), UnitScope::System);
        assert!(host.managed_unit().is_none());

        dir.write("sys/ollama.service.d/10-models.conf", "[Service]\n");
        let unit = host.managed_unit().expect("a drop-in dir counts");
        assert_eq!(unit.scope, UnitScope::System);
        assert_eq!(unit.start_command(), "sudo systemctl start ollama");

        let user = OllamaHost::default().with_unit_dir(dir.path("usr"), UnitScope::User);
        dir.write("usr/ollama.service", "[Service]\n");
        assert_eq!(
            user.managed_unit().unwrap().start_command(),
            "systemctl --user start ollama"
        );
    }

    #[test]
    fn a_masked_unit_is_not_a_managed_unit() {
        let dir = Scratch::new("masked");
        std::fs::create_dir_all(dir.path("sys")).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.path("sys/ollama.service")).unwrap();
        let host = OllamaHost::default().with_unit_dir(dir.path("sys"), UnitScope::System);
        assert!(host.managed_unit().is_none());
    }

    #[test]
    fn a_model_is_found_in_the_store_the_drop_in_names() {
        let dir = Scratch::new("store");
        let sys_store = dir.path("var/models");
        dir.write(
            "sys/ollama.service.d/10-models.conf",
            &format!(
                "[Service]\nEnvironment=\"OLLAMA_MODELS={}\"\n",
                sys_store.display()
            ),
        );
        dir.manifest("var/models", "registry.ollama.ai/library/gemma4/e4b");
        dir.manifest(
            "home/models",
            "registry.ollama.ai/library/nomic-embed-text/latest",
        );
        let host = OllamaHost::default()
            .with_unit_dir(dir.path("sys"), UnitScope::System)
            .with_store_root(dir.path("home/models"));

        assert_eq!(host.find_model_store("gemma4:e4b"), Some(sys_store));
        assert_eq!(
            host.find_model_store("nomic-embed-text:latest"),
            Some(dir.path("home/models"))
        );
        assert_eq!(host.find_model_store("gemma4:12b"), None);
    }

    #[test]
    fn the_wrong_store_sentence_names_the_unit_and_never_says_pull() {
        let dir = Scratch::new("explain");
        dir.manifest("store", "registry.ollama.ai/library/gemma4/e4b");
        let no_unit = OllamaHost::default().with_store_root(dir.path("store"));
        let s = no_unit
            .wrong_store_explanation("gemma4:e4b", "http://127.0.0.1:1")
            .unwrap();
        assert!(s.contains("OLLAMA_MODELS="), "{s}");
        assert!(!s.contains("ollama pull"), "{s}");

        dir.write("sys/ollama.service", "[Service]\n");
        let with_unit = no_unit.with_unit_dir(dir.path("sys"), UnitScope::System);
        let s = with_unit
            .wrong_store_explanation("gemma4:e4b", "http://127.0.0.1:1")
            .unwrap();
        assert!(s.contains("sudo systemctl restart ollama"), "{s}");
        assert!(!s.contains("ollama pull"), "{s}");
        assert!(with_unit
            .wrong_store_explanation("absent:1b", "http://127.0.0.1:1")
            .is_none());
    }

    #[test]
    fn system_host_searches_the_documented_places() {
        let host = OllamaHost::system_under(Some(Path::new("/home/someone")));
        let stores = host.model_stores();
        for want in [
            "/var/lib/ollama/.ollama/models",
            "/usr/share/ollama/.ollama/models",
            "/home/someone/.ollama/models",
        ] {
            assert!(stores.contains(&PathBuf::from(want)), "{want}: {stores:?}");
        }
    }
}
