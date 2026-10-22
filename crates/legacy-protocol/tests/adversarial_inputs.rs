//! Deterministic adversarial regression inputs for Protocol 5 parsers.
//! This is a bounded smoke corpus, not a substitute for cargo-fuzz coverage.
use legacy_protocol::{
    packet_id_and_body, read_frame, read_varint, BufferedFrameReader, Cursor, MAX_FRAME_BYTES,
};

fn next(seed: &mut u64) -> u8 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 24) as u8
}

#[test]
fn randomized_short_protocol_packets_do_not_panic() {
    let mut seed = 0x1710_0517_1234_5678u64;
    for len in 0..=256usize {
        for _ in 0..32 {
            let mut bytes = vec![0; len];
            for item in &mut bytes {
                *item = next(&mut seed);
            }
            let mut cursor = Cursor::new(&bytes);
            let _ = cursor.read_varint();
            let _ = cursor.read_string(255);
            let _ = cursor.read_i64();
            let _ = cursor.read_u16();
            let _ = packet_id_and_body(&bytes);
            let _ = read_varint(&mut bytes.as_slice());
            let _ = read_frame(&mut bytes.as_slice());
            let mut reader = BufferedFrameReader::new();
            let _ = reader.poll(&mut bytes.as_slice());
        }
    }
}

#[test]
fn oversized_frame_headers_are_rejected_without_allocating_payload() {
    // VarInt encoding of MAX_FRAME_BYTES + 1.
    let mut oversized = Vec::new();
    legacy_protocol::write_varint((MAX_FRAME_BYTES + 1) as u32, &mut oversized);
    let err = read_frame(&mut oversized.as_slice()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let mut reader = BufferedFrameReader::new();
    assert!(reader.poll(&mut oversized.as_slice()).is_err());
}
