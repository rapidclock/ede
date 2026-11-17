//! Incremental source decoding with bounded BOM, scalar, and CR lookahead.

use std::ops::Range;

/// Place a read at this offset to leave space for carried bytes and a generation seam.
pub(super) const PREFIX: usize = 8;
pub(super) const BLOCK: usize = 64 << 10;

/// Information about one validated span of the reusable input block.
pub(super) struct Decoded {
    pub bytes: Range<usize>,
    pub error: Option<String>,
}

/// Decoder state belongs to one source generation, independent of retained offsets.
#[derive(Default)]
pub(super) struct Decoder {
    carry: [u8; 4],
    pending: usize,
    offset: u64,
    classified: bool,
    pub bom: bool,
}

impl Decoder {
    /// Validate input in source order, leaving incomplete units in a fixed carry.
    pub fn decode(&mut self, block: &mut [u8], count: usize, eof: bool) -> Decoded {
        let start = PREFIX - self.pending;
        block[start..PREFIX].copy_from_slice(&self.carry[..self.pending]);
        let end = PREFIX + count;
        let base = self.offset - self.pending as u64;
        self.offset += count as u64;
        self.pending = 0;
        let mut text = start;
        if !self.classified {
            let data = &block[start..end];
            if !eof
                && [
                    b"\xef\xbb\xbf".as_slice(),
                    b"\xff\xfe",
                    b"\xfe\xff",
                    b"\0\0\xfe\xff",
                ]
                .iter()
                .any(|bom| data.len() < bom.len() && bom.starts_with(data))
            {
                self.remember(data);
                return Decoded {
                    bytes: end..end,
                    error: None,
                };
            }
            self.classified = true;
            if data.starts_with(b"\xff\xfe")
                || data.starts_with(b"\xfe\xff")
                || data.starts_with(b"\0\0\xfe\xff")
            {
                return Decoded {
                    bytes: end..end,
                    error: Some("Unsupported text encoding at byte 0".into()),
                };
            }
            self.bom = data.starts_with(b"\xef\xbb\xbf");
            if self.bom {
                text += 3;
            }
        }
        let data = &block[text..end];
        let utf8 = std::str::from_utf8(data);
        let valid = utf8
            .as_ref()
            .map_or_else(|e| e.valid_up_to(), |_| data.len());
        let nul = memchr::memchr(0, &data[..valid]);
        let mut complete = text + nul.unwrap_or(valid);
        let error_offset = base + (complete - start) as u64;
        let error = if nul.is_some() {
            Some(format!("NUL at byte {error_offset}"))
        } else if let Err(error) = utf8 {
            if error.error_len().is_some() || eof {
                Some(format!(
                    "Invalid or incomplete UTF-8 at byte {error_offset}"
                ))
            } else {
                None
            }
        } else {
            None
        };
        if !eof && error.is_none() {
            if complete == end && complete > text && block[complete - 1] == b'\r' {
                complete -= 1;
            }
            self.remember(&block[complete..end]);
        }
        Decoded {
            bytes: text..complete,
            error,
        }
    }

    /// A valid UTF-8 partial scalar and a pending CR are mutually exclusive.
    fn remember(&mut self, bytes: &[u8]) {
        self.carry[..bytes.len()].copy_from_slice(bytes);
        self.pending = bytes.len();
    }

    /// At the exact retention cap only a byte of lookahead is needed once classified.
    pub fn lookahead(&self) -> usize {
        if !self.classified { 4 } else { 1 }
    }

    /// Once classified, a carried byte proves that additional text exists.
    pub fn has_partial_text(&self) -> bool {
        self.classified && self.pending > 0
    }
}

/// Choose the longest prefix that does not split a scalar or a CRLF pair.
pub(super) fn within(bytes: &[u8], capacity: usize) -> usize {
    let mut take = capacity.min(bytes.len());
    while take < bytes.len() && bytes[take] & 0xc0 == 0x80 {
        take -= 1;
    }
    if take > 0 && bytes.get(take - 1..=take) == Some(b"\r\n") {
        take -= 1;
    }
    take
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_bom_scalar_and_crlf_have_the_same_valid_prefix() {
        let source = "\u{feff}lead\r\n🌍\r".as_bytes();
        for step in 1..=source.len() {
            let mut decoder = Decoder::default();
            let mut block = [0; 64];
            let mut retained = Vec::new();
            for bytes in source.chunks(step).chain([b"".as_slice()]) {
                block[PREFIX..PREFIX + bytes.len()].copy_from_slice(bytes);
                let decoded = decoder.decode(&mut block, bytes.len(), bytes.is_empty());
                assert!(decoded.error.is_none());
                retained.extend_from_slice(&block[decoded.bytes]);
            }
            assert!(decoder.bom);
            assert_eq!(retained, source[3..]);
        }
        assert_eq!(within("a\r\n🌍".as_bytes(), 2), 1);
        assert_eq!(within("a\r\n🌍".as_bytes(), 5), 3);
    }
}
