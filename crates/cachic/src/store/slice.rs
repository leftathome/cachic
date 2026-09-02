//! Slice key and the self-describing slice value.
//!
//! Promoted from the M0 spike (TASK-03), where the codec, its checksum verification and its
//! failure modes were already covered by tests.
//!
//! Slices carry everything needed to reconstruct the object index (FR-44), which is what makes
//! the index a rebuildable acceleration structure rather than the source of truth. The checksum
//! is verified on decode, so a corrupt slice fails to load rather than being served (FR-42).

use std::io::{Read, Write};

use bytes::Bytes;
use foyer::{Code, Error, ErrorKind, Result};

/// Marks a well-formed slice payload. A mismatch means the bytes are not ours.
const MAGIC: u32 = 0x4341_4331; // "CAC1"

/// Identifies an object. `blake3(identifier || normalised_key)[..16]`.
pub type ObjectId = [u8; 16];

/// Compute an object id from a raw string.
///
/// Production code derives object ids through [`crate::services::key::CacheKey::object_id`],
/// which folds in the service identifier. This exists for tests and for tools that already hold
/// a fully-qualified key.
pub fn object_id(key: &str) -> ObjectId {
    let hash = blake3::hash(key.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash.as_bytes()[..16]);
    id
}

/// Addresses one slice of one generation of one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SliceKey {
    pub object: ObjectId,
    /// Bumped when an object's validators change, which makes invalidation atomic: old-generation
    /// slices simply become unreachable (FR-14).
    pub generation: u32,
    pub index: u32,
}

impl SliceKey {
    pub const ENCODED_LEN: usize = 16 + 4 + 4;

    pub fn new(object: ObjectId, generation: u32, index: u32) -> Self {
        Self {
            object,
            generation,
            index,
        }
    }
}

impl Code for SliceKey {
    fn encode(&self, writer: &mut impl Write) -> Result<()> {
        writer.write_all(&self.object).map_err(Error::io_error)?;
        writer
            .write_all(&self.generation.to_le_bytes())
            .map_err(Error::io_error)?;
        writer
            .write_all(&self.index.to_le_bytes())
            .map_err(Error::io_error)?;
        Ok(())
    }

    fn decode(reader: &mut impl Read) -> Result<Self> {
        let mut object = [0u8; 16];
        reader.read_exact(&mut object).map_err(Error::io_error)?;
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).map_err(Error::io_error)?;
        let generation = u32::from_le_bytes(buf);
        reader.read_exact(&mut buf).map_err(Error::io_error)?;
        let index = u32::from_le_bytes(buf);
        Ok(Self {
            object,
            generation,
            index,
        })
    }

    fn estimated_size(&self) -> usize {
        Self::ENCODED_LEN
    }
}

/// Everything a slice knows about the object it belongs to.
///
/// This is the payload of FR-44: given only the stored slices, the object index can be rebuilt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    pub slice_size: u32,
    pub total_len: u64,
    pub generation: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
}

impl SliceHeader {
    fn encode(&self, writer: &mut impl Write) -> Result<()> {
        writer
            .write_all(&self.slice_size.to_le_bytes())
            .map_err(Error::io_error)?;
        writer
            .write_all(&self.total_len.to_le_bytes())
            .map_err(Error::io_error)?;
        writer
            .write_all(&self.generation.to_le_bytes())
            .map_err(Error::io_error)?;
        write_opt_str(writer, self.etag.as_deref())?;
        write_opt_str(writer, self.last_modified.as_deref())?;
        write_opt_str(writer, self.content_type.as_deref())?;
        Ok(())
    }

    fn decode(reader: &mut impl Read) -> Result<Self> {
        let mut b4 = [0u8; 4];
        let mut b8 = [0u8; 8];
        reader.read_exact(&mut b4).map_err(Error::io_error)?;
        let slice_size = u32::from_le_bytes(b4);
        reader.read_exact(&mut b8).map_err(Error::io_error)?;
        let total_len = u64::from_le_bytes(b8);
        reader.read_exact(&mut b4).map_err(Error::io_error)?;
        let generation = u32::from_le_bytes(b4);
        Ok(Self {
            slice_size,
            total_len,
            generation,
            etag: read_opt_str(reader)?,
            last_modified: read_opt_str(reader)?,
            content_type: read_opt_str(reader)?,
        })
    }

    fn encoded_len(&self) -> usize {
        4 + 8
            + 4
            + opt_str_len(self.etag.as_deref())
            + opt_str_len(self.last_modified.as_deref())
            + opt_str_len(self.content_type.as_deref())
    }
}

fn opt_str_len(s: Option<&str>) -> usize {
    4 + s.map_or(0, |v| v.len())
}

fn write_opt_str(writer: &mut impl Write, s: Option<&str>) -> Result<()> {
    match s {
        // u32::MAX marks absent, which keeps the empty string distinguishable from None.
        None => writer
            .write_all(&u32::MAX.to_le_bytes())
            .map_err(Error::io_error),
        Some(v) => {
            let len = u32::try_from(v.len()).map_err(|_| {
                Error::new(
                    ErrorKind::OutOfRange,
                    format!("header string too long: {} bytes", v.len()),
                )
            })?;
            writer
                .write_all(&len.to_le_bytes())
                .map_err(Error::io_error)?;
            writer.write_all(v.as_bytes()).map_err(Error::io_error)
        }
    }
}

fn read_opt_str(reader: &mut impl Read) -> Result<Option<String>> {
    let mut b4 = [0u8; 4];
    reader.read_exact(&mut b4).map_err(Error::io_error)?;
    let len = u32::from_le_bytes(b4);
    if len == u32::MAX {
        return Ok(None);
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).map_err(Error::io_error)?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| Error::new(ErrorKind::Parse, format!("header string is not utf-8: {e}")))
}

/// A stored slice: its self-describing header plus the payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceValue {
    pub header: SliceHeader,
    pub payload: Bytes,
}

impl SliceValue {
    pub fn new(header: SliceHeader, payload: Bytes) -> Self {
        Self { header, payload }
    }
}

impl Code for SliceValue {
    fn encode(&self, writer: &mut impl Write) -> Result<()> {
        writer
            .write_all(&MAGIC.to_le_bytes())
            .map_err(Error::io_error)?;
        self.header.encode(writer)?;
        let checksum = xxhash_rust::xxh3::xxh3_64(&self.payload);
        writer
            .write_all(&checksum.to_le_bytes())
            .map_err(Error::io_error)?;
        let len = u32::try_from(self.payload.len()).map_err(|_| {
            Error::new(
                ErrorKind::OutOfRange,
                format!("slice payload too large: {}", self.payload.len()),
            )
        })?;
        writer
            .write_all(&len.to_le_bytes())
            .map_err(Error::io_error)?;
        writer.write_all(&self.payload).map_err(Error::io_error)?;
        Ok(())
    }

    fn decode(reader: &mut impl Read) -> Result<Self> {
        let mut b4 = [0u8; 4];
        let mut b8 = [0u8; 8];
        reader.read_exact(&mut b4).map_err(Error::io_error)?;
        let magic = u32::from_le_bytes(b4);
        if magic != MAGIC {
            return Err(Error::new(
                ErrorKind::MagicMismatch,
                format!("slice magic mismatch: expected {MAGIC:#x}, found {magic:#x}"),
            ));
        }
        let header = SliceHeader::decode(reader)?;
        reader.read_exact(&mut b8).map_err(Error::io_error)?;
        let expected = u64::from_le_bytes(b8);
        reader.read_exact(&mut b4).map_err(Error::io_error)?;
        let len = u32::from_le_bytes(b4) as usize;
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).map_err(Error::io_error)?;
        let actual = xxhash_rust::xxh3::xxh3_64(&payload);
        if actual != expected {
            // A corrupt slice is dropped and refetched, never served (FR-42).
            return Err(Error::new(
                ErrorKind::ChecksumMismatch,
                format!("slice checksum mismatch: expected {expected:#x}, computed {actual:#x}"),
            ));
        }
        Ok(Self {
            header,
            payload: Bytes::from(payload),
        })
    }

    fn estimated_size(&self) -> usize {
        4 + self.header.encoded_len() + 8 + 4 + self.payload.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> SliceHeader {
        SliceHeader {
            slice_size: 1024,
            total_len: 10_000,
            generation: 3,
            etag: Some("\"abc123\"".into()),
            last_modified: None,
            content_type: Some("application/octet-stream".into()),
        }
    }

    fn roundtrip<T: Code>(value: &T) -> Result<T> {
        let mut buf = Vec::new();
        value.encode(&mut buf)?;
        T::decode(&mut buf.as_slice())
    }

    #[test]
    fn slice_key_roundtrips() {
        let key = SliceKey::new(object_id("/o/game/1000"), 7, 42);
        assert_eq!(roundtrip(&key).unwrap(), key);
        assert_eq!(key.estimated_size(), SliceKey::ENCODED_LEN);
    }

    #[test]
    fn slice_value_roundtrips() {
        let value = SliceValue::new(header(), Bytes::from_static(b"hello world"));
        assert_eq!(roundtrip(&value).unwrap(), value);
    }

    #[test]
    fn estimated_size_matches_encoded_length() {
        // foyer uses the estimate to pick between engines, so a wrong estimate is a silent
        // capacity-accounting bug rather than a crash.
        let value = SliceValue::new(header(), Bytes::from(vec![7u8; 4096]));
        let mut buf = Vec::new();
        value.encode(&mut buf).unwrap();
        assert_eq!(value.estimated_size(), buf.len());
    }

    #[test]
    fn absent_and_empty_header_strings_are_distinct() {
        let mut h = header();
        h.etag = Some(String::new());
        h.last_modified = None;
        let value = SliceValue::new(h.clone(), Bytes::from_static(b"x"));
        let decoded = roundtrip(&value).unwrap();
        assert_eq!(decoded.header.etag.as_deref(), Some(""));
        assert_eq!(decoded.header.last_modified, None);
    }

    #[test]
    fn corrupt_payload_fails_to_decode() {
        let value = SliceValue::new(header(), Bytes::from(vec![1u8; 512]));
        let mut buf = Vec::new();
        value.encode(&mut buf).unwrap();
        // Flip a bit in the payload, which is the last 512 bytes.
        let last = buf.len() - 1;
        buf[last] ^= 0x01;
        let err = SliceValue::decode(&mut buf.as_slice()).unwrap_err();
        assert!(
            err.to_string().contains("checksum mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn foreign_bytes_fail_to_decode() {
        let mut buf = vec![0u8; 64];
        buf[..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let err = SliceValue::decode(&mut buf.as_slice()).unwrap_err();
        assert!(
            err.to_string().contains("magic mismatch"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn truncated_payload_fails_to_decode() {
        // A torn write must read back as an error, not as a short slice (FR-42).
        let value = SliceValue::new(header(), Bytes::from(vec![9u8; 1024]));
        let mut buf = Vec::new();
        value.encode(&mut buf).unwrap();
        buf.truncate(buf.len() - 100);
        assert!(SliceValue::decode(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn object_ids_differ_per_key() {
        assert_ne!(object_id("/o/a/1"), object_id("/o/b/1"));
        assert_eq!(object_id("/o/a/1"), object_id("/o/a/1"));
    }
}
