//! Synthetic read-only test for isolation of a depth-limited Anvil chunk.
use super::*;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use std::io::Write;

fn named(out: &mut Vec<u8>, kind: u8, name: &str) {
    out.push(kind);
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
}

fn int(out: &mut Vec<u8>, name: &str, value: i32) {
    named(out, 3, name);
    out.extend_from_slice(&value.to_be_bytes());
}

// A valid chunk containing one chest. Only the `d` compound nesting differs
// between the shallow positive control and the deliberately overdeep case.
fn chunk_with_tile(cx: i32, depth: usize) -> Vec<u8> {
    let mut nbt = Vec::new();
    named(&mut nbt, 10, ""); // root compound
    named(&mut nbt, 10, "Level");
    int(&mut nbt, "xPos", cx);
    int(&mut nbt, "zPos", 0);
    named(&mut nbt, 9, "Sections");
    nbt.push(10); // list of compounds, empty
    nbt.extend_from_slice(&0i32.to_be_bytes());
    named(&mut nbt, 9, "TileEntities");
    nbt.push(10); // list of compounds, one entry
    nbt.extend_from_slice(&1i32.to_be_bytes());
    named(&mut nbt, 8, "id");
    nbt.extend_from_slice(&5u16.to_be_bytes());
    nbt.extend_from_slice(b"Chest");
    int(&mut nbt, "x", cx * 16);
    int(&mut nbt, "y", 64);
    int(&mut nbt, "z", 0);
    for _ in 0..depth {
        named(&mut nbt, 10, "d");
    }
    nbt.extend(std::iter::repeat_n(0, depth)); // each nested compound
    nbt.extend_from_slice(&[0, 0, 0]); // tile, Level and root compounds
    nbt
}

fn compressed_zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn two_chunk_region(bad: &[u8], good: &[u8]) -> Vec<u8> {
    let mut region = vec![0u8; 4 * SECTOR_BYTES];
    for (index, payload) in [bad, good].into_iter().enumerate() {
        let compressed = compressed_zlib(payload);
        assert!(compressed.len() + 5 <= SECTOR_BYTES);
        let sector = index + 2;
        region[index * 4..index * 4 + 4].copy_from_slice(&[0, 0, sector as u8, 1]);
        let start = sector * SECTOR_BYTES;
        region[start..start + 4].copy_from_slice(&((compressed.len() + 1) as u32).to_be_bytes());
        region[start + 4] = 2; // zlib
        region[start + 5..start + 5 + compressed.len()].copy_from_slice(&compressed);
    }
    region
}

fn create_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn overdeep_tile_rejects_only_its_chunk_and_keeps_region_unchanged() {
    use crate::level_dat::{LevelData, LevelRoot};
    use std::collections::BTreeMap;

    let shallow = chunk_with_tile(0, 2);
    let control = crate::legacy_nbt::parse_chunk(&shallow, 0, 0).unwrap();
    assert_eq!(control.level.tile_entities.len(), 1);
    let good = chunk_with_tile(1, 2);
    assert_eq!(
        crate::legacy_nbt::parse_chunk(&good, 1, 0).unwrap().level.x,
        1
    );
    let deep = chunk_with_tile(0, 512);
    let err = match crate::legacy_nbt::parse_chunk(&deep, 0, 0) {
        Ok(_) => panic!("overdeep TileEntity must fail"),
        Err(error) => error,
    };
    assert_eq!(err.kind(), ErrorKind::InvalidData);
    assert_eq!(err.to_string(), "nbt nesting limit");

    // The test's unique root is created, never removed or reused here. The
    // harness owns the surrounding synthetic TEMP and preserves failures.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ir1710-depth-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("region")).unwrap();
    let level = LevelRoot {
        data: LevelData {
            format_version: 19133,
            name: "synthetic depth isolation".into(),
            seed: 0,
            spawn_x: 0,
            spawn_y: 64,
            spawn_z: 0,
            time: 0,
            day_time: None,
            game_type: None,
            generator: None,
            raining: None,
            thundering: None,
            rain_time: None,
            thunder_time: None,
            game_rules: None,
            extra: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    };
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&fastnbt::to_bytes(&level).unwrap()).unwrap();
    let level_bytes = gz.finish().unwrap();
    let region_bytes = two_chunk_region(&deep, &good);
    let region_path = root.join("region/r.0.0.mca");
    create_file(&root.join("level.dat"), &level_bytes);
    create_file(&region_path, &region_bytes);

    let mut cache = ReadOnlyChunkCache::open(&root, 2).unwrap();
    let mut first_good = None;
    for _ in 0..2 {
        let order_before = cache.order.clone();
        let error = match cache.get(Dimension::Overworld, 0, 0) {
            Ok(_) => panic!("overdeep chunk must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "nbt nesting limit");
        assert!(!cache.entries.contains_key(&(Dimension::Overworld, 0, 0)));
        assert_eq!(cache.order, order_before);
        let good_chunk = cache.get(Dimension::Overworld, 1, 0).unwrap().unwrap();
        if let Some(previous) = first_good.as_ref() {
            assert!(Arc::ptr_eq(previous, &good_chunk));
        } else {
            first_good = Some(Arc::clone(&good_chunk));
        }
        assert_eq!((good_chunk.level.x, good_chunk.level.z), (1, 0));
        assert_eq!(good_chunk.level.tile_entities.len(), 1);
        assert_eq!(fs::read(&region_path).unwrap(), region_bytes);
        assert_eq!(fs::read(root.join("level.dat")).unwrap(), level_bytes);
    }
    assert_eq!(cache.entries.len(), 1);
}
