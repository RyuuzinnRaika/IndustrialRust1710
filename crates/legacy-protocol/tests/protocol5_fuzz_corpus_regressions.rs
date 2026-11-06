//! Deterministic Protocol 5 fuzz-seed corpus and regression checks (#510).
//! These are minimized, hand-authored baseline seeds; no libFuzzer crash
//! discovery or completed fuzz campaign is claimed by this file.
use legacy_protocol::{
    packet_id_and_body, read_frame, write_frame, BufferedFrameReader,
};
use std::io::{self, Read};

#[test]
fn minimized_invalid_frame_corpus_fails_closed_in_both_decoders() {
    let invalid: &[(&str, &[u8])] = &[
        ("zero_length", include_bytes!("../../../fuzz/corpus/protocol5_frames/zero_length")),
        ("excess_varint", include_bytes!("../../../fuzz/corpus/protocol5_frames/excess_varint")),
        (
            "invalid_fifth_byte",
            include_bytes!("../../../fuzz/corpus/protocol5_frames/invalid_fifth_byte"),
        ),
        (
            "oversize_frame_header",
            include_bytes!("../../../fuzz/corpus/protocol5_frames/oversize_frame_header"),
        ),
        (
            "truncated_frame",
            include_bytes!("../../../fuzz/corpus/protocol5_frames/truncated_frame"),
        ),
    ];
    for &(label, bytes) in invalid {
        let mut direct = bytes;
        assert!(read_frame(&mut direct).is_err(), "{label}: direct accepted invalid input");

        let mut buffered = BufferedFrameReader::new();
        let mut stream = bytes;
        match buffered.poll(&mut stream) {
            Err(_) => {}
            Ok(None) => assert!(
                buffered.poll(&mut stream).is_err(),
                "{label}: EOF should not be a partial-frame success"
            ),
            Ok(Some(_)) => panic!("{label}: buffered accepted invalid input"),
        }
    }
}

#[test]
fn corpus_preserves_valid_1710_frames_and_rejects_bad_utf8() {
    for bytes in [
        include_bytes!("../../../fuzz/corpus/protocol5_frames/valid_status_request").as_slice(),
        include_bytes!("../../../fuzz/corpus/protocol5_frames/valid_login_handshake").as_slice(),
    ] {
        let mut stream = bytes;
        let direct = read_frame(&mut stream).unwrap();
        let (packet, mut payload) = packet_id_and_body(&direct).unwrap();
        assert_eq!(packet, 0);
        if bytes.len() > 2 {
            assert_eq!(payload.read_varint().unwrap(), 5);
            assert_eq!(payload.read_string(255).unwrap(), "a");
            assert_eq!(payload.read_u16().unwrap(), 25565);
            assert_eq!(payload.read_varint().unwrap(), 2);
            payload.finish().unwrap();
        } else {
            payload.finish().unwrap();
        }
        let mut buffered = BufferedFrameReader::new();
        let mut input = bytes;
        assert_eq!(buffered.poll(&mut input).unwrap(), Some(direct));
    }

    let mut wire = include_bytes!("../../../fuzz/corpus/protocol5_frames/invalid_utf8_string")
        .as_slice();
    let frame = read_frame(&mut wire).unwrap();
    let (id, mut cursor) = packet_id_and_body(&frame).unwrap();
    assert_eq!(id, 0);
    assert_eq!(
        cursor.read_string(16).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn deterministic_fragmented_frame_sweep_has_no_decode_drift() {
    struct Chunks<'a> {
        source: &'a [u8],
        chunk_size: usize,
    }
    impl Read for Chunks<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let n = output.len().min(self.source.len()).min(self.chunk_size);
            output[..n].copy_from_slice(&self.source[..n]);
            self.source = &self.source[n..];
            Ok(n)
        }
    }
    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
    for seed in 1u64..=512 {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let length = (next(&mut state) % 1024 + 1) as usize;
        let mut body = vec![0u8; length];
        for byte in &mut body {
            *byte = next(&mut state) as u8;
        }
        let mut wire = Vec::new();
        write_frame(&mut wire, &body).unwrap();

        let mut direct = wire.as_slice();
        assert_eq!(read_frame(&mut direct).unwrap(), body, "seed={seed}");
        let mut reader = BufferedFrameReader::new();
        let mut chunked = Chunks {
            source: &wire,
            chunk_size: (seed % 17 + 1) as usize,
        };
        let mut result = None;
        for _ in 0..wire.len() + 1 {
            match reader.poll(&mut chunked).unwrap() {
                Some(decoded) => {
                    result = Some(decoded);
                    break;
                }
                None => continue,
            }
        }
        assert_eq!(result, Some(body), "fragmented frame seed={seed}");
    }
}
