//! Private disk-backed clipboard/paste bytes with bounded streaming readers.
use crate::document::Newline;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, Write},
    os::unix::fs::FileExt,
};

/// Immutable paste/clipboard after staging; dropping the last owner removes its file.
#[derive(Debug)]
pub(crate) struct Spool {
    file: File,
    pub len: u64,
}
impl Spool {
    /// Create a private, unlinked temporary file. No system clipboard access is implied.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            file: tempfile::tempfile()?,
            len: 0,
        })
    }
    /// Append a bounded chunk after checking available disk space and the reserve.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let space = nix::sys::statvfs::fstatvfs(&self.file)?;
        if (space.blocks_available() as u128) * (space.fragment_size() as u128)
            < (512 << 20) + bytes.len() as u128
        {
            return Err(io::Error::other("Insufficient temporary disk space"));
        }
        self.file.write_all(bytes)?;
        self.len = self
            .len
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("Paste length overflow"))?;
        Ok(())
    }
    /// Read without changing another user's file position (clipboard reads may overlap).
    pub fn reader(&self) -> impl Read + '_ {
        Reader {
            spool: self,
            offset: 0,
        }
    }

    /// Restore terminal-encoded line breaks as the document's preferred ending.
    pub fn terminal_paste_reader(&self, newline: Newline) -> impl Read + '_ {
        TerminalPasteReader {
            input: BufReader::new(self.reader()),
            newline,
            pending: b"",
        }
    }

    /// Feed one native helper from the immutable staging file.
    /// Other users of the spool use positional reads and keep their own cursors.
    pub fn input_file(&self) -> io::Result<File> {
        let mut file = self.file.try_clone()?;
        file.rewind()?;
        Ok(file)
    }
}
/// Independent forward-only file cursor; resident memory does not scale with the payload.
struct Reader<'a> {
    spool: &'a Spool,
    offset: u64,
}
impl Read for Reader<'_> {
    /// Use positional reads so an Arc-shared clipboard stays immutable.
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.spool.file.read_at(output, self.offset)?;
        self.offset += count as u64;
        Ok(count)
    }
}

/// Interpret bare CR as a line break and pass through LF and CRLF as received.
struct TerminalPasteReader<R> {
    input: R,
    newline: Newline,
    pending: &'static [u8],
}

impl<R: BufRead> Read for TerminalPasteReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < output.len() {
            if !self.pending.is_empty() {
                written += self.pending.read(&mut output[written..])?;
                continue;
            }
            let input = self.input.fill_buf()?;
            if input.is_empty() {
                break;
            }
            let count = input.len().min(output.len() - written);
            let plain = memchr::memchr(b'\r', &input[..count]).unwrap_or(count);
            output[written..written + plain].copy_from_slice(&input[..plain]);
            written += plain;
            self.input.consume(plain);
            if plain < count {
                self.input.consume(1);
                self.pending = if self.input.fill_buf()?.first() == Some(&b'\n') {
                    b"\r"
                } else {
                    self.newline.as_str().as_bytes()
                };
            }
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both staged input and transaction output can split a line ending.
    #[test]
    fn terminal_paste_handles_split_line_endings() {
        let payload = "\r\r\n界\n\rX\r";
        for (newline, expected) in [
            (Newline::Lf, "\n\r\n界\n\nX\n"),
            (Newline::CrLf, "\r\n\r\n界\n\r\nX\r\n"),
        ] {
            let mut reader = TerminalPasteReader {
                input: BufReader::with_capacity(1, payload.as_bytes()),
                newline,
                pending: b"",
            };
            let mut actual = Vec::new();
            let mut byte = [0];
            while reader.read(&mut byte).unwrap() != 0 {
                actual.push(byte[0]);
            }
            assert_eq!(actual, expected.as_bytes());
        }
    }
}
