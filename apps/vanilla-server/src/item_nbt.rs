//! Lossless, bounded Protocol 5 ItemStack NBT in an interned process-local arena.
//! A Stack remains Copy; its handle is never serialized as an integer. Saves
//! embed the *original compressed NBT bytes*, which are re-interned on load.
//! This preserves the wire representation and all unknown tags through moves.
use fastnbt::Value;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Deserializer, Serializer};
use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::{Mutex, OnceLock};

const MAX_WIRE: usize = i16::MAX as usize;
const MAX_DECOMPRESSED: usize = 256 * 1024;
const MAX_ARENA: usize = 64 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}
#[derive(Default)]
struct Arena {
    next: u64,
    bytes: usize,
    ids: HashMap<Vec<u8>, u64>,
    blobs: HashMap<u64, Vec<u8>>,
}
static ARENA: OnceLock<Mutex<Arena>> = OnceLock::new();

fn arena() -> &'static Mutex<Arena> {
    ARENA.get_or_init(|| Mutex::new(Arena::default()))
}

// Validate the tag envelope before deserializing an untrusted NBT tree.
// Iterative traversal bounds nesting even when fastnbt's Value recursion
// would otherwise exhaust the stack; array lengths are checked *before*
// allocating their decoded representation.
const MAX_NBT_DEPTH: usize = 64;

enum Container {
    Compound,
    List { kind: u8, remaining: usize },
}

fn take(raw: &[u8], offset: &mut usize, len: usize) -> io::Result<()> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| invalid("NBT length overflow"))?;
    if end > raw.len() {
        return Err(invalid("truncated NBT"));
    }
    *offset = end;
    Ok(())
}

fn byte(raw: &[u8], offset: &mut usize) -> io::Result<u8> {
    let value = *raw.get(*offset).ok_or_else(|| invalid("truncated NBT"))?;
    *offset += 1;
    Ok(value)
}

fn short(raw: &[u8], offset: &mut usize) -> io::Result<usize> {
    let a = byte(raw, offset)?;
    let b = byte(raw, offset)?;
    Ok(u16::from_be_bytes([a, b]) as usize)
}

fn count(raw: &[u8], offset: &mut usize) -> io::Result<usize> {
    let bytes = [
        byte(raw, offset)?,
        byte(raw, offset)?,
        byte(raw, offset)?,
        byte(raw, offset)?,
    ];
    usize::try_from(i32::from_be_bytes(bytes))
        .map_err(|_| invalid("negative NBT array or list length"))
}

fn payload(raw: &[u8], offset: &mut usize, kind: u8) -> io::Result<Option<Container>> {
    match kind {
        1 => take(raw, offset, 1)?,
        2 => take(raw, offset, 2)?,
        3 | 5 => take(raw, offset, 4)?,
        4 | 6 => take(raw, offset, 8)?,
        7 => {
            let size = count(raw, offset)?;
            take(raw, offset, size)?;
        }
        8 => {
            let size = short(raw, offset)?;
            take(raw, offset, size)?;
        }
        9 => {
            let element = byte(raw, offset)?;
            let remaining = count(raw, offset)?;
            if element > 11 || (element == 0 && remaining != 0) {
                return Err(invalid("invalid NBT list element"));
            }
            // Every non-empty element consumes at least one byte.
            if remaining > raw.len().saturating_sub(*offset) {
                return Err(invalid("NBT list exceeds remaining input"));
            }
            return Ok(Some(Container::List {
                kind: element,
                remaining,
            }));
        }
        10 => return Ok(Some(Container::Compound)),
        11 => {
            let len = count(raw, offset)?;
            take(
                raw,
                offset,
                len.checked_mul(4)
                    .ok_or_else(|| invalid("NBT int-array length overflow"))?,
            )?;
        }
        _ => return Err(invalid("unknown or unsupported Protocol 5 NBT tag")),
    }
    Ok(None)
}

fn validate_nbt_wire(raw: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    if byte(raw, &mut offset)? != 10 {
        return Err(invalid("ItemStack NBT root must be compound"));
    }
    let root_name = short(raw, &mut offset)?;
    take(raw, &mut offset, root_name)?;
    let mut stack = vec![Container::Compound];
    while !stack.is_empty() {
        let kind = match stack.last_mut().expect("nonempty stack") {
            Container::Compound => {
                let kind = byte(raw, &mut offset)?;
                if kind == 0 {
                    stack.pop();
                    continue;
                }
                let name = short(raw, &mut offset)?;
                take(raw, &mut offset, name)?;
                kind
            }
            Container::List { kind, remaining } => {
                if *remaining == 0 {
                    stack.pop();
                    continue;
                }
                *remaining -= 1;
                *kind
            }
        };
        if let Some(child) = payload(raw, &mut offset, kind)? {
            if stack.len() >= MAX_NBT_DEPTH {
                return Err(invalid("ItemStack NBT nesting limit"));
            }
            stack.push(child);
        }
    }
    if offset != raw.len() {
        return Err(invalid("trailing ItemStack NBT data"));
    }
    Ok(())
}

fn decoded(bytes: &[u8]) -> io::Result<Value> {
    if bytes.is_empty() || bytes.len() > MAX_WIRE {
        return Err(invalid("invalid compressed ItemStack NBT length"));
    }
    let mut raw = Vec::new();
    GzDecoder::new(bytes)
        .take((MAX_DECOMPRESSED + 1) as u64)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_DECOMPRESSED {
        return Err(invalid("ItemStack NBT decompression limit"));
    }
    validate_nbt_wire(&raw)?;
    match fastnbt::from_bytes::<Value>(&raw) {
        Ok(value @ Value::Compound(_)) => Ok(value),
        _ => Err(invalid("ItemStack NBT must be a compound")),
    }
}

/// Validate before inserting, even for Creative packets and disk saves.
pub fn intern(bytes: &[u8]) -> io::Result<u64> {
    decoded(bytes)?;
    let mut state = arena().lock().map_err(|_| invalid("NBT lock poisoned"))?;
    if let Some(&id) = state.ids.get(bytes) {
        return Ok(id);
    }
    if state.bytes.saturating_add(bytes.len()) > MAX_ARENA {
        return Err(invalid("ItemStack NBT arena full"));
    }
    state.next = state
        .next
        .checked_add(1)
        .ok_or_else(|| invalid("NBT handle overflow"))?;
    let id = state.next;
    state.bytes += bytes.len();
    state.ids.insert(bytes.to_vec(), id);
    state.blobs.insert(id, bytes.to_vec());
    Ok(id)
}
pub fn wire(handle: u64) -> Option<Vec<u8>> {
    arena().lock().ok()?.blobs.get(&handle).cloned()
}
pub fn is_live(handle: u64) -> bool {
    arena()
        .lock()
        .is_ok_and(|state| state.blobs.contains_key(&handle))
}
pub fn value(handle: u64) -> io::Result<Value> {
    decoded(&wire(handle).ok_or_else(|| invalid("unknown NBT handle"))?)
}
pub fn from_value(value: &Value) -> io::Result<u64> {
    if !matches!(value, Value::Compound(_)) {
        return Err(invalid("ItemStack NBT root must be compound"));
    }
    let raw = fastnbt::to_bytes(value).map_err(|_| invalid("cannot serialize ItemStack NBT"))?;
    if raw.len() > MAX_DECOMPRESSED {
        return Err(invalid("ItemStack NBT too large"));
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw)?;
    intern(&encoder.finish()?)
}

/// JSON inventory/container saves store bytes, never arena handles.
pub fn serialize<S: Serializer>(handle: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
    match handle {
        None => serializer.serialize_none(),
        Some(id) => {
            let bytes = wire(*id).ok_or_else(|| serde::ser::Error::custom("unknown NBT handle"))?;
            serializer.serialize_some(&bytes)
        }
    }
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    let data = Option::<Vec<u8>>::deserialize(deserializer)?;
    data.map(|bytes| intern(&bytes).map_err(serde::de::Error::custom))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct SavedTag {
        #[serde(with = "crate::item_nbt")]
        nbt: Option<u64>,
    }

    #[test]
    fn json_persistence_stores_nbt_bytes_not_process_local_handles() {
        let tag = Value::Compound(HashMap::from([(
            "UnknownCustomMetadata".to_string(),
            Value::IntArray(fastnbt::IntArray::new(vec![1, 0, -19])),
        )]));
        let handle = from_value(&tag).unwrap();
        let source = wire(handle).unwrap();

        let json = serde_json::to_value(SavedTag { nbt: Some(handle) }).unwrap();
        assert!(json["nbt"].is_array(), "NBT must be persisted as bytes");
        let recovered: SavedTag = serde_json::from_value(json).unwrap();
        assert_eq!(wire(recovered.nbt.unwrap()).unwrap(), source);
        assert_eq!(value(recovered.nbt.unwrap()).unwrap(), tag);

        let absent = serde_json::to_value(SavedTag { nbt: None }).unwrap();
        let restored: SavedTag = serde_json::from_value(absent).unwrap();
        assert!(restored.nbt.is_none());
        assert!(serde_json::to_value(SavedTag {
            nbt: Some(u64::MAX)
        })
        .is_err());
        assert!(serde_json::from_value::<SavedTag>(serde_json::json!({"nbt": [1, 2, 3]})).is_err());
    }

    #[test]
    fn unknown_nested_tags_and_arrays_keep_identical_wire_bytes() {
        let mut compound = HashMap::new();
        compound.insert("Strange".into(), Value::String("stay".into()));
        compound.insert(
            "Scores".into(),
            Value::IntArray(fastnbt::IntArray::new(vec![7, -1, 99])),
        );
        compound.insert(
            "nested".into(),
            Value::Compound(HashMap::from([
                ("id".into(), Value::Short(34)),
                ("lvl".into(), Value::Short(2)),
            ])),
        );
        let id = from_value(&Value::Compound(compound)).unwrap();
        let original = wire(id).unwrap();
        assert_eq!(intern(&original).unwrap(), id);
        assert_eq!(wire(id).unwrap(), original);
        assert!(matches!(value(id).unwrap(), Value::Compound(_)));
        assert!(intern(&[0, 1, 2]).is_err());
    }

    fn gzip_raw(raw: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap()
    }

    fn nested_compound(depth: usize) -> Vec<u8> {
        // Root Compound followed by 'depth' named Compound children,
        // each with an empty name. Close every compound with TAG_End.
        let mut raw = vec![10, 0, 0];
        for _ in 0..depth {
            raw.extend_from_slice(&[10, 0, 0]);
        }
        raw.extend(std::iter::repeat_n(0, depth + 1));
        raw
    }

    #[test]
    fn add007_nbt_preflight_enforces_depth_without_recursion() {
        assert!(validate_nbt_wire(&nested_compound(MAX_NBT_DEPTH - 1)).is_ok());
        assert!(validate_nbt_wire(&nested_compound(MAX_NBT_DEPTH)).is_err());
        assert!(intern(&gzip_raw(&nested_compound(MAX_NBT_DEPTH))).is_err());
    }

    #[test]
    fn add007_rejects_array_length_lies_and_illegal_list_tags_before_deserialization() {
        // Named ByteArray "b" with a negative length.
        let negative = [10, 0, 0, 7, 0, 1, b'b', 255, 255, 255, 255, 0];
        // Named IntArray "i" with an impossible count, before reading elements.
        let enormous = [10, 0, 0, 11, 0, 1, b'i', 0x7f, 0xff, 0xff, 0xff, 0];
        // TAG_List "l" declares a nonempty list of TAG_End (not valid).
        let illegal = [10, 0, 0, 9, 0, 1, b'l', 0, 0, 0, 0, 1, 0];
        // Correct scalar tag, but its payload is truncated.
        let truncated = [10, 0, 0, 4, 0, 1, b'n', 0, 0, 0];
        for bytes in [negative.as_slice(), enormous.as_slice(), illegal.as_slice(), truncated.as_slice()] {
            assert!(validate_nbt_wire(bytes).is_err());
            assert!(intern(&gzip_raw(bytes)).is_err());
        }
    }
}
