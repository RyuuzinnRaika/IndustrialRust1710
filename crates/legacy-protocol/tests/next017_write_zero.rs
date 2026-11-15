//! NEXT-017 / #1000: 0-byte writes must terminate frame transmission
//! with WriteZero rather than repeatedly spinning on a broken transport.
use legacy_protocol::write_frame;
use std::io::{self, Write};

#[derive(Default)]
struct AlwaysZero {
    calls: usize,
}

impl Write for AlwaysZero {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ZeroAfter {
    successful_writes_left: usize,
    calls: usize,
}

impl Write for ZeroAfter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.successful_writes_left > 0 {
            self.successful_writes_left -= 1;
            Ok(bytes.len().min(1))
        } else {
            Ok(0)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn zero_first_write_is_reported_once_without_retry_loop() {
    let mut out = AlwaysZero::default();
    let err = write_frame(&mut out, &[0x01, 0x02]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(out.calls, 1, "a zero-byte write must not cause spin");
}

#[test]
fn zero_after_prefix_write_terminates_before_body_is_sent() {
    let mut out = ZeroAfter {
        successful_writes_left: 1,
        calls: 0,
    };
    let err = write_frame(&mut out, &[0x01, 0x02]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(out.calls, 2);
}

#[test]
fn partial_prefix_write_followed_by_zero_is_bounded() {
    // 128-byte payload produces a multi-byte length VarInt.
    let mut out = ZeroAfter {
        successful_writes_left: 1,
        calls: 0,
    };
    let err = write_frame(&mut out, &[0x01; 128]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(out.calls, 2);
}
