//! Protocol 5 writable and signed books. Book IDs in the *internal* Stack.damage
//! field refer to world-owned NBT records; network damage is always zero.
//! The item and record are journaled together on edit/sign. Anvil export of
//! these records is not implemented.
use crate::inventory::{Inventory, Stack};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_NBT: u64 = 32 * 1024;
const MAX_PAGES: usize = 50;
const MAX_PAGE_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 16;
const VERSION: u8 = 1;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    pub pages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}
impl Book {
    fn valid(&self, signed: bool) -> bool {
        !self.pages.is_empty()
            && self.pages.len() <= MAX_PAGES
            && self.pages.iter().all(|page| page.chars().count() <= MAX_PAGE_CHARS)
            && if signed {
                self.title.as_ref().is_some_and(|s| !s.is_empty() && s.chars().count() <= MAX_TITLE_CHARS)
                    && self.author.as_ref().is_some_and(|s| !s.is_empty() && s.chars().count() <= 16)
            } else {
                self.title.is_none() && self.author.is_none()
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookRecord {
    signed: bool,
    book: Book,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u8,
    next_id: u16,
    books: BTreeMap<u16, BookRecord>,
}
impl Default for Store {
    fn default() -> Self {
        Self { version: VERSION, next_id: 1, books: BTreeMap::new() }
    }
}
impl Store {
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.next_id == 0 || self.next_id > 32768
            || self.books.len() > 32767
            || self.books.iter().any(|(&id, record)|
                id == 0 || id >= self.next_id || !record.book.valid(record.signed))
        {
            return Err(invalid("invalid book store"));
        }
        Ok(())
    }
    fn load(path: &Path) -> io::Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_FILE {
            return Err(invalid("book file size or type"));
        }
        let value: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|_| invalid("malformed book store"))?;
        value.validate()?;
        Ok(value)
    }
    fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("serialize books"))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(invalid("book store full"));
        }
        Ok(bytes)
    }
}

struct Service {
    path: PathBuf,
    state: Mutex<Store>,
}
static SERVICE: OnceLock<Service> = OnceLock::new();

pub fn initialize(world_path: &Path) -> io::Result<()> {
    let path = world_path.with_extension("books.json");
    let state = Store::load(&path)?;
    SERVICE.set(Service { path, state })
        .map_err(|_| invalid("books initialized more than once"))
}

/// Create a wire-format NBT body for a book. Return None for blank books and
/// corrupt/orphaned references; those are never populated from a client packet.
/// Caller writes the short length and the bytes.
pub fn wire_nbt(stack: Stack) -> Option<Vec<u8>> {
    if !matches!(stack.id, 386 | 387) || stack.damage <= 0 {
        return None;
    }
    let service = SERVICE.get()?;
    let guard = service.state.lock().ok()?;
    let record = guard.books.get(&(stack.damage as u16))?;
    if record.signed != (stack.id == 387) {
        return None;
    }
    compressed_nbt(&record.book).ok()
}
fn compressed_nbt(book: &Book) -> io::Result<Vec<u8>> {
    let raw = fastnbt::to_bytes(book).map_err(|_| invalid("serialize book NBT"))?;
    if raw.len() as u64 > MAX_NBT { return Err(invalid("book NBT exceeds limit")); }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw)?;
    let bytes = encoder.finish()?;
    if bytes.len() > i16::MAX as usize { return Err(invalid("book compressed NBT too large")); }
    Ok(bytes)
}

/// Decode MC|BEdit / MC|BSign payload: Slot (ID, count, damage, Short-length
/// compressed NBT). Never copy client-supplied author into signed books.
fn incoming(payload: &[u8], signing: bool) -> io::Result<(i16, Book)> {
    if payload.len() < 7 { return Err(invalid("short book slot")); }
    let id = i16::from_be_bytes([payload[0], payload[1]]);
    if id != if signing { 387 } else { 386 } || payload[2] != 1 {
        return Err(invalid("unexpected book slot"));
    }
    let damage = i16::from_be_bytes([payload[3], payload[4]]);
    let length = i16::from_be_bytes([payload[5], payload[6]]);
    if damage < 0 || length <= 0 || length as usize != payload.len() - 7 {
        return Err(invalid("bad book NBT length"));
    }
    let mut decoder = GzDecoder::new(&payload[7..]);
    let mut raw = Vec::new();
    decoder.take(MAX_NBT + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_NBT { return Err(invalid("book NBT inflated too large")); }
    let book: Book = fastnbt::from_bytes(&raw).map_err(|_| invalid("invalid book NBT"))?;
    // The client can include an author field when signing, but authority is
    // solely the authenticated session name (in offline mode, not verified).
    if book.pages.is_empty() || book.pages.len() > MAX_PAGES
        || book.pages.iter().any(|p| p.chars().count() > MAX_PAGE_CHARS)
        || (signing && !book.title.as_ref().is_some_and(|t| !t.is_empty() && t.chars().count() <= MAX_TITLE_CHARS))
    { return Err(invalid("invalid book pages or title")); }
    Ok((damage, book))
}

/// An edit targets only the *currently held* unsigned book. Both the updated
/// inventory and the world-wide NBT store are committed in one redo transaction.
pub fn process(
    channel: &str, payload: &[u8], inventory: &mut Inventory,
    inventory_path: &Path, world_path: &Path, author: &str,
) -> io::Result<bool> {
    let signing = match channel {
        "MC|BEdit" => false,
        "MC|BSign" => true,
        _ => return Ok(false),
    };
    let (claimed_damage, mut book) = incoming(payload, signing)?;
    if !author.chars().count().ge(&1) || author.chars().count() > 16 {
        return Err(invalid("invalid book author"));
    }
    let held = match inventory.held() {
        Some(s) if s.id == 386 && s.count == 1 && s.damage == claimed_damage => s,
        _ => return Ok(false),
    };
    if signing {
        book.author = Some(author.to_owned());
    } else {
        book.title = None;
        book.author = None;
    }
    if !book.valid(signing) { return Err(invalid("invalid final book")); }
    let service = SERVICE.get().ok_or_else(|| invalid("books not initialized"))?;
    let mut store = service.state.lock().map_err(|_| invalid("books mutex poisoned"))?;
    let mut next = store.clone();
    let id = if held.damage == 0 {
        if next.next_id > i16::MAX as u16 { return Err(invalid("book ID exhausted")); }
        let id = next.next_id;
        next.next_id += 1;
        id
    } else {
        let id = held.damage as u16;
        if !next.books.get(&id).is_some_and(|record| !record.signed) {
            return Ok(false);
        }
        id
    };
    next.books.insert(id, BookRecord { signed: signing, book });
    let mut changed = inventory.clone();
    if !changed.replace_held_book(held, if signing { 387 } else { 386 }, id as i16) {
        return Ok(false);
    }
    let inventory_bytes = changed.encoded()?;
    let books_bytes = next.encoded()?;
    crate::journal::commit(
        &world_path.with_extension("pending-txn.json"),
        &[(inventory_path, inventory_bytes), (&service.path, books_bytes)],
    )?;
    *inventory = changed;
    *store = next;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_pages_and_unauthenticated_title() {
        let mut b = Book { pages: vec!["hello".into()], title: None, author: None };
        assert!(b.valid(false));
        b.pages[0] = "x".repeat(257);
        assert!(!b.valid(false));
        b.pages[0] = "test".into();
        b.title = Some("Title".into());
        b.author = Some("Tester".into());
        assert!(b.valid(true));
        assert!(!b.valid(false));
    }
    #[test]
    fn compressed_book_nbt_is_a_valid_legacy_compound() {
        let b = Book { pages: vec!["hello".into(), "world".into()], title: None, author: None };
        let bytes = compressed_nbt(&b).unwrap();
        let mut raw = Vec::new();
        GzDecoder::new(bytes.as_slice()).read_to_end(&mut raw).unwrap();
        assert_eq!(fastnbt::from_bytes::<Book>(&raw).unwrap(), b);
        let mut payload = vec![];
        payload.extend_from_slice(&386i16.to_be_bytes());
        payload.push(1);
        payload.extend_from_slice(&0i16.to_be_bytes());
        payload.extend_from_slice(&(bytes.len() as i16).to_be_bytes());
        payload.extend_from_slice(&bytes);
        assert_eq!(incoming(&payload, false).unwrap().1, b);
        assert!(incoming(&payload, true).is_err());
    }
    #[test]
    fn rejects_missing_or_forged_book_reference() {
        let mut store = Store::default();
        store.next_id = 2;
        store.books.insert(1, BookRecord { signed: false, book: Book { pages: vec!["p".into()], title: None, author: None }});
        assert!(store.validate().is_ok());
        store.books.get_mut(&1).unwrap().signed = true;
        assert!(store.validate().is_err());
    }
}
