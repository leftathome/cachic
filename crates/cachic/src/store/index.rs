//! The object index.
//!
//! Maps an object id to what we know about the object: its length, validators, current
//! generation, and whether the origin honours ranges. It exists so that a request for a known
//! object skips the probe.
//!
//! **The index is never authoritative.** Slices are self-describing (FR-44), so the index is a
//! rebuildable acceleration structure. Any code path that resolves a disagreement between the
//! index and a slice in favour of the index is a bug: it would serve bytes described by stale
//! metadata. When they disagree, the slice wins and the index is repaired.

use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use super::slice::ObjectId;

const OBJECTS: TableDefinition<[u8; 16], Vec<u8>> = TableDefinition::new("objects");

/// How stale a `last_seen` may get before it is worth another write.
///
/// Updating it on every request would turn every cache hit into a disk write, which on a busy
/// LAN party is a great deal of IO to record something only eviction cares about.
const LAST_SEEN_GRANULARITY: Duration = Duration::from_secs(3600);

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("index transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("index table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("index storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("index commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("index entry for {id} is corrupt: {reason}")]
    Corrupt { id: String, reason: String },
}

/// What the index knows about an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    /// The normalised cache key, kept so an operator can be told what an object is and so purge
    /// by prefix has something to match.
    pub key: String,
    pub total_len: u64,
    pub generation: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// The origin ignores `Range` for this object (FR-13).
    pub no_ranges: bool,
    /// Unix seconds.
    pub created: u64,
    /// Unix seconds, updated at most once per [`LAST_SEEN_GRANULARITY`].
    pub last_seen: u64,
    /// The object's validators changed, so everything recorded here except `generation` is
    /// unreliable and the next request must re-probe (FR-14).
    ///
    /// The generation survives because it is what makes the previous version's slices
    /// unreachable. Losing it across a restart would make stale slices addressable again.
    pub stale: bool,
}

impl ObjectMeta {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.key.len());
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.extend_from_slice(&self.generation.to_le_bytes());
        out.push(self.no_ranges as u8);
        out.push(self.stale as u8);
        out.extend_from_slice(&self.created.to_le_bytes());
        out.extend_from_slice(&self.last_seen.to_le_bytes());
        put_str(&mut out, Some(&self.key));
        put_str(&mut out, self.etag.as_deref());
        put_str(&mut out, self.last_modified.as_deref());
        put_str(&mut out, self.content_type.as_deref());
        out
    }

    fn decode(bytes: &[u8], id: &ObjectId) -> Result<Self, IndexError> {
        let mut cursor = Cursor { bytes, at: 0 };
        let corrupt = |reason: &str| IndexError::Corrupt {
            id: hex(id),
            reason: reason.to_owned(),
        };
        let total_len = cursor.u64().ok_or_else(|| corrupt("truncated total_len"))?;
        let generation = cursor
            .u32()
            .ok_or_else(|| corrupt("truncated generation"))?;
        let no_ranges = cursor.u8().ok_or_else(|| corrupt("truncated no_ranges"))? != 0;
        let stale = cursor.u8().ok_or_else(|| corrupt("truncated stale"))? != 0;
        let created = cursor.u64().ok_or_else(|| corrupt("truncated created"))?;
        let last_seen = cursor.u64().ok_or_else(|| corrupt("truncated last_seen"))?;
        let key = cursor
            .string()
            .ok_or_else(|| corrupt("truncated key"))?
            .ok_or_else(|| corrupt("missing key"))?;
        Ok(Self {
            key,
            total_len,
            generation,
            etag: cursor.string().ok_or_else(|| corrupt("truncated etag"))?,
            last_modified: cursor
                .string()
                .ok_or_else(|| corrupt("truncated last_modified"))?,
            content_type: cursor
                .string()
                .ok_or_else(|| corrupt("truncated content_type"))?,
            no_ranges,
            created,
            last_seen,
            stale,
        })
    }
}

fn hex(id: &ObjectId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn put_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
        Some(v) => {
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            out.extend_from_slice(v.as_bytes());
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    /// `Some(None)` is a stored absent value; `None` is a truncated buffer.
    fn string(&mut self) -> Option<Option<String>> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Some(None);
        }
        let bytes = self.take(len as usize)?;
        Some(String::from_utf8(bytes.to_vec()).ok())
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The object index.
pub struct ObjectIndex {
    db: Database,
}

impl ObjectIndex {
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        let db = Database::create(path)?;
        // Create the table so a fresh database can be read from immediately.
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(OBJECTS)?;
        }
        txn.commit()?;
        Ok(Self { db })
    }

    pub fn get(&self, id: &ObjectId) -> Result<Option<ObjectMeta>, IndexError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(OBJECTS)?;
        match table.get(id)? {
            Some(value) => Ok(Some(ObjectMeta::decode(&value.value(), id)?)),
            None => Ok(None),
        }
    }

    pub fn put(&self, id: &ObjectId, meta: &ObjectMeta) -> Result<(), IndexError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(OBJECTS)?;
            table.insert(id, meta.encode())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn remove(&self, id: &ObjectId) -> Result<(), IndexError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(OBJECTS)?;
            table.remove(id)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Record that an object was used, skipping the write if it was seen recently.
    ///
    /// Returns whether a write actually happened, which the metrics care about.
    pub fn touch(&self, id: &ObjectId) -> Result<bool, IndexError> {
        let Some(mut meta) = self.get(id)? else {
            return Ok(false);
        };
        let now = now_secs();
        if now.saturating_sub(meta.last_seen) < LAST_SEEN_GRANULARITY.as_secs() {
            return Ok(false);
        }
        meta.last_seen = now;
        self.put(id, &meta)?;
        Ok(true)
    }

    /// Mark an object stale and bump its generation, so the next request re-probes while the
    /// previous version's slices stay unreachable.
    ///
    /// Returns the new generation. An unknown object starts at generation 1, since generation 0
    /// may already be on disk from a previous run.
    pub fn invalidate(&self, id: &ObjectId) -> Result<u32, IndexError> {
        let mut meta = match self.get(id)? {
            Some(meta) => meta,
            None => return Ok(1),
        };
        meta.generation = meta.generation.wrapping_add(1);
        meta.stale = true;
        let generation = meta.generation;
        self.put(id, &meta)?;
        Ok(generation)
    }

    /// Drop entries not seen within `max_age`. Returns how many were removed.
    pub fn prune(&self, max_age: Duration) -> Result<usize, IndexError> {
        let cutoff = now_secs().saturating_sub(max_age.as_secs());
        let mut stale = Vec::new();
        {
            let txn = self.db.begin_read()?;
            let table = txn.open_table(OBJECTS)?;
            for entry in table.iter()? {
                let (key, value) = entry?;
                let id = key.value();
                let meta = ObjectMeta::decode(&value.value(), &id)?;
                if meta.last_seen < cutoff {
                    stale.push(id);
                }
            }
        }
        if stale.is_empty() {
            return Ok(0);
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(OBJECTS)?;
            for id in &stale {
                table.remove(id)?;
            }
        }
        txn.commit()?;
        Ok(stale.len())
    }

    pub fn len(&self) -> Result<usize, IndexError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(OBJECTS)?;
        Ok(table.len()? as usize)
    }

    pub fn is_empty(&self) -> Result<bool, IndexError> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::slice::object_id, test_support::Scratch};

    fn meta(key: &str, len: u64) -> ObjectMeta {
        let now = now_secs();
        ObjectMeta {
            key: key.into(),
            total_len: len,
            generation: 0,
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_type: Some("application/octet-stream".into()),
            no_ranges: false,
            created: now,
            last_seen: now,
            stale: false,
        }
    }

    fn index(dir: &Scratch) -> ObjectIndex {
        ObjectIndex::open(&dir.path().join("index.redb")).unwrap()
    }

    #[test]
    fn stores_and_returns_metadata() {
        let dir = Scratch::new("index-basic");
        let ix = index(&dir);
        let id = object_id("/a");
        assert_eq!(ix.get(&id).unwrap(), None);
        let m = meta("/a", 1234);
        ix.put(&id, &m).unwrap();
        assert_eq!(ix.get(&id).unwrap(), Some(m));
    }

    #[test]
    fn round_trips_absent_and_present_optional_fields() {
        let dir = Scratch::new("index-optionals");
        let ix = index(&dir);
        let id = object_id("/b");
        let mut m = meta("/b", 1);
        m.etag = None;
        m.last_modified = Some(String::new());
        m.content_type = None;
        ix.put(&id, &m).unwrap();
        let got = ix.get(&id).unwrap().unwrap();
        assert_eq!(got.etag, None);
        assert_eq!(got.last_modified.as_deref(), Some(""));
        assert_eq!(got.content_type, None);
    }

    #[test]
    fn survives_reopening() {
        // The index is rebuildable, but it should not need rebuilding after an ordinary restart.
        let dir = Scratch::new("index-reopen");
        let id = object_id("/c");
        let m = meta("/c", 99);
        {
            let ix = index(&dir);
            ix.put(&id, &m).unwrap();
        }
        let ix = index(&dir);
        assert_eq!(ix.get(&id).unwrap(), Some(m));
    }

    #[test]
    fn touch_does_not_write_when_recently_seen() {
        // Writing on every hit would turn a busy LAN party into a stream of index writes.
        let dir = Scratch::new("index-touch");
        let ix = index(&dir);
        let id = object_id("/d");
        ix.put(&id, &meta("/d", 1)).unwrap();
        assert!(
            !ix.touch(&id).unwrap(),
            "fresh entry should not be rewritten"
        );
    }

    #[test]
    fn touch_writes_once_the_entry_is_stale() {
        let dir = Scratch::new("index-touch-stale");
        let ix = index(&dir);
        let id = object_id("/e");
        let mut m = meta("/e", 1);
        m.last_seen = now_secs() - LAST_SEEN_GRANULARITY.as_secs() - 60;
        ix.put(&id, &m).unwrap();
        assert!(ix.touch(&id).unwrap());
        assert!(ix.get(&id).unwrap().unwrap().last_seen > m.last_seen);
    }

    #[test]
    fn touching_an_unknown_object_is_not_an_error() {
        let dir = Scratch::new("index-touch-missing");
        let ix = index(&dir);
        assert!(!ix.touch(&object_id("/nope")).unwrap());
    }

    #[test]
    fn prunes_only_entries_older_than_max_age() {
        let dir = Scratch::new("index-prune");
        let ix = index(&dir);
        let fresh = object_id("/fresh");
        let stale = object_id("/stale");
        ix.put(&fresh, &meta("/fresh", 1)).unwrap();
        let mut old = meta("/stale", 1);
        old.last_seen = now_secs() - 10_000;
        ix.put(&stale, &old).unwrap();

        let removed = ix.prune(Duration::from_secs(5_000)).unwrap();
        assert_eq!(removed, 1);
        assert!(ix.get(&fresh).unwrap().is_some());
        assert!(ix.get(&stale).unwrap().is_none());
    }

    #[test]
    fn prune_on_an_empty_index_is_a_no_op() {
        let dir = Scratch::new("index-prune-empty");
        let ix = index(&dir);
        assert_eq!(ix.prune(Duration::from_secs(1)).unwrap(), 0);
        assert!(ix.is_empty().unwrap());
    }

    #[test]
    fn invalidate_bumps_the_generation_and_marks_the_entry_stale() {
        // The generation must persist so the previous version's slices stay unreachable across a
        // restart; everything else is unreliable and must be re-probed.
        let dir = Scratch::new("index-invalidate");
        let ix = index(&dir);
        let id = object_id("/i");
        ix.put(&id, &meta("/i", 100)).unwrap();

        let generation = ix.invalidate(&id).unwrap();
        assert_eq!(generation, 1);
        let got = ix.get(&id).unwrap().unwrap();
        assert!(got.stale, "entry was not marked stale");
        assert_eq!(got.generation, 1);

        // A second change bumps again rather than resetting.
        assert_eq!(ix.invalidate(&id).unwrap(), 2);
    }

    #[test]
    fn invalidating_an_unknown_object_starts_at_generation_one() {
        // Not zero: generation 0 slices may already be on disk from a previous run.
        let dir = Scratch::new("index-invalidate-unknown");
        let ix = index(&dir);
        assert_eq!(ix.invalidate(&object_id("/unknown")).unwrap(), 1);
    }

    #[test]
    fn removal_is_immediate() {
        let dir = Scratch::new("index-remove");
        let ix = index(&dir);
        let id = object_id("/f");
        ix.put(&id, &meta("/f", 1)).unwrap();
        assert_eq!(ix.len().unwrap(), 1);
        ix.remove(&id).unwrap();
        assert!(ix.get(&id).unwrap().is_none());
        assert_eq!(ix.len().unwrap(), 0);
    }

    #[test]
    fn a_truncated_entry_is_reported_rather_than_silently_wrong() {
        // If the index is corrupt we must notice and rebuild from slices, not serve bytes
        // described by half-read metadata.
        let id = object_id("/g");
        let encoded = meta("/g", 1).encode();
        let err = ObjectMeta::decode(&encoded[..encoded.len() / 2], &id).unwrap_err();
        assert!(matches!(err, IndexError::Corrupt { .. }), "{err}");
    }
}
