#![no_main]
use legacy_protocol::{packet_id_and_body, read_frame, BufferedFrameReader};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|bytes: &[u8]| {
    // Input is capped to one legal frame plus header and trailing bytes.
    let slice = &bytes[..bytes.len().min(1_048_576 + 5)];
    if let Ok(body) = read_frame(&mut slice.as_ref()) {
        let _ = packet_id_and_body(&body);
    }
    let mut buffered = BufferedFrameReader::new();
    let mut reader = Cursor::new(slice);
    for _ in 0..8 {
        match buffered.poll(&mut reader) {
            Ok(Some(frame)) => {
                let _ = packet_id_and_body(&frame);
            }
            Ok(None) | Err(_) => break,
        }
    }
});
