//! Personal vocabulary: SQLite CRUD, in-memory matching, recognizer bias, and
//! read-only proposals. No dependency on core, hardware, or a language model.
mod config;
mod matcher;
pub mod store;
pub mod suggestions;
pub use config::DictionaryConfig;
pub use matcher::{Applied, Replacement};
pub use store::{DictionaryStore, StoredEntry};

use dictate_proto::{AppContext, DictionaryEntry, ProtoError};
use matcher::Matcher;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};
use store::internal;

pub struct Dictionary {
    pub config: DictionaryConfig,
    store: Mutex<DictionaryStore>,
    snapshot: RwLock<Arc<Matcher>>,
    pending: Mutex<HashMap<i64, u64>>,
    static_prompt: Option<String>,
}
impl Dictionary {
    pub fn open(config: DictionaryConfig, static_prompt: Option<String>) -> anyhow::Result<Self> {
        config.validate()?;
        let store = DictionaryStore::open(config.path())?;
        Self::new(store, config, static_prompt)
    }
    pub fn new(
        store: DictionaryStore,
        config: DictionaryConfig,
        static_prompt: Option<String>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let snapshot = Matcher::new(store.entries()?, &config)?;
        Ok(Self {
            config,
            store: Mutex::new(store),
            snapshot: RwLock::new(Arc::new(snapshot)),
            pending: Mutex::new(HashMap::new()),
            static_prompt,
        })
    }
    pub fn in_memory() -> anyhow::Result<Self> {
        Self::new(
            DictionaryStore::in_memory()?,
            DictionaryConfig::default(),
            None,
        )
    }
    pub fn list(&self, query: Option<&str>, limit: Option<u32>) -> Vec<DictionaryEntry> {
        let snapshot = self.snapshot.read().expect("dictionary snapshot poisoned");
        let q = query.map(str::to_lowercase);
        snapshot
            .entries
            .iter()
            .filter(|e| {
                q.as_ref()
                    .is_none_or(|q| e.entry.phrase.to_lowercase().contains(q))
            })
            .take(limit.map(|n| n.min(10_000) as usize).unwrap_or(10_000))
            .map(|e| e.entry.clone())
            .collect()
    }
    pub fn upsert(&self, entry: DictionaryEntry) -> Result<DictionaryEntry, ProtoError> {
        let mut store = self.store.lock().map_err(internal)?;
        let entry = store.upsert(entry)?;
        self.refresh(&store).map_err(internal)?;
        Ok(entry)
    }
    pub fn delete(&self, id: i64) -> Result<(), ProtoError> {
        let mut store = self.store.lock().map_err(internal)?;
        store.delete(id)?;
        self.refresh(&store).map_err(internal)?;
        Ok(())
    }
    fn refresh(&self, store: &DictionaryStore) -> anyhow::Result<()> {
        let next = Arc::new(Matcher::new(store.entries()?, &self.config)?);
        *self
            .snapshot
            .write()
            .map_err(|_| anyhow::anyhow!("dictionary snapshot poisoned"))? = next;
        Ok(())
    }
    /// Pure matching; call `record_hits` separately only for non-private sessions.
    pub fn apply(&self, text: &str, app: Option<&AppContext>) -> Applied {
        let snapshot = self
            .snapshot
            .read()
            .expect("dictionary snapshot poisoned")
            .clone();
        snapshot.apply(text, app, &self.config)
    }
    pub fn record_hits(&self, replacements: &[Replacement], privacy: bool) {
        if privacy {
            return;
        }
        let mut pending = self.pending.lock().expect("dictionary hit queue poisoned");
        for r in replacements {
            *pending.entry(r.entry_id).or_default() += 1;
        }
    }
    /// SQLite work belongs on a blocking worker. Failed batches remain queued.
    pub fn flush_hits(&self) -> anyhow::Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("dictionary store poisoned"))?;
        let batch = std::mem::take(
            &mut *self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("dictionary hit queue poisoned"))?,
        );
        if batch.is_empty() {
            return Ok(());
        }
        if let Err(e) = store.increment_hits(&batch) {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("dictionary hit queue poisoned"))?;
            for (id, count) in batch {
                *pending.entry(id).or_default() += count;
            }
            return Err(e);
        }
        self.refresh(&store)
    }
    pub fn vocabulary(&self, app: Option<&AppContext>) -> Vec<String> {
        if !self.config.enabled {
            return Vec::new();
        }
        self.snapshot
            .read()
            .expect("dictionary snapshot poisoned")
            .in_scope(app)
            .map(|e| e.entry.phrase.clone())
            .collect()
    }
    /// Static prompt takes priority; whole glossary terms are added in ranking
    /// order. The hard ceiling is 400 UTF-8 bytes AND 400 Unicode characters.
    pub fn initial_prompt(&self, app: Option<&AppContext>, use_dictionary: bool) -> Option<String> {
        let budget = self.config.max_prompt_chars.min(400);
        let mut out = String::new();
        for c in self
            .static_prompt
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(budget)
        {
            if out.len() + c.len_utf8() > 400 {
                break;
            }
            out.push(c);
        }
        if use_dictionary && self.config.enabled && self.config.stt_bias {
            let mut first = true;
            for term in self.vocabulary(app) {
                let added = format!(
                    "{}{}",
                    if first {
                        if out.is_empty() {
                            "Glossary: "
                        } else {
                            " Glossary: "
                        }
                    } else {
                        ", "
                    },
                    term
                );
                if out.chars().count() + added.chars().count() + 1 > budget
                    || out.len() + added.len() + 1 > 400
                {
                    continue;
                }
                out.push_str(&added);
                first = false;
            }
            if !first {
                out.push('.');
            }
        }
        (!out.is_empty()).then_some(out)
    }
}
