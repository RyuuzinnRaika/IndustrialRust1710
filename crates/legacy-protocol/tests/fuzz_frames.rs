//! Deterministic J98 smoke fuzzing for malformed Protocol 5 frames.
//! Run full coverage guided fuzzing separately; no Minecraft binary required.
use legacy_protocol::{packet_id_and_body, read_frame, BufferedFrameReader, MAX_FRAME_BYTES};
use std::io::Cursor;

#[test]
fn malformed_frame_inputs_do_not_panic_or_bypass_frame_budget() {
    let mut seed = 0x1a7b_10f0_f00d_1234u64;
    for _ in 0..4_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let size = (seed as usize) % 2048;
        let mut input = vec![0u8; size];
        for byte in &mut input {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }
        let result = std::panic::catch_unwind(|| {
            let decoded = read_frame(&mut input.as_slice());
            if let Ok(payload) = decoded {
                assert!(!payload.is_empty() && payload.len() <= MAX_FRAME_BYTES);
                let _ = packet_id_and_body(&payload);
            }
            let mut buffered = BufferedFrameReader::new();
            let mut cursor = Cursor::new(&input);
            for _ in 0..4 {
                match buffered.poll(&mut cursor) {
                    Ok(Some(packet)) => assert!(packet.len() <= MAX_FRAME_BYTES),
                    Ok(None) | Err(_) => break,
                }
            }
        });
        assert!(
            result.is_ok(),
            "frame decoder panicked for deterministic seed {seed:x}"
        );
    }
}

#[test]
fn oversized_frame_prefix_is_rejected_without_allocating_the_claimed_size() {
    // 2^28 bytes is beyond the 1 MiB bound. A malicious length must be
    // rejected before allocating or reading the claimed body.
    let mut bytes = &[0x80, 0x80, 0x80, 0x08][..];
    assert!(read_frame(&mut bytes).is_err());
}
