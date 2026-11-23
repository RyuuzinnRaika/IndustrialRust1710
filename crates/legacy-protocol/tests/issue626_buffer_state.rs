//! A read-only view of the first userspace frame; no socket or payload logging.
use legacy_protocol::{BufferedFrameReader, BufferedFrameState, MAX_FRAME_BYTES};
use std::io::{self, ErrorKind, Read};

struct Once(Option<Vec<u8>>);
impl Read for Once {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self.0.take() {
            Some(bytes) => {
                out[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            None => Err(ErrorKind::WouldBlock.into()),
        }
    }
}

fn buffered_after_first(rest: &[u8]) -> (BufferedFrameReader, Once) {
    let mut bytes = vec![1, 0x33];
    bytes.extend_from_slice(rest);
    let mut source = Once(Some(bytes));
    let mut reader = BufferedFrameReader::new();
    assert_eq!(reader.poll(&mut source).unwrap(), Some(vec![0x33]));
    (reader, source)
}

#[test]
fn empty_partial_and_complete_states_do_not_consume_input() {
    let (mut empty, mut src) = buffered_after_first(&[]);
    assert_eq!(empty.buffered_frame_state(), BufferedFrameState::Empty);
    assert_eq!(empty.buffered_frame_state(), BufferedFrameState::Empty);
    assert_eq!(empty.poll(&mut src).unwrap(), None);

    for partial in [&[0x80][..], &[0x80, 0x80, 0x80, 0x80][..], &[3, 0, 42][..]] {
        let (mut reader, mut src) = buffered_after_first(partial);
        assert_eq!(
            reader.buffered_frame_state(),
            BufferedFrameState::Incomplete
        );
        assert_eq!(
            reader.buffered_frame_state(),
            BufferedFrameState::Incomplete
        );
        assert_eq!(reader.poll(&mut src).unwrap(), None);
    }
    let (mut reader, mut src) = buffered_after_first(&[1, 0x55, 1, 0x77]);
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Complete);
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Complete);
    assert_eq!(reader.poll(&mut src).unwrap(), Some(vec![0x55]));
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Complete);
    assert_eq!(reader.poll(&mut src).unwrap(), Some(vec![0x77]));
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Empty);
}

#[test]
fn invalid_lengths_are_classified_without_consuming_or_changing_parser_error() {
    for bytes in [
        &[0][..],
        &[0x80, 0x80, 0x41][..],
        &[0x80, 0x80, 0x80, 0x80, 0x10][..],
        &[0x80, 0x80, 0x80, 0x80, 0x80][..],
    ] {
        let (mut reader, mut src) = buffered_after_first(bytes);
        assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Invalid);
        assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Invalid);
        assert_eq!(
            reader.poll(&mut src).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Invalid);
    }
    let mut over_limit = Vec::new();
    legacy_protocol::write_varint(MAX_FRAME_BYTES as u32 + 1, &mut over_limit);
    let (mut reader, mut src) = buffered_after_first(&over_limit);
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Invalid);
    let error = reader.poll(&mut src).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "invalid frame size");
    assert!(error.get_ref().is_some());
    let (mut reader, mut src) = buffered_after_first(&[0x80, 0x80, 0x80, 0x80, 0x10]);
    let error = reader.poll(&mut src).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "invalid fifth VarInt byte");
    assert!(error.get_ref().is_some());
}

#[test]
fn partial_body_remains_intact_until_a_later_read_completes_it() {
    let (mut reader, mut src) = buffered_after_first(&[3, 0x11]);
    assert_eq!(
        reader.buffered_frame_state(),
        BufferedFrameState::Incomplete
    );
    assert_eq!(reader.poll(&mut src).unwrap(), None);
    src.0 = Some(vec![0x22, 0x33]);
    assert_eq!(reader.poll(&mut src).unwrap(), Some(vec![0x11, 0x22, 0x33]));
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Empty);
}

#[test]
fn maximum_length_prefix_is_valid_but_incomplete_without_its_body() {
    let mut prefix = Vec::new();
    legacy_protocol::write_varint(MAX_FRAME_BYTES as u32, &mut prefix);
    let (mut reader, mut src) = buffered_after_first(&prefix);
    assert_eq!(
        reader.buffered_frame_state(),
        BufferedFrameState::Incomplete
    );
    assert_eq!(reader.poll(&mut src).unwrap(), None);
}

#[test]
fn noncanonical_but_accepted_length_and_following_invalid_frame_keep_order() {
    let (mut reader, mut src) = buffered_after_first(&[0x81, 0, 0x55, 0]);
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Complete);
    assert_eq!(reader.poll(&mut src).unwrap(), Some(vec![0x55]));
    assert_eq!(reader.buffered_frame_state(), BufferedFrameState::Invalid);
    assert_eq!(
        reader.poll(&mut src).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}
