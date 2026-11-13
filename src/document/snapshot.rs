//! Bounded immutable revision leases and scalar-aligned owned text windows.
use super::{
    ByteOffset, Cancellation, DocumentId, Error, Format, Revision,
    history::State,
    resources::Permit,
    source::{Reader, Store},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// One immutable snapshot lease. Dropping it releases its bounded lease slot.
pub struct Snapshot {
    pub(super) store: Arc<Mutex<Store>>,
    pub(super) state: Box<State>,
    pub(super) reader: Box<Reader>,
    pub(super) document: DocumentId,
    pub(super) revision: Revision,
    pub(super) leases: Arc<AtomicUsize>,
}
impl Snapshot {
    /// Originating document identity, retained after close for stale-result checks.
    pub fn document(&self) -> DocumentId {
        self.document
    }
    /// Revision captured by this lease, unaffected by subsequent edits.
    pub fn revision(&self) -> Revision {
        self.revision
    }
    /// Logical text length, excluding BOM.
    pub fn len(&self) -> u64 {
        self.state.root.entry.bytes
    }
    /// Whether the captured text is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Preserved serialization metadata.
    pub fn format(&self) -> Format {
        self.state.format
    }
    /// Exact number of logical lines, including the empty final line after LF.
    pub fn line_count(&self) -> u64 {
        self.state.root.entry.lines + 1
    }
    /// Round a byte position down to a UTF-8 scalar boundary with at most three bytes of context.
    /// Blocking; intended for bounded chunk readers on background workers.
    pub fn floor_scalar(&self, offset: ByteOffset) -> Result<ByteOffset, Error> {
        if offset.0 > self.len() {
            return Err(Error::InvalidRange);
        }
        let mut position = offset.0;
        let mut byte = [0];
        let mut context = 0;
        while position != 0 && position < self.len() {
            if context == 4 {
                return Err(Error::CorruptStorage);
            }
            context += 1;
            self.reader.read(*self.state, position, &mut byte)?;
            if byte[0] & 0xc0 != 0x80 {
                break;
            }
            position -= 1;
        }
        Ok(ByteOffset(position))
    }
    /// Blocking scalar-aligned window read. Use a storage worker, never a UI handler.
    pub fn read_window(&self, start: ByteOffset, max_bytes: usize) -> Result<Window, Error> {
        self.read_window_with_cancel(start, max_bytes, &Cancellation::default())
    }
    /// Read a bounded window, checking cancellation between text pages and before returning.
    /// An in-progress filesystem call must finish before cancellation can be observed.
    pub fn read_window_with_cancel(
        &self,
        start: ByteOffset,
        max_bytes: usize,
        cancel: &Cancellation,
    ) -> Result<Window, Error> {
        cancel.check()?;
        let store = &self.reader;
        if max_bytes < 4 {
            return Err(Error::InvalidRange);
        }
        let charge = max_bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(store.source.limits.text_page_bytes * 2 + 256))
            .ok_or(Error::ResourceLimit("window size overflow"))?;
        let permit = store.source.resources.buffers.acquire(charge)?;
        store.boundary(*self.state, start.0, false)?;
        let count = (self.len() - start.0).min(max_bytes as u64) as usize;
        let mut bytes = vec![0; count];
        let mut offset = start.0;
        for chunk in bytes.chunks_mut(store.source.limits.text_page_bytes) {
            cancel.check()?;
            store.read(*self.state, offset, chunk)?;
            offset += chunk.len() as u64;
        }
        cancel.check()?;
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => count,
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return Err(Error::CorruptStorage),
        };
        bytes.truncate(valid);
        let text = String::from_utf8(bytes).map_err(|_| Error::CorruptStorage)?;
        let next = start.0 + valid as u64;
        Ok(Window {
            text,
            next: (next < self.len()).then_some(ByteOffset(next)),
            _permit: permit,
        })
    }
    /// Blocking zero-based line lookup; an empty final line is addressable.
    pub fn locate_line(&self, line: u64, cancel: &Cancellation) -> Result<ByteOffset, Error> {
        cancel.check()?;
        let _buffers = self
            .reader
            .source
            .resources
            .buffers
            .acquire(self.reader.source.limits.text_page_bytes * 2 + 256)?;
        if line == 0 {
            return Ok(ByteOffset(0));
        }
        let store = &self.reader;
        Ok(ByteOffset(store.tree.after_lf(
            self.state.root,
            line - 1,
            &store.source,
            cancel,
        )?))
    }
    /// Blocking line number of a scalar boundary, including EOF.
    pub fn line_of_offset(&self, offset: ByteOffset) -> Result<u64, Error> {
        let _buffers = self
            .reader
            .source
            .resources
            .buffers
            .acquire(self.reader.source.limits.text_page_bytes * 2 + 256)?;
        let store = &self.reader;
        store.boundary(*self.state, offset.0, false)?;
        store
            .tree
            .lines_before(self.state.root, offset.0, &store.source)
    }
}
impl Drop for Snapshot {
    /// Release one externally retained state and its store generation.
    fn drop(&mut self) {
        self.leases.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Owned text window with an aggregate buffer reservation that lasts until drop.
pub struct Window {
    text: String,
    next: Option<ByteOffset>,
    _permit: Permit,
}
impl Window {
    /// Valid UTF-8 text; may contain controls that rendering must escape safely.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Next logical byte offset, or EOF.
    pub fn continuation(&self) -> Option<ByteOffset> {
        self.next
    }
}
