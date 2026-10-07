//! `get_config` / `set_config`: configuration over the protocol (S32).
//!
//! # What this promises
//!
//! - **One validator.** Every write is checked by
//!   [`dictate_core::config::parse_config`], the loader `dictated
//!   --check-config` and daemon startup use. A write the daemon would refuse
//!   to start with is refused here as `config_invalid`, naming the offending
//!   key, and nothing is written.
//! - **The user's file stays the user's file.** Entry writes are applied with
//!   `toml_edit`, so comments, ordering, blank lines, legacy sections and keys
//!   this build does not know survive untouched. Only the addressed values
//!   change, and an inline comment on a replaced value is kept.
//! - **Atomic, with one backup.** The new file is written beside the old one,
//!   synced, and renamed over it; the previous contents are kept as
//!   `<file>.bak`. A symlinked config (dotfile managers) is written through to
//!   its target rather than replaced by a regular file.
//! - **No lost updates.** Every snapshot carries the file's `revision`. A
//!   write that names the revision it was based on (`base_revision`) is
//!   refused as `conflict` if the file has changed since — whether another
//!   client wrote it or someone edited it by hand — and the file is re-read
//!   immediately before the rename, so an edit that lands while a write is
//!   being validated is not overwritten either.
//! - **Honest about restarts.** Most of the daemon is built from the
//!   configuration once, at startup. A write reports every key whose value on
//!   disk now differs from what the running daemon uses in
//!   `restart_required`. Only [`LIVE_KEYS`] are applied without a restart, and
//!   only because this module applies them.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use dictate_core::config::{parse_config, Config};
use dictate_history::HistoryStore;
use dictate_proto::{ConfigEntry, ConfigFile, ConfigSnapshot, ErrorCode, ProtoError};
use serde_json::{json, Value};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike};
use tracing::info;

/// Keys a running daemon applies the moment they are written.
///
/// Everything else takes effect at the next start. Kept deliberately short:
/// a key belongs here only when the code that applies it exists.
pub const LIVE_KEYS: &[&str] = &["history.privacy_mode"];

/// Reads and writes the daemon's configuration file on behalf of clients.
pub struct ConfigService {
    path: PathBuf,
    /// The configuration the daemon is running, as JSON. Updated when a live
    /// key is applied.
    running: Mutex<Value>,
    /// Every known key with its built-in default: the schema a write is
    /// checked against.
    defaults: Value,
    history: Option<Arc<Mutex<HistoryStore>>>,
    /// Serializes writers, so two clients cannot interleave read-modify-write.
    writes: Mutex<()>,
}

impl ConfigService {
    /// A service for the file at `path`, whose contents the daemon is
    /// currently running as `running`.
    #[must_use]
    pub fn new(path: PathBuf, running: &Config) -> Self {
        Self {
            path,
            running: Mutex::new(to_json(running)),
            defaults: to_json(&Config::default()),
            history: None,
            writes: Mutex::new(()),
        }
    }

    /// Let `history.privacy_mode` apply live to this store.
    #[must_use]
    pub fn with_history(mut self, history: Arc<Mutex<HistoryStore>>) -> Self {
        self.history = Some(history);
        self
    }

    /// The configuration file this service reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the running daemon has privacy mode on, as last applied.
    #[must_use]
    pub fn running_privacy_mode(&self) -> Option<bool> {
        self.running
            .lock()
            .ok()?
            .pointer("/history/privacy_mode")
            .and_then(Value::as_bool)
    }

    /// Answer `get_config`.
    ///
    /// # Errors
    ///
    /// `not_found` for a path that names no key; `internal` when the file
    /// exists but cannot be read.
    pub fn read(&self, path: Option<&str>) -> Result<ConfigSnapshot, ProtoError> {
        let (text, exists) = self.load_text()?;
        let running = self.running_snapshot();
        let (values, warnings, errors) = match resolve(&text) {
            Ok((values, warnings)) => (values, warnings, Vec::new()),
            Err((errors, warnings)) => (running.clone(), warnings, errors),
        };
        let mut restart_required = Vec::new();
        if errors.is_empty() {
            diff_paths(&running, &values, "", &mut restart_required);
        }
        let file = ConfigFile {
            path: self.path.to_string_lossy().into_owned(),
            exists,
            revision: revision(&text),
            document: text,
        };
        let Some(path) = path.filter(|p| !p.is_empty()) else {
            return Ok(ConfigSnapshot {
                values,
                restart_required,
                file: Some(file),
                warnings,
                errors,
                ..Default::default()
            });
        };
        let subtree = lookup(&values, path).cloned().ok_or_else(|| {
            ProtoError::new(
                ErrorCode::NotFound,
                format!("no configuration key '{path}'"),
            )
        })?;
        restart_required.retain(|p| p == path || p.starts_with(&format!("{path}.")));
        Ok(ConfigSnapshot {
            values: subtree,
            path: Some(path.to_string()),
            restart_required,
            warnings,
            errors,
            ..Default::default()
        })
    }

    /// Answer `set_config`.
    ///
    /// # Errors
    ///
    /// `invalid_params` for a malformed request, `config_invalid` (with the
    /// offending `path` in `detail`) for a configuration the daemon would
    /// refuse, `conflict` when the file is no longer at `base_revision` (or
    /// changed while this write was being validated), `internal` when the
    /// file cannot be written.
    pub fn write(
        &self,
        entries: Vec<ConfigEntry>,
        document: Option<String>,
        dry_run: bool,
        base_revision: Option<&str>,
    ) -> Result<ConfigSnapshot, ProtoError> {
        let _writer = self.writes.lock().map_err(|_| {
            ProtoError::new(ErrorCode::Internal, "configuration writer is poisoned")
        })?;
        let (old_text, exists) = self.load_text()?;
        if let Some(base) = base_revision {
            let current = revision(&old_text);
            if base != current {
                return Err(conflict(&current));
            }
        }
        let running = self.running_snapshot();
        let old_values = resolve(&old_text)
            .map(|(v, _)| v)
            .unwrap_or_else(|_| running.clone());

        let new_text = match (document, entries.is_empty()) {
            (Some(_), false) => {
                return Err(ProtoError::new(
                    ErrorCode::InvalidParams,
                    "send either entries or a document, not both",
                ))
            }
            (None, true) => {
                return Err(ProtoError::new(
                    ErrorCode::InvalidParams,
                    "nothing to write: entries is empty and no document was given",
                ))
            }
            (Some(document), true) => {
                if let Err(e) = DocumentMut::from_str(&document) {
                    return Err(invalid(None, vec![format!("not valid TOML: {e}")]));
                }
                document
            }
            (None, false) => self.apply_entries(&old_text, &entries)?,
        };

        let (new_values, warnings) = match resolve(&new_text) {
            Ok(resolved) => resolved,
            Err((errors, _)) => {
                let path = if entries.is_empty() {
                    errors
                        .iter()
                        .find_map(|e| path_in_message(e, &self.defaults))
                } else {
                    self.culprit(&old_text, &entries)
                };
                return Err(invalid(path, errors));
            }
        };
        // An entry write must not set anything the daemon would ignore. The
        // schema check in `apply_entries` makes this unreachable today; it
        // stays as the backstop for a key the loader stops reading.
        if let Some((entry, warning)) = entries.iter().find_map(|e| {
            warnings
                .iter()
                .find(|w| w.contains(&format!("'{}'", e.path)))
                .map(|w| (e, w))
        }) {
            return Err(invalid(Some(entry.path.clone()), vec![warning.clone()]));
        }

        let mut applied = Vec::new();
        diff_paths(&old_values, &new_values, "", &mut applied);

        if !dry_run && new_text != old_text {
            // The file may have been edited by hand while this write was being
            // validated; never overwrite what we did not read.
            let (now_text, _) = self.load_text()?;
            if now_text != old_text {
                return Err(conflict(&revision(&now_text)));
            }
            write_atomically(&self.path, &new_text).map_err(|e| {
                ProtoError::new(
                    ErrorCode::Internal,
                    format!("writing {}: {e}", self.path.display()),
                )
            })?;
            info!(file = %self.path.display(), changed = ?applied, "configuration written");
        }

        let running = if dry_run {
            let mut projected = running;
            for key in LIVE_KEYS {
                if let (Some(slot), Some(v)) =
                    (pointer_mut(&mut projected, key), lookup(&new_values, key))
                {
                    *slot = v.clone();
                }
            }
            projected
        } else {
            self.apply_live(&new_values)
        };
        let mut restart_required = Vec::new();
        diff_paths(&running, &new_values, "", &mut restart_required);

        Ok(ConfigSnapshot {
            values: new_values,
            path: None,
            applied,
            restart_required,
            file: Some(ConfigFile {
                path: self.path.to_string_lossy().into_owned(),
                exists: exists || !dry_run,
                revision: revision(if dry_run { &old_text } else { &new_text }),
                document: new_text,
            }),
            warnings,
            errors: Vec::new(),
            dry_run,
        })
    }

    fn running_snapshot(&self) -> Value {
        self.running
            .lock()
            .map(|v| v.clone())
            .unwrap_or(Value::Null)
    }

    fn load_text(&self) -> Result<(String, bool), ProtoError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok((text, true)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), false)),
            Err(e) => Err(ProtoError::new(
                ErrorCode::Internal,
                format!("reading {}: {e}", self.path.display()),
            )),
        }
    }

    /// Apply entries to the file's text, preserving everything else in it.
    fn apply_entries(&self, text: &str, entries: &[ConfigEntry]) -> Result<String, ProtoError> {
        let mut doc = DocumentMut::from_str(text).map_err(|e| {
            invalid(
                None,
                vec![format!(
                    "the configuration file is not valid TOML, so keys cannot be edited in \
                     place; fix it with a whole-document write first: {e}"
                )],
            )
        })?;
        for (path, value) in self.flatten(entries)? {
            set_path(&mut doc, &path, &value, lookup(&self.defaults, &path))
                .map_err(|message| invalid(Some(path.clone()), vec![message]))?;
        }
        Ok(doc.to_string())
    }

    /// Check every entry against the schema and expand section-valued entries
    /// into their keys, so writing `{"vad": {"threshold": 0.4}}` leaves the
    /// rest of `[vad]` alone.
    fn flatten(&self, entries: &[ConfigEntry]) -> Result<Vec<(String, Value)>, ProtoError> {
        fn expand(
            path: String,
            value: &Value,
            schema: &Value,
            out: &mut Vec<(String, Value)>,
        ) -> Result<(), ProtoError> {
            match (value, lookup(schema, &path)) {
                (_, None) => Err(invalid(
                    Some(path.clone()),
                    vec![format!("unknown configuration key '{path}'")],
                )),
                (Value::Object(map), Some(Value::Object(_))) => {
                    for (key, v) in map {
                        expand(format!("{path}.{key}"), v, schema, out)?;
                    }
                    Ok(())
                }
                (_, Some(Value::Object(_))) if !value.is_null() => Err(invalid(
                    Some(path.clone()),
                    vec![format!("'{path}' is a section; write a table or its keys")],
                )),
                _ => {
                    out.push((path, value.clone()));
                    Ok(())
                }
            }
        }
        let mut out = Vec::new();
        for entry in entries {
            let valid = !entry.path.is_empty()
                && entry.path.split('.').all(|seg| {
                    !seg.is_empty()
                        && seg
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                });
            if !valid {
                return Err(invalid(
                    Some(entry.path.clone()),
                    vec![format!(
                        "'{}' is not a dotted configuration path",
                        entry.path
                    )],
                ));
            }
            expand(entry.path.clone(), &entry.value, &self.defaults, &mut out)?;
        }
        Ok(out)
    }

    /// The first entry that turns a valid file invalid: apply them one at a
    /// time and resolve after each.
    fn culprit(&self, base: &str, entries: &[ConfigEntry]) -> Option<String> {
        let baseline = resolve(base).err().map(|(e, _)| e).unwrap_or_default();
        for n in 1..=entries.len() {
            let Ok(text) = self.apply_entries(base, &entries[..n]) else {
                return Some(entries[n - 1].path.clone());
            };
            if let Err((errors, _)) = resolve(&text) {
                if errors.iter().any(|e| !baseline.contains(e)) {
                    return Some(entries[n - 1].path.clone());
                }
            }
        }
        entries.first().map(|e| e.path.clone())
    }

    /// Apply [`LIVE_KEYS`] to the running daemon; returns the running config.
    fn apply_live(&self, new_values: &Value) -> Value {
        let Ok(mut running) = self.running.lock() else {
            return Value::Null;
        };
        if let (Some(history), Some(want)) = (
            &self.history,
            new_values
                .pointer("/history/privacy_mode")
                .and_then(Value::as_bool),
        ) {
            if running.pointer("/history/privacy_mode") != Some(&Value::Bool(want)) {
                if let Ok(mut store) = history.lock() {
                    let now = store.set_privacy_mode(want);
                    info!(privacy_mode = now, "history privacy mode applied live");
                    if let Some(slot) = running.pointer_mut("/history/privacy_mode") {
                        *slot = Value::Bool(now);
                    }
                }
            }
        }
        running.clone()
    }
}

/// `(values, warnings)` for a usable configuration.
type Resolved = (Value, Vec<String>);
/// `(errors, warnings)` for one the daemon would refuse.
type Refused = (Vec<String>, Vec<String>);

/// Resolve configuration text the way the daemon would at startup.
fn resolve(text: &str) -> Result<Resolved, Refused> {
    match parse_config(text) {
        Err(e) => Err((vec![format!("{e:#}")], Vec::new())),
        Ok((config, report)) if report.errors.is_empty() => Ok((to_json(&config), report.warnings)),
        Ok((_, report)) => Err((report.errors, report.warnings)),
    }
}

fn invalid(path: Option<String>, errors: Vec<String>) -> ProtoError {
    let message = match (&path, errors.first()) {
        (Some(p), Some(first)) => format!("invalid configuration at '{p}': {first}"),
        (None, Some(first)) => format!("invalid configuration: {first}"),
        (Some(p), None) => format!("invalid configuration at '{p}'"),
        (None, None) => "invalid configuration".to_string(),
    };
    let mut error = ProtoError::new(ErrorCode::ConfigInvalid, message);
    error.detail = Some(Box::new(json!({ "path": path, "errors": errors })));
    error
}

/// The opaque revision token for a file's contents: FNV-1a over its bytes.
///
/// Not a security boundary — it only has to change when the text does, so a
/// stale editor is noticed. Stable across builds (unlike `DefaultHasher`).
#[must_use]
pub fn revision(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}

fn conflict(current: &str) -> ProtoError {
    let mut error = ProtoError::new(
        ErrorCode::Conflict,
        "the configuration file changed since it was read; reload it and apply the edit again",
    );
    error.detail = Some(Box::new(json!({ "revision": current })));
    error
}

/// Serialize a config, tidying `f32` values: `0.12f32` widens to
/// `0.11999999731779099`, which is noise in a settings UI and would be written
/// back to the user's file verbatim.
fn to_json(config: &Config) -> Value {
    fn tidy(v: &mut Value) {
        match v {
            Value::Number(n) if n.is_f64() => {
                let x = n.as_f64().unwrap_or_default();
                let narrow = x as f32;
                if f64::from(narrow) == x {
                    if let Some(clean) = narrow
                        .to_string()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                    {
                        *n = clean;
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(tidy),
            Value::Object(map) => map.values_mut().for_each(tidy),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(config).unwrap_or(Value::Null);
    tidy(&mut value);
    value
}

fn lookup<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(root, |node, seg| node.as_object()?.get(seg))
}

fn pointer_mut<'a>(root: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    path.split('.')
        .try_fold(root, |node, seg| node.as_object_mut()?.get_mut(seg))
}

/// Leaf paths at which `a` and `b` differ. Arrays compare whole.
fn diff_paths(a: &Value, b: &Value, prefix: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                diff_paths(
                    x.get(key).unwrap_or(&Value::Null),
                    y.get(key).unwrap_or(&Value::Null),
                    &path,
                    out,
                );
            }
        }
        _ if a != b => out.push(prefix.to_string()),
        _ => {}
    }
}

/// The first dotted key in an error message that names a known setting.
fn path_in_message(message: &str, schema: &Value) -> Option<String> {
    message
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .map(|t| t.trim_matches('.'))
        .filter(|t| t.contains('.'))
        .find(|t| lookup(schema, t).is_some())
        .map(str::to_string)
}

/// Set (or, for `null`, remove) one dotted key, keeping the decor — comments
/// and whitespace — of a value it replaces.
fn set_path(
    doc: &mut DocumentMut,
    path: &str,
    value: &Value,
    template: Option<&Value>,
) -> Result<(), String> {
    let segments: Vec<&str> = path.split('.').collect();
    let (leaf, parents) = segments.split_last().ok_or("empty path")?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for seg in parents {
        let is_table = table.get(seg).is_some_and(|item| item.is_table_like());
        if !is_table {
            if value.is_null() {
                return Ok(()); // Removing a key under a section that is not there.
            }
            let mut section = Table::new();
            section.set_implicit(true);
            table.insert(seg, Item::Table(section));
        }
        table = table
            .get_mut(seg)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("'{seg}' is not a table"))?;
    }
    if value.is_null() {
        table.remove(leaf);
        return Ok(());
    }
    let item = json_to_item(value, template)?;
    match (table.get_mut(leaf), item) {
        (Some(Item::Value(old)), Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        (_, item) => {
            table.insert(leaf, item);
        }
    }
    Ok(())
}

fn json_to_item(value: &Value, template: Option<&Value>) -> Result<Item, String> {
    match value {
        // A list of tables (context profiles) is written as `[[a.b]]` blocks,
        // the way a person would write it.
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            let element = template.and_then(Value::as_array).and_then(|a| a.first());
            let mut tables = ArrayOfTables::new();
            for item in items {
                let mut table = Table::new();
                for (key, v) in item.as_object().into_iter().flatten() {
                    if !v.is_null() {
                        table.insert(key, json_to_item(v, element.and_then(|e| e.get(key)))?);
                    }
                }
                tables.push(table);
            }
            Ok(Item::ArrayOfTables(tables))
        }
        _ => Ok(Item::Value(json_to_value(value, template)?)),
    }
}

fn json_to_value(value: &Value, template: Option<&Value>) -> Result<toml_edit::Value, String> {
    Ok(match value {
        Value::Null => {
            return Err("null is only meaningful as a whole value (reset to default)".into())
        }
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                // Keep a float-typed key a float in the file (`10.0`, not `10`).
                if template.is_some_and(Value::is_f64) {
                    (i as f64).into()
                } else {
                    i.into()
                }
            } else if n.is_u64() {
                return Err(format!("{n} is too large for a TOML integer"));
            } else {
                n.as_f64().ok_or("not a finite number")?.into()
            }
        }
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let element = template.and_then(Value::as_array).and_then(|a| a.first());
            let mut array = Array::new();
            for item in items {
                array.push(json_to_value(item, element)?);
            }
            array.into()
        }
        Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, v) in map {
                if !v.is_null() {
                    table.insert(key, json_to_value(v, template.and_then(|t| t.get(key)))?);
                }
            }
            table.into()
        }
    })
}

/// Replace `path` with `text` atomically, keeping the previous contents as
/// `<name>.bak`.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Write through a symlink to its target: renaming over the link itself
    // would silently detach a dotfile-managed config from its repository.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".into());
    let existing = std::fs::metadata(&target).ok();
    let mode = existing
        .as_ref()
        .map_or(0o644, |m| m.permissions().mode() & 0o7777);

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    let bak_tmp = dir.join(format!(".{name}.bak-tmp-{}-{nonce}", std::process::id()));
    let result = (|| {
        write_new_file(&tmp, text.as_bytes(), mode)?;
        if existing.is_some() {
            // Never `fs::copy` onto `<name>.bak`: that opens the destination
            // and writes through a symlink or hard link someone planted there
            // (`config.toml.bak -> ~/.ssh/authorized_keys`). A fresh exclusive
            // temp file renamed into place replaces the *directory entry*
            // instead, so whatever the old `.bak` pointed at is untouched.
            let previous = std::fs::read(&target)?;
            write_new_file(&bak_tmp, &previous, mode)?;
            std::fs::rename(&bak_tmp, dir.join(format!("{name}.bak")))?;
        }
        std::fs::rename(&tmp, &target)?;
        std::fs::File::open(&dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&bak_tmp);
    }
    result
}

/// Create `path` exclusively (`O_CREAT|O_EXCL`, which refuses an existing
/// entry including a dangling symlink), write `bytes`, and set `mode` exactly.
fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // `mode` above is filtered by the umask; restore the original exactly.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Value {
        to_json(&Config::default())
    }

    #[test]
    fn revision_tracks_content_and_is_stable() {
        assert_eq!(revision(""), "fnv1a64:cbf29ce484222325");
        assert_eq!(revision("a"), "fnv1a64:af63dc4c8601ec8c");
        assert_ne!(revision("[grammar]\n"), revision("[grammar] \n"));
    }

    #[test]
    fn diff_reports_changed_leaves_only() {
        let a = json!({"a": {"x": 1, "y": [1, 2]}, "b": true});
        let b = json!({"a": {"x": 2, "y": [1, 2]}, "b": true, "c": 3});
        let mut out = Vec::new();
        diff_paths(&a, &b, "", &mut out);
        assert_eq!(out, vec!["a.x", "c"]);
    }

    #[test]
    fn f32_settings_read_as_written_and_f64_ones_are_untouched() {
        let v = defaults();
        assert_eq!(lookup(&v, "audio.gain.target_rms"), Some(&json!(0.12)));
        assert_eq!(lookup(&v, "whisper.no_speech_threshold"), Some(&json!(0.6)));
    }

    #[test]
    fn a_path_is_found_in_a_validation_message() {
        let v = defaults();
        assert_eq!(
            path_in_message(
                "grammar.timeout_s must be a positive number of seconds, got 0",
                &v
            )
            .as_deref(),
            Some("grammar.timeout_s")
        );
        assert_eq!(path_in_message("something vague went wrong.", &v), None);
    }

    #[test]
    fn replacing_a_value_keeps_its_comments() {
        let mut doc = DocumentMut::from_str(
            "# top\n[grammar]\n# above\nenabled = false # inline\nmodel = \"m\"\n",
        )
        .unwrap();
        set_path(
            &mut doc,
            "grammar.enabled",
            &json!(true),
            Some(&json!(false)),
        )
        .unwrap();
        assert_eq!(
            doc.to_string(),
            "# top\n[grammar]\n# above\nenabled = true # inline\nmodel = \"m\"\n"
        );
    }

    #[test]
    fn a_float_key_stays_a_float_and_null_removes() {
        let mut doc = DocumentMut::from_str("[grammar]\ntimeout_s = 3.0\nmin_words = 2\n").unwrap();
        set_path(
            &mut doc,
            "grammar.timeout_s",
            &json!(12),
            Some(&json!(10.0)),
        )
        .unwrap();
        set_path(&mut doc, "grammar.min_words", &Value::Null, None).unwrap();
        assert_eq!(doc.to_string(), "[grammar]\ntimeout_s = 12.0\n");
    }

    #[test]
    fn a_new_nested_key_creates_only_the_section_it_needs() {
        let mut doc = DocumentMut::from_str("[grammar]\nenabled = true\n").unwrap();
        set_path(&mut doc, "audio.gain.enabled", &json!(false), None).unwrap();
        let text = doc.to_string();
        assert!(text.contains("[audio.gain]\nenabled = false"), "{text}");
        assert!(
            !text.contains("[audio]\n"),
            "no empty parent header: {text}"
        );
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dictated-cfgw-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_backup_does_not_write_through_a_symlink() {
        let dir = scratch("bak-symlink");
        let cfg = dir.join("config.toml");
        let victim = dir.join("victim");
        std::fs::write(&cfg, "old = 1\n").unwrap();
        std::fs::write(&victim, "precious\n").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("config.toml.bak")).unwrap();

        write_atomically(&cfg, "new = 2\n").unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious\n");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "new = 2\n");
        let bak = dir.join("config.toml.bak");
        assert!(!std::fs::symlink_metadata(&bak).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "old = 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_backup_does_not_write_through_a_hard_link() {
        let dir = scratch("bak-hardlink");
        let cfg = dir.join("config.toml");
        let victim = dir.join("victim");
        std::fs::write(&cfg, "old = 1\n").unwrap();
        std::fs::write(&victim, "precious\n").unwrap();
        std::fs::hard_link(&victim, dir.join("config.toml.bak")).unwrap();

        write_atomically(&cfg, "new = 2\n").unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml.bak")).unwrap(),
            "old = 1\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dangling_backup_symlink_creates_nothing_outside_the_directory() {
        let dir = scratch("bak-dangling");
        let cfg = dir.join("config.toml");
        let outside = dir.join("not-yet");
        std::fs::write(&cfg, "old = 1\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("config.toml.bak")).unwrap();

        write_atomically(&cfg, "new = 2\n").unwrap();

        assert!(!outside.exists(), "the link target must not be created");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_backup_keeps_the_config_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("bak-mode");
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "old = 1\n").unwrap();
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomically(&cfg, "new = 2\n").unwrap();
        let mode = |n: &str| std::fs::metadata(dir.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("config.toml"), 0o600);
        assert_eq!(mode("config.toml.bak"), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
