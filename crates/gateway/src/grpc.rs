//! Minimal gRPC length-prefixed message framing.
//!
//! A gRPC body is a sequence of `[flag:u8][len:u32 BE][payload]` records
//! that HTTP/2 is free to split across DATA frames however it likes. The
//! parser buffers until a whole record is present and hands back payloads.

use bytes::{Buf, Bytes, BytesMut};

/// Records larger than this are reported as an error instead of buffered.
/// Zaino and lightwalletd both cap messages well below this.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Record {
    Message(Bytes),
    /// The compressed flag was set. We never negotiate compression, so this
    /// should not happen; it is surfaced rather than decoded.
    Compressed(usize),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    TooLarge(usize),
    BadFlag(u8),
}

#[derive(Default)]
pub struct Deframer {
    buf: BytesMut,
}

impl Deframer {
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pops the next complete record, if one is buffered.
    pub fn next_record(&mut self) -> Result<Option<Record>, FrameError> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let flag = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_MESSAGE {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        self.buf.advance(5);
        let payload = self.buf.split_to(len).freeze();
        match flag {
            0 => Ok(Some(Record::Message(payload))),
            1 => Ok(Some(Record::Compressed(len))),
            other => Err(FrameError::BadFlag(other)),
        }
    }

    /// Bytes received that do not yet form a complete record.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Encodes one uncompressed gRPC record.
pub fn encode(payload: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(5 + payload.len());
    out.extend_from_slice(&[0]);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassembles_records_split_across_chunks() {
        let a = encode(b"first");
        let b = encode(b"");
        let c = encode(&[7u8; 300]);
        let mut wire = Vec::new();
        wire.extend_from_slice(&a);
        wire.extend_from_slice(&b);
        wire.extend_from_slice(&c);

        // Feed one byte at a time; the worst case HTTP/2 is allowed to give us.
        let mut d = Deframer::default();
        let mut got = Vec::new();
        for byte in &wire {
            d.push(std::slice::from_ref(byte));
            while let Some(r) = d.next_record().unwrap() {
                got.push(r);
            }
        }
        assert_eq!(
            got,
            vec![
                Record::Message(Bytes::from_static(b"first")),
                Record::Message(Bytes::new()),
                Record::Message(Bytes::from(vec![7u8; 300])),
            ]
        );
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn rejects_oversized_length_prefix() {
        let mut d = Deframer::default();
        d.push(&[0, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(
            d.next_record(),
            Err(FrameError::TooLarge(u32::MAX as usize))
        );
    }

    #[test]
    fn rejects_unknown_flag() {
        let mut d = Deframer::default();
        d.push(&[2, 0, 0, 0, 0]);
        assert_eq!(d.next_record(), Err(FrameError::BadFlag(2)));
    }
}
