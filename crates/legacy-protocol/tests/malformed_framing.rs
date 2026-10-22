//! Deterministic malformed-frame smoke testing; not a substitute for coverage-guided fuzzing.
use legacy_protocol::{read_frame, read_varint, BufferedFrameReader, MAX_FRAME_BYTES};
use std::io::Cursor;

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn random_short_protocol_inputs_do_not_panic() {
    let mut seed = 0x1710_5eed_2026_1022_u64;
    for _ in 0..20_000 {
        let length = (next(&mut seed) % 128) as usize;
        let bytes: Vec<u8> = (0..length).map(|_| next(&mut seed) as u8).collect();
        let _ = read_varint(&mut Cursor::new(&bytes));
        // The read_frame parser bounds allocation using MAX_FRAME_BYTES.
        let _ = read_frame(&mut Cursor::new(&bytes));
        let mut buffered = BufferedFrameReader::new();
        let _ = buffered.poll(&mut Cursor::new(&bytes));
    }
}

#[test]
fn oversized_frame_length_is_rejected_before_allocation() {
    let length = (MAX_FRAME_BYTES + 1) as u32;
    let mut bytes = Vec::new();
    legacy_protocol::write_varint(length, &mut bytes);
    assert!(read_frame(&mut Cursor::new(&bytes)).is_err());
    let mut buffered = BufferedFrameReader::new();
    assert!(buffered.poll(&mut Cursor::new(&bytes)).is_err());
}
