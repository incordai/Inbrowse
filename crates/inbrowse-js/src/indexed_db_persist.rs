//! Disk form of `IndexedDbStorage` for `--storage-dir`.
//!
//! Called from inbrowse/src/context.rs: `load_indexed_db` restores each
//! origin when a context is created, and `save_indexed_db` flushes changed
//! origins from `save_storage` (shutdown) and after each navigation (page.rs).
//! Origins are marked changed by `Inner::command` on `commit` and `delete`.
//! Sibling: `OriginStorage::{load, flush_changed}` in ops.rs does the same for
//! `localStorage`.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

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

    /// Independent store holding a copy of every committed database. Open
    /// handles, transactions and unflushed changes are not carried over, so
    /// nothing done through the copy is visible here, and the other way round.
    pub fn isolated_snapshot(&self) -> Self {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let databases = inner
            .databases
            .iter()
            .filter(|(_, db)| db.version > 0)
            .map(|(key, db)| {
                let copy = Database {
                    version: db.version,
                    schema: db.schema.clone(),
                    records: db.records.clone(),
                    schema_bytes: db.schema_bytes,
                    ..Default::default()
                };
                (key.clone(), copy)
            })
            .collect();
        Self {
            inner: Mutex::new(Inner { bytes: inner.bytes, databases, ..Default::default() }),
            ..Default::default()
        }
    }

    /// Apply what `current` changed since `initial` (a snapshot taken when it
    /// was created) to this store, leaving everything else here as it is, so
    /// two copies of one profile merge without reverting each other. Per
    /// database and record, the later merge wins; a database whose version or
    /// schema `current` changed takes `current`'s schema and drops stores it no
    /// longer has. A false result means some change exceeded this store's
    /// budget and was skipped.
    pub fn merge_changes(&self, initial: &Self, current: &Self) -> bool {
        let changed: Vec<(String, Vec<StoredDatabase>)> = {
            let current = current.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut origins: Vec<String> = current.changed.iter().cloned().collect();
            origins.sort();
            origins.into_iter().map(|origin| {
                let databases = origin_databases(&current, &origin);
                (origin, databases)
            }).collect()
        };
        let initial: HashMap<String, Vec<StoredDatabase>> = {
            let initial = initial.inner.lock().unwrap_or_else(|e| e.into_inner());
            changed.iter().map(|(origin, _)| (origin.clone(), origin_databases(&initial, origin))).collect()
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut accepted = true;
        for (origin, current) in &changed {
            let initial = &initial[origin];
            let mut names: Vec<&str> = current.iter().chain(initial).map(|db| db.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            for name in names {
                let before = initial.iter().find(|db| db.name == name);
                let after = current.iter().find(|db| db.name == name);
                accepted &= merge_database(&mut inner, origin, name, before, after);
            }
        }
        accepted
    }
}

/// Merge one database's change (`before` to `after`) into `inner`.
fn merge_database(
    inner: &mut Inner,
    origin: &str,
    name: &str,
    before: Option<&StoredDatabase>,
    after: Option<&StoredDatabase>,
) -> bool {
    let key = (origin.to_string(), name.to_string());
    let old_bytes = inner.databases.get(&key).map_or(0, |db| {
        db.schema_bytes
            + db.records.values().flatten().map(|(k, v)| k.len() + v.len() + RECORD_OVERHEAD).sum::<usize>()
    });
    let existing = inner.databases.get(&key).filter(|db| db.version > 0);
    let Some(after) = after else {
        if before.is_none() || existing.is_none() {
            return true;
        }
        inner.bytes = inner.bytes.saturating_sub(old_bytes);
        if let Some(db) = inner.databases.get_mut(&key) {
            db.records.clear();
            db.schema.clear();
            db.schema_bytes = 0;
            db.version = 0;
        }
        inner.changed.insert(origin.to_string());
        return true;
    };

    let upgraded = before.is_none_or(|before| before.version != after.version || before.schema != after.schema);
    let (version, schema, mut records) = match existing {
        Some(db) if !upgraded => (db.version, db.schema.clone(), db.records.clone()),
        Some(db) => (after.version, after.schema.clone(), db.records.clone()),
        None => (after.version, after.schema.clone(), HashMap::new()),
    };
    let stores: HashSet<&str> = schema.iter().filter_map(|store| store.get("name")?.as_str()).collect();
    records.retain(|store, _| stores.contains(store.as_str()));
    let empty = HashMap::new();
    for store in after.records.keys().chain(before.iter().flat_map(|db| db.records.keys())) {
        if !stores.contains(store.as_str()) {
            continue;
        }
        let was = before.and_then(|db| db.records.get(store)).unwrap_or(&empty);
        let now = after.records.get(store).unwrap_or(&empty);
        let target = records.entry(store.clone()).or_default();
        for (record, value) in now {
            if was.get(record) != Some(value) {
                target.insert(record.clone(), value.clone());
            }
        }
        for record in was.keys().filter(|record| !now.contains_key(*record)) {
            target.remove(record);
        }
    }

    let merged = StoredDatabase { name: name.to_string(), version, schema, records };
    let Some(new_bytes) = merged.charged_bytes() else { return false };
    let is_new = !inner.databases.contains_key(&key);
    if inner.bytes.saturating_sub(old_bytes).saturating_add(new_bytes) > MAX_BYTES
        || is_new && inner.databases.len() >= MAX_HANDLES
    {
        return false;
    }
    let unchanged = existing.is_some_and(|db| {
        db.version == merged.version && db.schema == merged.schema && db.records == merged.records
    });
    if unchanged {
        return true;
    }
    let schema_bytes = serde_json::to_vec(&merged.schema).map_or(0, |encoded| encoded.len());
    let db = inner.databases.entry(key).or_default();
    db.version = merged.version;
    db.schema = merged.schema;
    db.records = merged.records;
    db.schema_bytes = schema_bytes;
    inner.bytes = inner.bytes.saturating_sub(old_bytes).saturating_add(new_bytes);
    inner.changed.insert(origin.to_string());
    true
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

    fn put(owner: &IndexedDbOwner, name: &str, records: &[(&str, Option<&str>)]) {
        let connection = owner.command(ORIGIN, "connect", json!({"name":name})).as_u64().unwrap();
        let id = owner
            .command(ORIGIN, "begin", json!({"name":name,"mode":"readwrite","stores":["items"],"connection":connection}))
            .as_u64()
            .unwrap();
        assert!(!owner.command(ORIGIN, "ready", json!({"id":id})).is_null());
        for (key, value) in records {
            match value {
                Some(value) => owner.command(ORIGIN, "put", json!({"id":id,"store":"items","key":key,"value":value})),
                None => owner.command(ORIGIN, "remove", json!({"id":id,"store":"items","key":key})),
            };
        }
        assert!(owner.command(ORIGIN, "commit", json!({"id":id,"schema":[{"name":"items"}]})).is_null());
        owner.command(ORIGIN, "close", json!({"name":name,"connection":connection}));
    }

    fn records(storage: &IndexedDbStorage, name: &str) -> Option<Vec<(String, String)>> {
        let inner = storage.inner.lock().unwrap();
        let db = inner.databases.get(&(ORIGIN.to_string(), name.to_string())).filter(|db| db.version > 0)?;
        let mut all: Vec<_> = db.records.values().flatten().map(|(k, v)| (k.clone(), v.clone())).collect();
        all.sort();
        Some(all)
    }

    fn pairs(entries: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        Some(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn a_snapshot_starts_from_the_profile_and_stays_apart() {
        let profile = Arc::new(IndexedDbStorage::default());
        commit(&IndexedDbOwner::new(profile.clone()), "app", &[("k", "v")]);
        let copy = Arc::new(profile.isolated_snapshot());
        assert_eq!(records(&copy, "app"), pairs(&[("k", "v")]));
        assert!(!copy.is_dirty(), "a snapshot has nothing to flush");
        put(&IndexedDbOwner::new(copy.clone()), "app", &[("copy", Some("1"))]);
        put(&IndexedDbOwner::new(profile.clone()), "app", &[("profile", Some("2"))]);
        assert_eq!(records(&copy, "app"), pairs(&[("copy", "1"), ("k", "v")]));
        assert_eq!(records(&profile, "app"), pairs(&[("k", "v"), ("profile", "2")]));
    }

    #[test]
    fn merging_applies_only_what_a_copy_changed() {
        let profile = Arc::new(IndexedDbStorage::default());
        let seed = IndexedDbOwner::new(profile.clone());
        commit(&seed, "app", &[("keep", "1"), ("edit", "old"), ("drop", "x")]);
        commit(&seed, "doomed", &[("k", "v")]);
        let initial = profile.isolated_snapshot();
        let a = Arc::new(profile.isolated_snapshot());
        let b = Arc::new(profile.isolated_snapshot());
        put(&IndexedDbOwner::new(a.clone()), "app", &[("edit", Some("a")), ("drop", None), ("a", Some("A"))]);
        let owner_a = IndexedDbOwner::new(a.clone());
        assert_eq!(owner_a.command(ORIGIN, "delete", json!({"name":"doomed"})), json!(true));
        commit(&owner_a, "fresh", &[("n", "new")]);
        put(&IndexedDbOwner::new(b.clone()), "app", &[("b", Some("B"))]);

        let destination = profile.isolated_snapshot();
        assert!(destination.merge_changes(&initial, &b));
        assert!(destination.merge_changes(&initial, &a));
        assert_eq!(
            records(&destination, "app"),
            pairs(&[("a", "A"), ("b", "B"), ("edit", "a"), ("keep", "1")]),
            "each copy's changes land, and the other copy's are not reverted",
        );
        assert_eq!(records(&destination, "doomed"), None);
        assert_eq!(records(&destination, "fresh"), pairs(&[("n", "new")]));
        let saved = flushed(&destination);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].1.iter().map(|db| db.name.as_str()).collect::<Vec<_>>(), ["app", "fresh"]);
    }

    #[test]
    fn merging_an_unchanged_copy_leaves_the_profile_clean() {
        let profile = Arc::new(IndexedDbStorage::default());
        commit(&IndexedDbOwner::new(profile.clone()), "app", &[("k", "v")]);
        let initial = profile.isolated_snapshot();
        let copy = profile.isolated_snapshot();
        let destination = profile.isolated_snapshot();
        assert!(destination.merge_changes(&initial, &copy));
        assert!(!destination.is_dirty());
    }
}
