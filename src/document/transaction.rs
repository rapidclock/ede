//! Incremental disk-spooled batches with sorted, non-overlapping logical ranges.
use super::{
    Cancellation, DocumentId, Error, Newline, Revision, TextRange,
    index::Validator,
    platform,
    resources::{Permit, Resources},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};

/// Disk-spooled transaction; descriptors and insertion bytes never grow resident with batch size.
pub struct StagedTransaction {
    pub(super) _permit: Permit,
    pub(super) file: File,
    pub(super) document: DocumentId,
    pub(super) expected: Revision,
    /// First LF or CRLF in the same revision the staged edit will target.
    pub(crate) first_line_ending: Option<Newline>,
    pub(super) count: u64,
    pub(super) last_end: u64,
    pub(super) length: u64,
    pub(super) max_buffer: usize,
    pub(super) reserve: u64,
    pub(super) resources: Arc<Resources>,
}
impl StagedTransaction {
    /// Append a sorted non-overlapping replacement, reading insertion bytes incrementally.
    ///
    /// Invalid UTF-8/NUL, overlap, read errors or cancellation discard this record;
    /// previous records remain usable. No document content is changed until apply.
    pub fn replace(
        &mut self,
        range: TextRange,
        text: impl Read,
        cancel: &Cancellation,
    ) -> Result<(), Error> {
        self.replace_counted(range, text, cancel).map(|_| ())
    }

    /// Stage a replacement and report its actual byte count for the editor caret.
    pub(crate) fn replace_counted(
        &mut self,
        range: TextRange,
        mut text: impl Read,
        cancel: &Cancellation,
    ) -> Result<u64, Error> {
        if self.count != 0 && range.start < self.last_end {
            return Err(Error::InvalidRange);
        }
        let _permit = self.resources.buffers.acquire(self.max_buffer * 3 + 256)?;
        let start = self.length;
        let result = (|| {
            self.file.seek(SeekFrom::Start(start))?;
            let mut validator = Validator::new();
            let mut buffer = vec![0; self.max_buffer];
            let mut written: u64 = 0;
            let mut dirty_bytes = 0;
            loop {
                cancel.check()?;
                let count = text.read(&mut buffer)?;
                // A reader may yield short first chunks; collect at least four bytes for BOM recognition.
                let mut count = count;
                if written == 0 {
                    while count < 4 {
                        let n = text.read(&mut buffer[count..4])?;
                        if n == 0 {
                            break;
                        }
                        count += n;
                    }
                }
                validator.feed(&buffer[..count], count == 0)?;
                if count == 0 {
                    break;
                }
                // Insertions are logical bytes; a BOM here is text, never removed.
                platform::reserve(&self.file, count as u64 + 32, self.reserve)?;
                self.file.write_all(&buffer[..count])?;
                dirty_bytes += count;
                if dirty_bytes >= 8 << 20 {
                    self.file.sync_data()?;
                    dirty_bytes = 0;
                }
                written = written
                    .checked_add(count as u64)
                    .ok_or(Error::ResourceLimit("transaction length overflow"))?;
            }
            for value in [range.start, range.end, written, start] {
                self.file.write_all(&value.to_le_bytes())?;
            }
            Ok::<u64, Error>(written)
        })();
        match result {
            Ok(written) => {
                self.length = start
                    .checked_add(written)
                    .and_then(|n| n.checked_add(32))
                    .ok_or(Error::ResourceLimit("transaction offset overflow"))?;
                self.last_end = range.end;
                self.count += 1;
                Ok(written)
            }
            Err(error) => {
                self.file.set_len(start)?;
                Err(error)
            }
        }
    }
}
