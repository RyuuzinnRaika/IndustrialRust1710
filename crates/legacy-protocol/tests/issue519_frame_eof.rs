//! Issue #519: distinguish an incomplete frame from EOF at a frame boundary.
//!
//! A truncated frame records an incomplete receive, not the peer's intent.
//! These checks use only synthetic in-memory readers; no TCP or world data.
use legacy_protocol::{is_truncated_frame_eof, BufferedFrameReader};
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read};

enum Step {
    Bytes(Vec<u8>),
    Error(ErrorKind),
    ReaderEof,
    Eof,
}

struct Script(VecDeque<Step>);

impl Script {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self(steps.into_iter().collect())
    }
}

impl Read for Script {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front().expect("unexpected extra read") {
            Step::Bytes(bytes) => {
                assert!(bytes.len() <= output.len());
                output[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Step::Error(kind) => Err(kind.into()),
            Step::ReaderEof => Err(io::Error::new(ErrorKind::UnexpectedEof, "reader sentinel")),
            Step::Eof => Ok(0),
        }
    }
}

fn check_eof(error: io::Error, truncated: bool) {
    // Preserve the public ErrorKind so existing peer-close callers remain
    // compatible. Incomplete EOF must carry a structured, non-payload marker
    // so a caller can distinguish it without parsing text. The diagnostic is
    // static and must not include packet data.
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(error.get_ref().is_some(), truncated);
    assert_eq!(is_truncated_frame_eof(&error), truncated);
    if truncated {
        assert_eq!(error.to_string(), "truncated frame");
    }
}

#[test]
fn empty_stream_eof_is_at_a_frame_boundary() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Eof]);
    check_eof(reader.poll(&mut input).unwrap_err(), false);
}

#[test]
fn partial_length_survives_poll_timeouts_and_is_reported_on_eof() {
    for kind in [
        ErrorKind::WouldBlock,
        ErrorKind::TimedOut,
        ErrorKind::Interrupted,
    ] {
        let mut reader = BufferedFrameReader::new();
        let mut input = Script::new([Step::Bytes(vec![0x80]), Step::Error(kind), Step::Eof]);
        assert!(reader.poll(&mut input).unwrap().is_none());
        assert!(reader.poll(&mut input).unwrap().is_none());
        check_eof(reader.poll(&mut input).unwrap_err(), true);
    }
}

#[test]
fn partial_body_is_reported_on_eof() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Bytes(vec![3, 0, 42]), Step::Eof]);
    assert!(reader.poll(&mut input).unwrap().is_none());
    check_eof(reader.poll(&mut input).unwrap_err(), true);
}

#[test]
fn complete_frame_then_partial_frame_in_one_read_is_reported() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Bytes(vec![1, 0, 0x80]), Step::Eof]);
    assert_eq!(reader.poll(&mut input).unwrap().unwrap(), vec![0]);
    check_eof(reader.poll(&mut input).unwrap_err(), true);
}

#[test]
fn complete_frame_then_eof_is_at_a_frame_boundary() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Bytes(vec![1, 0]), Step::Eof]);
    assert_eq!(reader.poll(&mut input).unwrap().unwrap(), vec![0]);
    check_eof(reader.poll(&mut input).unwrap_err(), false);
}

#[test]
fn direct_reader_error_keeps_its_original_kind_and_source() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Bytes(vec![0x80]), Step::ReaderEof]);
    assert!(reader.poll(&mut input).unwrap().is_none());
    let error = reader.poll(&mut input).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(error.to_string(), "reader sentinel");
    assert!(!is_truncated_frame_eof(&error));
}

#[test]
fn identical_message_without_marker_is_not_classified() {
    let error = io::Error::new(ErrorKind::UnexpectedEof, "truncated frame");
    assert!(!is_truncated_frame_eof(&error));
}

#[test]
fn wrapped_marker_is_not_mistaken_for_a_direct_poll_result() {
    let mut reader = BufferedFrameReader::new();
    let mut input = Script::new([Step::Bytes(vec![0x80]), Step::Eof]);
    assert!(reader.poll(&mut input).unwrap().is_none());
    let original = reader.poll(&mut input).unwrap_err();
    assert!(is_truncated_frame_eof(&original));
    let wrapped = io::Error::new(ErrorKind::UnexpectedEof, original);
    assert!(!is_truncated_frame_eof(&wrapped));
}
