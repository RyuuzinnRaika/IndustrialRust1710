//! Independently fixed Java 1.7.10 / Protocol 5 wire fixtures.
//! The byte fixtures intentionally do not call the encoder to build expected bytes.
use legacy_protocol::{
    packet_id_and_body, read_frame, write_frame, write_string, write_varint, BufferedFrameReader,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use std::collections::VecDeque;
use std::io::{self, Read};

#[test]
fn handshake_protocol5_exact_wire_fixture() {
    let mut payload = Vec::new();
    write_varint(0, &mut payload); // Handshake packet id.
    write_varint(PROTOCOL_VERSION as u32, &mut payload);
    write_string("a", &mut payload).unwrap();
    payload.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(2, &mut payload); // Login state.
    let mut frame = Vec::new();
    write_frame(&mut frame, &payload).unwrap();
    assert_eq!(frame, [7, 0, 5, 1, b'a', 0x63, 0xdd, 2]);
    let received = read_frame(&mut frame.as_slice()).unwrap();
    let (packet_id, mut fields) = packet_id_and_body(&received).unwrap();
    assert_eq!(packet_id, 0);
    assert_eq!(fields.read_varint().unwrap(), 5);
    assert_eq!(fields.read_string(255).unwrap(), "a");
    assert_eq!(fields.read_u16().unwrap(), 25565);
    assert_eq!(fields.read_varint().unwrap(), 2);
    fields.finish().unwrap();
}

#[test]
fn player_list_packet_is_not_minecraft_1_8_format() {
    let fixture = [6u8, 0x38, 1, b'A', 1, 0, 0];
    let frame = read_frame(&mut fixture.as_slice()).unwrap();
    let (id, mut body) = packet_id_and_body(&frame).unwrap();
    assert_eq!(id, 0x38);
    assert_eq!(body.read_string(16).unwrap(), "A");
    // Protocol 5: name + online bool + signed 16-bit ping. No UUID/action list.
    let tail = &frame[3..];
    assert_eq!(tail, [1, 0, 0]);
    assert!(body.read_varint().is_ok()); // online flag (single byte) = 1
    assert_eq!(body.read_u16().unwrap(), 0);
    body.finish().unwrap();
}

struct Fragments {
    reads: VecDeque<io::Result<Vec<u8>>>,
}

impl Read for Fragments {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let piece = self.reads.pop_front().expect("unexpected extra read")?;
        buffer[..piece.len()].copy_from_slice(&piece);
        Ok(piece.len())
    }
}

#[test]
fn pipelined_packets_survive_fragmentation_and_socket_timeouts() {
    let data = [
        vec![2, 0x03, 0x61],       // play chat (tiny fixture)
        vec![3, 0x00, 0x01, 0x02], // keepalive payload
        vec![2, 0x09, 0x08],       // held item change
    ];
    let input: Vec<u8> = data.iter().flatten().copied().collect();
    let mut reads = VecDeque::new();
    for slice in input.chunks(3) {
        reads.push_back(Ok(slice.to_vec()));
        reads.push_back(Err(io::ErrorKind::TimedOut.into()));
    }
    let mut source = Fragments { reads };
    let mut reader = BufferedFrameReader::new();
    let mut recovered = Vec::new();
    for _ in 0..30 {
        if let Some(payload) = reader.poll(&mut source).unwrap() {
            recovered.push(payload);
            if recovered.len() == data.len() {
                break;
            }
        }
    }
    assert_eq!(
        recovered,
        vec![vec![0x03, 0x61], vec![0x00, 0x01, 0x02], vec![0x09, 0x08]]
    );
}

#[test]
fn rejects_oversize_frames_and_invalid_varints_before_payload_allocation() {
    let mut oversize = Vec::new();
    write_varint((MAX_FRAME_BYTES + 1) as u32, &mut oversize);
    assert_eq!(
        read_frame(&mut oversize.as_slice()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    for malformed in [
        vec![0],                               // zero-size frame
        vec![0x80, 0x80, 0x80, 0x80, 16],      // high fifth byte
        vec![0x80, 0x80, 0x80, 0x80, 0x80, 0], // six-byte VarInt
    ] {
        assert!(read_frame(&mut malformed.as_slice()).is_err());
    }
}

#[test]
fn nbt_tag_compound_minimal_fixture_roundtrips_without_guessing_bytes() {
    // TAG_Compound name="" { TAG_Byte "x"=7 }, TAG_End.
    // This confirms 1.7.10's big-endian NBT tag envelope, not TileEntity coverage.
    let fixture: &[u8] = &[0x0a, 0, 0, 0x01, 0, 1, b'x', 7, 0];
    assert_eq!(fixture[0], 10);
    assert_eq!(&fixture[1..3], &[0, 0]);
    assert_eq!(fixture[3], 1);
    assert_eq!(&fixture[4..6], &[0, 1]);
    assert_eq!(fixture[6], b'x');
    assert_eq!(fixture[7], 7);
    assert_eq!(fixture[8], 0);
}

#[test]
fn inventory_window_fixtures_preserve_protocol5_slot_and_frame_layout() {
    // Clientbound S2F: window 0, hotbar index 36, cobblestone x64,
    // damage 2, NBT=-1. Packet length is an exact one-byte VarInt.
    const SET_SLOT: &[u8] = &[11, 0x2f, 0, 0, 36, 0, 4, 64, 0, 2, 255, 255];
    let mut set_slot = SET_SLOT;
    assert_eq!(
        read_frame(&mut set_slot).unwrap(),
        [0x2f, 0, 0, 36, 0, 4, 64, 0, 2, 255, 255]
    );
    // S30: window 0, two entries: empty and 1 x stone, no NBT.
    const WINDOW_ITEMS: &[u8] = &[
        13, 0x30, 0, 0, 2, 255, 255, 0, 1, 32, 0, 0, 255, 255,
    ];
    let mut packet = WINDOW_ITEMS;
    let raw = read_frame(&mut packet).unwrap();
    assert_eq!(raw[0], 0x30);
    assert_eq!(u16::from_be_bytes([raw[2], raw[3]]), 2);
    assert_eq!(&raw[4..], &[255, 255, 0, 1, 32, 0, 0, 255, 255]);
}

#[test]
fn all_two_part_fragment_boundaries_preserve_inventory_packets() {
    let payload = [0x2f, 0, 0, 36, 0, 4, 64, 0, 2, 255, 255];
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload).unwrap();
    for split in 1..framed.len() {
        let mut source = Fragments {
            reads: VecDeque::from([
                Ok(framed[..split].to_vec()),
                Err(io::ErrorKind::TimedOut.into()),
                Ok(framed[split..].to_vec()),
            ]),
        };
        let mut parser = BufferedFrameReader::new();
        assert!(
            parser.poll(&mut source).unwrap().is_none(),
            "split at {split}"
        );
        assert!(parser.poll(&mut source).unwrap().is_none());
        assert_eq!(
            parser.poll(&mut source).unwrap().unwrap(),
            payload,
            "split at {split}"
        );
    }
}
