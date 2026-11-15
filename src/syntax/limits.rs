//! Byte-based whole-document eligibility using immutable logical text.

use super::Fallback;
use crate::document::{ByteOffset, Cancellation, Snapshot};
use std::io::{self, Write};

/// Eligible length and line width include equality.
pub(crate) const DOCUMENT: u64 = 20 * 1024 * 1024;
const LINE: usize = 64 * 1024;
// Storage reserves twice the input plus two 64 KiB pages. This leaves room
// for running projection scratch and both published result copies under 1 MiB.
const WINDOW: usize = 48 * 1024;

/// Logical line check that preserves a pending CR across transfer windows.
#[derive(Default)]
struct Lines {
    len: usize,
    last_cr: bool,
}

impl Lines {
    /// Check complete lines and allow one provisional CR at the partial line end.
    fn push(&mut self, mut chunk: &[u8]) -> bool {
        while let Some(lf) = memchr::memchr(b'\n', chunk) {
            let cr = if lf == 0 {
                self.last_cr
            } else {
                chunk[lf - 1] == b'\r'
            };
            if self.len + lf - usize::from(cr) > LINE {
                return false;
            }
            self.len = 0;
            self.last_cr = false;
            chunk = &chunk[lf + 1..];
        }
        if let Some(last) = chunk.last() {
            self.last_cr = *last == b'\r';
        }
        self.len += chunk.len();
        self.len - usize::from(self.last_cr) <= LINE
    }

    /// A trailing CR at EOF counts as an ordinary byte.
    fn finish(&self) -> bool {
        self.len <= LINE
    }
}

/// Stream validated logical bytes. The child waits for the final dispatch message to parse.
pub(super) fn transfer(
    snapshot: &Snapshot,
    cancel: &Cancellation,
    writer: &mut impl Write,
) -> Result<(), Fallback> {
    if snapshot.len() > DOCUMENT {
        return Err(Fallback::Size);
    }
    let mut check = Lines::default();
    let mut at = ByteOffset(0);
    loop {
        let window = snapshot
            .read_window_with_cancel(at, WINDOW, cancel)
            .map_err(|error| match error {
                crate::document::Error::Cancelled => Fallback::Cancelled,
                crate::document::Error::ResourceLimit(_) => Fallback::Busy,
                _ => Fallback::Read,
            })?;
        let bytes = window.text().as_bytes();
        if !check.push(bytes) {
            return Err(Fallback::Line);
        }
        writer.write_all(bytes).map_err(|error| {
            if error.kind() == io::ErrorKind::Interrupted || cancel.is_cancelled() {
                Fallback::Cancelled
            } else {
                Fallback::Worker
            }
        })?;
        let Some(next) = window.continuation() else {
            break;
        };
        at = next;
    }
    check.finish().then_some(()).ok_or(Fallback::Line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_limits_count_bytes_and_only_crlf_excludes_cr() {
        for size in [LINE - 1, LINE, LINE + 1] {
            let bytes = "é".repeat(size / 2) + &"x".repeat(size % 2);
            let mut lines = Lines::default();
            let accepted = lines.push(bytes.as_bytes()) && lines.finish();
            assert_eq!(accepted, size <= LINE);
        }
        let at_limit = vec![b'a'; LINE];
        let mut lines = Lines::default();
        assert!(lines.push(&at_limit));
        assert!(lines.push(b"\r"));
        assert!(!lines.finish(), "standalone CR at EOF counts");
        assert!(lines.push(b"\nnext\r\n"));
        assert!(lines.finish(), "CRLF may straddle a window");
        let mut lines = Lines::default();
        assert!(lines.push(b"short\r"));
        assert!(lines.push(b"\n"));
        assert!(lines.push(&at_limit));
        assert!(lines.push(b"\r"));
        assert!(!lines.push(b"x"), "nonterminal CR counts");
    }
}
