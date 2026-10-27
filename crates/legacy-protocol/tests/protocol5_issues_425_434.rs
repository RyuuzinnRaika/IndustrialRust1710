//! ADD-001/002/003/004/006/010: byte-level Protocol 5 security regressions.
//! Independent fixtures exercise the same public decoder as TCP sessions.
use legacy_protocol::{
    packet_id_and_body, read_frame, read_varint, write_frame, write_string, write_varint,
    BufferedFrameReader, Cursor, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use std::io::{self, Read};

fn rejected_frame(header: &[u8]) {
    assert_eq!(
        read_frame(&mut header.as_ref()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut reader = BufferedFrameReader::new();
    assert_eq!(
        reader.poll(&mut header.as_ref()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn add001_six_byte_varint_and_invalid_fifth_byte_are_rejected() {
    for header in [
        &[0x80, 0x80, 0x80, 0x80, 0x80, 0][..],
        &[0xff, 0xff, 0xff, 0xff, 0x8f, 0][..],
        &[0x80, 0x80, 0x80, 0x80, 0x10][..],
    ] {
        assert_eq!(
            read_varint(&mut header.as_ref()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        rejected_frame(header);
    }
    // Five bytes are legal for an unsigned primitive, but not for a frame
    // exceeding MAX_FRAME_BYTES.
    assert_eq!(
        read_varint(&mut &[0xff, 0xff, 0xff, 0xff, 0x0f][..]).unwrap(),
        u32::MAX
    );
    rejected_frame(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
}

#[test]
fn add002_zero_negative_and_oversize_frame_lengths_are_bounded() {
    rejected_frame(&[0]); // zero-length packets cannot contain a packet ID
    rejected_frame(&[0xff, 0xff, 0xff, 0xff, 0x0f]); // signed -1 on wire
    let mut header = Vec::new();
    write_varint((MAX_FRAME_BYTES + 1) as u32, &mut header);
    rejected_frame(&header);

    // The largest permitted payload is accepted without a second allocation
    // proportional to any attacker-supplied signed length.
    let body = vec![0x2a; MAX_FRAME_BYTES];
    let mut wire = Vec::new();
    write_frame(&mut wire, &body).unwrap();
    assert_eq!(read_frame(&mut wire.as_slice()).unwrap(), body);
    assert!(write_frame(&mut Vec::new(), &[]).is_err());
}

#[test]
fn add003_truncated_or_invalid_utf8_is_never_replaced_silently() {
    for bytes in [
        &[2, 0xc3, 0x28][..], // invalid continuation byte
        &[2, 0xc0, 0xaf][..], // overlong slash
        &[3, 0xe3, 0x81][..], // truncated multibyte codepoint
        &[4, b'a', b'b'][..], // truncated payload
    ] {
        assert!(Cursor::new(bytes).read_string(16).is_err());
    }
    let valid = [3, 0xe3, 0x81, 0x82]; // hiragana 'あ'
    let mut cursor = Cursor::new(&valid);
    assert_eq!(cursor.read_string(16).unwrap(), "あ");
    cursor.finish().unwrap();
}

#[test]
fn add004_handshake_and_login_length_boundaries() {
    let mut packet = Vec::new();
    write_varint(PROTOCOL_VERSION as u32, &mut packet);
    write_string(&"h".repeat(255), &mut packet).unwrap();
    packet.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(2, &mut packet);
    let mut cursor = Cursor::new(&packet);
    assert_eq!(cursor.read_varint().unwrap(), PROTOCOL_VERSION);
    assert_eq!(cursor.read_string(255).unwrap().len(), 255);
    assert_eq!(cursor.read_u16().unwrap(), 25565);
    assert_eq!(cursor.read_varint().unwrap(), 2);
    cursor.finish().unwrap();

    for (limit, accepted, rejected) in [(255, 255, 256), (16, 16, 17)] {
        let mut ok = Vec::new();
        write_string(&"A".repeat(accepted), &mut ok).unwrap();
        assert_eq!(Cursor::new(&ok).read_string(limit).unwrap().len(), accepted);
        let mut bad = Vec::new();
        write_string(&"A".repeat(rejected), &mut bad).unwrap();
        assert!(Cursor::new(&bad).read_string(limit).is_err());
    }
}

#[test]
fn add006_protocol5_uses_an_uncompressed_packet_id_directly_after_length() {
    // Minecraft 1.7.10 (protocol 5) has no modern Set Compression packet,
    // nor the extra "uncompressed data length" field within every frame.
    let mut handshake = vec![0x00, 0x05, 0x01, b'a', 0x63, 0xdd, 0x02];
    let mut wire = Vec::new();
    write_frame(&mut wire, &handshake).unwrap();
    assert_eq!(wire, [7, 0, 5, 1, b'a', 0x63, 0xdd, 2]);
    let decoded = read_frame(&mut wire.as_slice()).unwrap();
    let (id, mut body) = packet_id_and_body(&decoded).unwrap();
    assert_eq!(id, 0);
    assert_eq!(body.read_varint().unwrap(), PROTOCOL_VERSION);
    assert_eq!(body.read_string(255).unwrap(), "a");
    body.read_u16().unwrap();
    assert_eq!(body.read_varint().unwrap(), 2);
    body.finish().unwrap();

    // If modern framing is injected, its "data length" is interpreted as a
    // Protocol 5 packet ID, not silently decompressed.
    handshake.insert(0, 3);
    let mut modern = Vec::new();
    write_frame(&mut modern, &handshake).unwrap();
    let modern_decoded = read_frame(&mut modern.as_slice()).unwrap();
    let (packet_id, _) = packet_id_and_body(&modern_decoded).unwrap();
    assert_eq!(packet_id, 3);
}

struct ChunkReader<'a> {
    bytes: &'a [u8],
    chunk: usize,
}
impl Read for ChunkReader<'_> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.bytes.is_empty() {
            return Ok(0);
        }
        let size = into.len().min(self.bytes.len()).min(self.chunk);
        into[..size].copy_from_slice(&self.bytes[..size]);
        self.bytes = &self.bytes[size..];
        Ok(size)
    }
}

#[test]
fn add010_bytewise_and_coalesced_packets_restore_identical_order() {
    let payloads = [
        vec![0x00, 0x05],
        vec![0x09, 0x02],
        vec![0x01, 0x03, b'a', b'b'],
    ];
    let mut wire = Vec::new();
    for payload in &payloads {
        write_frame(&mut wire, payload).unwrap();
    }
    for chunk in [1, 2, 3, 7, wire.len()] {
        let mut socket = ChunkReader {
            bytes: &wire,
            chunk,
        };
        let mut parser = BufferedFrameReader::new();
        let mut decoded = Vec::new();
        while decoded.len() < payloads.len() {
            if let Some(frame) = parser.poll(&mut socket).unwrap() {
                decoded.push(frame);
            }
        }
        assert_eq!(decoded, payloads, "read chunk size {chunk}");
    }
}
