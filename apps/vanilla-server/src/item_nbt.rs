//! Lossless, bounded Protocol 5 ItemStack NBT in an interned process-local arena.
//! A Stack remains Copy; its handle is never serialized as an integer. Saves
//! embed the *original compressed NBT bytes*, which are re-interned on load.
//! This preserves the wire representation and all unknown tags through moves.
use fastnbt::Value;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
    state.next = state.next.checked_add(1).ok_or_else(|| invalid("NBT handle overflow"))?;
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
    arena().lock().is_ok_and(|state| state.blobs.contains_key(&handle))
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

    #[test]
    fn unknown_nested_tags_and_arrays_keep_identical_wire_bytes() {
        let mut compound = HashMap::new();
        compound.insert("Strange".into(), Value::String("stay".into()));
        compound.insert("Scores".into(), Value::IntArray(fastnbt::IntArray::new(vec![7, -1, 99])));
        compound.insert("nested".into(), Value::Compound(HashMap::from([
            ("id".into(), Value::Short(34)),
            ("lvl".into(), Value::Short(2)),
        ])));
        let id = from_value(&Value::Compound(compound)).unwrap();
        let original = wire(id).unwrap();
        assert_eq!(intern(&original).unwrap(), id);
        assert_eq!(wire(id).unwrap(), original);
        assert!(matches!(value(id).unwrap(), Value::Compound(_)));
        assert!(intern(&[0, 1, 2]).is_err());
    }
}
