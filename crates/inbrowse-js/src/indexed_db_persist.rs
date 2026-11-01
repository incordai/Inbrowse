//! Disk form of `IndexedDbStorage` for `--storage-dir`.
//!
//! Called from inbrowse/src/context.rs: `load_indexed_db` restores each
//! origin when a context is created, and `save_indexed_db` flushes changed
//! origins from `save_storage` (shutdown) and after each navigation (page.rs).
//! Origins are marked changed by `Inner::command` on `commit` and `delete`.
//! Sibling: `OriginStorage::{load, flush_changed}` in ops.rs does the same for
//! `localStorage`.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{text, Database, IndexedDbStorage, Inner, MAX_BYTES, MAX_HANDLES, RECORD_OVERHEAD};

const MAX_NAME_BYTES: usize = 16 * 1024;
const MAX_KEY_BYTES: usize = 64 * 1024;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;

/// One committed database: its version, object store schema and records
/// (`store -> key -> serialized value`).
#[derive(Serialize, Deserialize)]
pub struct StoredDatabase {
    pub name: String,
    pub version: u64,
    pub schema: Vec<Value>,
    pub records: HashMap<String, HashMap<String, String>>,
}

impl StoredDatabase {
    /// Bytes this database charges against `MAX_BYTES`, or `None` when it
    /// breaks a limit `Inner::command` enforces on live writes.
    fn charged_bytes(&self) -> Option<usize> {
        if self.version == 0 || self.name.len() > MAX_NAME_BYTES || self.schema.len() > MAX_HANDLES {
            return None;
        }
        let stores: HashSet<&str> = self.schema.iter().map(|s| text(s, "name")).collect::<Result<_, _>>().ok()?;
        let mut bytes = serde_json::to_vec(&self.schema).ok()?.len();
        for (store, records) in &self.records {
            if !stores.contains(store.as_str()) {
                return None;
            }
            for (key, value) in records {
                if key.len() > MAX_KEY_BYTES || value.len() > MAX_VALUE_BYTES {
                    return None;
                }
                bytes = bytes.saturating_add(key.len() + value.len() + RECORD_OVERHEAD);
            }
        }
        Some(bytes)
    }
}

fn origin_databases(inner: &Inner, origin: &str) -> Vec<StoredDatabase> {
    let mut databases: Vec<StoredDatabase> = inner
        .databases
        .iter()
        .filter(|((o, _), db)| o == origin && db.version > 0)
        .map(|((_, name), db)| StoredDatabase {
            name: name.clone(),
            version: db.version,
            schema: db.schema.clone(),
            records: db.records.clone(),
        })
        .collect();
    databases.sort_by(|a, b| a.name.cmp(&b.name));
    databases
}

impl IndexedDbStorage {
    pub fn is_dirty(&self) -> bool {
        !self.inner.lock().unwrap_or_else(|e| e.into_inner()).changed.is_empty()
    }

    /// Hand each changed origin's committed databases to `write`; an empty
    /// slice means the origin has none left. An origin stays changed until
    /// `write` returns true, so a failed write is retried on the next flush.
    pub fn flush_changed(&self, mut write: impl FnMut(&str, &[StoredDatabase]) -> bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut origins: Vec<String> = inner.changed.iter().cloned().collect();
        origins.sort();
        for origin in origins {
            if write(&origin, &origin_databases(&inner, &origin)) {
                inner.changed.remove(&origin);
            }
        }
    }

    /// Restore one origin's databases without scheduling a rewrite. The whole
    /// origin is refused when any database is malformed, a name repeats or is
    /// already open in this store, or the result would exceed the store's
    /// handle or byte budget.
    pub fn load(&self, origin: &str, databases: Vec<StoredDatabase>) -> bool {
        if origin == "null" {
            return false;
        }
        let mut names = HashSet::new();
        let mut bytes = 0usize;
        for db in &databases {
            let Some(charged) = db.charged_bytes() else { return false };
            if !names.insert(db.name.as_str()) {
                return false;
            }
            bytes = bytes.saturating_add(charged);
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if databases.iter().any(|db| inner.databases.contains_key(&(origin.to_string(), db.name.clone())))
            || inner.databases.len() + databases.len() > MAX_HANDLES
            || inner.bytes.saturating_add(bytes) > MAX_BYTES
        {
            return false;
        }
        for db in databases {
            let schema_bytes = serde_json::to_vec(&db.schema).map_or(0, |encoded| encoded.len());
            inner.databases.insert(
                (origin.to_string(), db.name),
                Database { version: db.version, schema: db.schema, records: db.records, schema_bytes, ..Default::default() },
            );
        }
        inner.bytes += bytes;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::super::IndexedDbOwner;
    use super::*;

    const ORIGIN: &str = "https://idb.test";

    /// Create database `name` at version 1 with one `items` store holding `records`.
    fn commit(owner: &IndexedDbOwner, name: &str, records: &[(&str, &str)]) {
        let id = owner.command(ORIGIN, "begin", json!({"name":name,"mode":"versionchange","stores":["items"]}))
            .as_u64().unwrap();
        assert!(!owner.command(ORIGIN, "ready", json!({"id":id})).is_null());
        for (key, value) in records {
            owner.command(ORIGIN, "put", json!({"id":id,"store":"items","key":key,"value":value}));
        }
        assert!(owner.command(ORIGIN, "commit", json!({"id":id,"schema":[{"name":"items"}],"version":1})).is_null());
    }

    fn flushed(storage: &IndexedDbStorage) -> Vec<(String, Vec<StoredDatabase>)> {
        let mut out = Vec::new();
        storage.flush_changed(|origin, databases| {
            let copy = serde_json::from_value(serde_json::to_value(databases).unwrap()).unwrap();
            out.push((origin.to_string(), copy));
            true
        });
        out
    }

    #[test]
    fn committed_databases_round_trip_through_flush_and_load() {
        let storage = Arc::new(IndexedDbStorage::default());
        let owner = IndexedDbOwner::new(storage.clone());
        assert!(!storage.is_dirty());
        commit(&owner, "app", &[("k", "v")]);
        assert!(storage.is_dirty());
        let mut saved = flushed(&storage);
        assert!(!storage.is_dirty(), "a successful write marks the origin clean");
        assert_eq!(saved.len(), 1);
        let (origin, databases) = saved.remove(0);
        assert_eq!(origin, ORIGIN);

        let restored = Arc::new(IndexedDbStorage::default());
        assert!(restored.load(&origin, databases));
        assert!(!restored.is_dirty(), "restoring a profile must not schedule a rewrite");
        let reader = IndexedDbOwner::new(restored);
        assert_eq!(reader.command(ORIGIN, "info", json!({"name":"app"}))["version"], json!(1));
        let connection = reader.command(ORIGIN, "connect", json!({"name":"app"}));
        let id = reader.command(ORIGIN, "begin",
            json!({"name":"app","mode":"readonly","stores":["items"],"connection":connection})).as_u64().unwrap();
        assert_eq!(reader.command(ORIGIN, "get", json!({"id":id,"store":"items","key":"k"})), json!("v"));
    }

    #[test]
    fn a_failed_write_stays_pending_and_a_deleted_database_flushes_empty() {
        let storage = Arc::new(IndexedDbStorage::default());
        let owner = IndexedDbOwner::new(storage.clone());
        commit(&owner, "app", &[("k", "v")]);
        storage.flush_changed(|_, _| false);
        assert!(storage.is_dirty(), "a failed write must be retried on the next flush");
        flushed(&storage);
        assert_eq!(owner.command(ORIGIN, "delete", json!({"name":"app"})), json!(true));
        let saved = flushed(&storage);
        assert_eq!(saved.len(), 1);
        assert!(saved[0].1.is_empty(), "the origin's file must be removed, not kept stale");
    }

    #[test]
    fn invalid_profiles_are_refused_whole() {
        let db = |name: &str, store: &str| StoredDatabase {
            name: name.into(),
            version: 1,
            schema: vec![json!({"name":"items"})],
            records: HashMap::from([(store.to_string(), HashMap::from([("k".to_string(), "v".to_string())]))]),
        };
        let storage = IndexedDbStorage::default();
        assert!(!storage.load("null", vec![db("a", "items")]), "opaque origins have no storage");
        assert!(!storage.load(ORIGIN, vec![db("a", "missing")]), "records outside the schema");
        assert!(!storage.load(ORIGIN, vec![db("a", "items"), db("a", "items")]), "repeated names");
        let mut unversioned = db("a", "items");
        unversioned.version = 0;
        assert!(!storage.load(ORIGIN, vec![unversioned]), "version 0 is never committed");
        assert!(storage.load(ORIGIN, vec![db("a", "items")]));
        assert!(!storage.load(ORIGIN, vec![db("a", "items")]), "a loaded database is not overwritten");
    }
}
