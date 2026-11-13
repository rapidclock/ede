//! Public value types and explicit storage policy; no terminal dependencies.

use std::{
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

/// Allocate process-unique identities without wrapping into a stale identity.
pub(super) fn identity() -> Result<u64, Error> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| Error::ResourceLimit("identity space exhausted"))
}

macro_rules! identity_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub struct $name(pub u64);
    };
}
identity_type!(
    DocumentId,
    "Process-unique document identity; closing never permits reuse."
);
identity_type!(JobId, "Process-unique storage request identity.");
identity_type!(
    SourceId,
    "Identity of one private immutable-byte store generation."
);
identity_type!(
    Revision,
    "Monotonic document epoch, including undo and redo."
);
identity_type!(
    ByteOffset,
    "Logical UTF-8 byte offset, excluding a leading BOM."
);

/// Checked half-open logical byte range. Construction rejects reversed endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextRange {
    pub(super) start: u64,
    pub(super) end: u64,
}
impl TextRange {
    /// Construct `[start, end)`; document bounds and scalar boundaries are checked by edits.
    pub fn new(start: u64, end: u64) -> Result<Self, Error> {
        if start > end {
            return Err(Error::InvalidRange);
        }
        Ok(Self { start, end })
    }
    /// Inclusive start, measured in logical bytes.
    pub fn start(self) -> ByteOffset {
        ByteOffset(self.start)
    }
    /// Exclusive end, measured in logical bytes.
    pub fn end(self) -> ByteOffset {
        ByteOffset(self.end)
    }
    /// Number of bytes covered.
    pub fn len(self) -> u64 {
        self.end - self.start
    }
    /// Whether this range is an insertion point.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Suggested terminator for the later interactive Enter command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Newline {
    /// LF, also the default for text without a terminator.
    #[default]
    Lf,
    /// A CR followed by LF.
    CrLf,
}
/// Serialization metadata, retained through edits and save points.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Format {
    /// Whether serialization includes the initial UTF-8 BOM.
    pub bom: bool,
    /// First encountered line ending; storage never normalizes inserted text.
    pub newline: Newline,
}

/// Cooperative cancellation; an in-progress OS call may finish before observing it.
#[derive(Clone, Default, Debug)]
pub struct Cancellation {
    requested: Arc<AtomicBool>,
    closed: Option<Arc<AtomicBool>>,
    stopping: Option<Arc<AtomicBool>>,
    epoch: Option<(Arc<AtomicU64>, u64)>,
}
impl Cancellation {
    /// Request cancellation. After save publication this cannot undo the commit.
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
    }
    /// Whether a cancellation request has been issued.
    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
            || self
                .epoch
                .as_ref()
                .is_some_and(|(epoch, expected)| epoch.load(Ordering::Acquire) != *expected)
            || self
                .closed
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            || self
                .stopping
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
    }
    /// Link document close and runtime shutdown without maintaining job lists.
    pub(crate) fn linked(closed: Arc<AtomicBool>, stopping: Arc<AtomicBool>) -> Self {
        Self {
            closed: Some(closed),
            stopping: Some(stopping),
            ..Self::default()
        }
    }
    /// Share explicit cancellation while rejecting a superseded background view generation.
    pub(crate) fn for_epoch(&self, epoch: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            epoch: Some((epoch, expected)),
            ..self.clone()
        }
    }
    /// Return a typed cancellation failure at a safe operation boundary.
    pub(super) fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Recoverable document failures; I/O errors retain the original OS error.
#[derive(Debug)]
pub enum Error {
    /// Directory, device, socket or FIFO, not an editable regular file.
    UnsupportedType,
    /// Unsupported encoding or invalid UTF-8 at a raw source byte offset.
    UnsupportedEncoding {
        /// Raw offset, including any BOM.
        offset: u64,
    },
    /// A NUL byte was detected outside a recognized encoding BOM.
    Binary,
    /// Source version changed while preparing a private snapshot.
    SourceChanged,
    /// Out-of-bounds, reversed, non-scalar or bisected CRLF edit range.
    InvalidRange,
    /// A mutation did not name the current revision.
    StaleRevision,
    /// Operation stopped before its commit point.
    Cancelled,
    /// A configured resource budget would be exceeded.
    ResourceLimit(&'static str),
    /// Destination contents, identity, link or metadata changed.
    SaveConflict,
    /// This destination cannot safely be replaced under the current policy.
    SaveAsRequired(&'static str),
    /// A save worker panicked; publication may have occurred and needs explicit user attention.
    SaveWorkerPanicked,
    /// No additional bounded request/lease can currently be admitted.
    Busy,
    /// A private storage page failed validation.
    CorruptStorage,
    /// The document was closed or belongs to another service.
    Closed,
    /// Underlying filesystem failure.
    Io(io::Error),
}
impl fmt::Display for Error {
    /// Render a concise diagnostic; preserve OS detail on I/O failures.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedType => f.write_str("Cannot open this file type"),
            Self::UnsupportedEncoding { offset } => {
                write!(f, "Unsupported text encoding at byte {offset}")
            }
            Self::Binary => f.write_str("Cannot open binary file"),
            Self::SourceChanged => f.write_str("Source changed while opening; retry explicitly"),
            Self::InvalidRange => f.write_str("Invalid text range"),
            Self::StaleRevision => f.write_str("Document revision changed"),
            Self::Cancelled => f.write_str("Operation cancelled"),
            Self::ResourceLimit(reason) => write!(f, "Storage resource limit: {reason}"),
            Self::SaveConflict => f.write_str("Destination changed; save conflict"),
            Self::SaveAsRequired(reason) => write!(f, "Save As required: {reason}"),
            Self::SaveWorkerPanicked => {
                f.write_str("Save worker stopped; destination may have changed")
            }
            Self::Busy => f.write_str("Storage busy; retry after releasing work"),
            Self::CorruptStorage => f.write_str("Invalid private storage page"),
            Self::Closed => f.write_str("Document closed"),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {
    /// Expose original OS errors to callers without flattening them into text.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for Error {
    /// Retain an I/O error and its raw OS code.
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Injectable operation boundaries. Used by tests, never CLI failure switches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoPoint {
    /// Reading original or private text.
    Read,
    /// Appending immutable text, indexes, or pages.
    Write,
    /// Explicit flush before save synchronization.
    Flush,
    /// Synchronize save output before publishing its name.
    Sync,
    /// Preserve/check destination metadata.
    Metadata,
    /// Final cancellation/conflict boundary before name publication.
    Publish,
    /// Parent-directory sync after name publication.
    DirectorySync,
    /// Verify the published destination identity.
    Verify,
}
/// Optional failure/interleaving seam for deterministic filesystem tests.
pub trait FaultInjector: Send + Sync + fmt::Debug {
    /// Called at the named boundary. Returning an error stops pre-commit work.
    fn check(&self, point: IoPoint) -> io::Result<()>;
}

/// Per-service aggregate limits. Tiny values make paging/resource tests practical.
#[derive(Clone, Debug)]
pub struct StorageLimits {
    /// Resident immutable text pages, shared by documents in a service.
    pub text_cache_bytes: usize,
    /// Resident tree/index pages, shared by documents in a service.
    pub node_cache_bytes: usize,
    /// Aggregate owned window, queued payload, and worker staging allowance.
    pub buffer_bytes: usize,
    /// Text/index block width; must be at least four bytes.
    pub text_page_bytes: usize,
    /// Fixed tree page width; defaults to 16 KiB.
    pub node_page_bytes: usize,
    /// Maximum retained undo transactions per document.
    pub undo_transactions: u64,
    /// Maximum non-current text retained by undo/redo per document.
    pub undo_bytes: u64,
    /// Concurrent external snapshot leases per document.
    pub leases: usize,
    /// Bounded request and result queue capacity.
    pub queue_entries: usize,
    /// Free disk space required beyond planned allocation.
    pub disk_reserve_bytes: u64,
}
impl Default for StorageLimits {
    /// Initial Stage 02 budgets; none impose a document-size cutoff.
    fn default() -> Self {
        Self {
            text_cache_bytes: 64 << 20,
            node_cache_bytes: 32 << 20,
            buffer_bytes: 32 << 20,
            text_page_bytes: 64 << 10,
            node_page_bytes: 16 << 10,
            undo_transactions: 1000,
            undo_bytes: 256 << 20,
            leases: 8,
            queue_entries: 16,
            disk_reserve_bytes: 512 << 20,
        }
    }
}
impl StorageLimits {
    /// Reject unusable or overflowing budgets before allocating storage.
    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.text_page_bytes < 4
            || self.text_page_bytes > 1 << 20
            || self.node_page_bytes < 256
            || self.node_page_bytes > 1 << 20
            || self.text_cache_bytes < self.text_page_bytes * 2
            || self.node_cache_bytes < self.node_page_bytes * 4
            || self.buffer_bytes < self.text_page_bytes * 8 + self.node_page_bytes * 8 + (256 << 10)
            || self.undo_transactions == 0
            || self.leases == 0
            || self.queue_entries == 0
            || self.queue_entries > 1024
            || self.leases > 1024
        {
            return Err(Error::ResourceLimit("invalid storage limits"));
        }
        self.text_cache_bytes
            .checked_add(self.node_cache_bytes)
            .and_then(|n| n.checked_add(self.buffer_bytes))
            .ok_or(Error::ResourceLimit("memory budget overflow"))?;
        Ok(())
    }
}

/// Measured managed storage usage; OS page cache and allocator overhead are separate.
#[derive(Clone, Copy, Debug, Default)]
pub struct StorageStats {
    /// Whether the initial private snapshot used an actual filesystem clone.
    pub snapshot_cloned: bool,
    /// Resident cache allocations including cache-key/accounting allowance.
    pub cache_bytes: usize,
    /// Sum of the individual text/node cache-pool peaks (a conservative aggregate high water).
    pub cache_high_water_bytes: usize,
    /// Highest admitted aggregate storage buffer charge, including queued/in-flight jobs.
    pub buffer_high_water_bytes: usize,
    /// Current immutable source bytes allocated in the working store.
    pub text_bytes: u64,
    /// Tree, index, and history working bytes.
    pub metadata_bytes: u64,
    /// Exact LF count plus the final logical line.
    pub lines: u64,
    /// Longest logical line in the original snapshot, excluding LF but including CR.
    pub original_longest_line: u64,
}

use std::path::PathBuf;

/// Opening configuration; filesystem clones are optional and can be forced off in tests.
#[derive(Clone, Default)]
pub struct OpenOptions {
    /// Starting storage budgets, shared when used through a service.
    pub limits: StorageLimits,
    /// Parent directory for private per-instance storage, or the OS temporary directory.
    pub working_directory: Option<PathBuf>,
    /// Exercise the mandatory portable copy path even on clone-capable filesystems.
    pub force_copy: bool,
    /// Optional deterministic failure/interleaving seam.
    pub faults: Option<Arc<dyn FaultInjector>>,
}
/// Observable opening progress. Preview remains provisional and read-only until Ready.
#[derive(Debug)]
pub enum OpenProgress {
    /// Initial validated UTF-8 prefix; does not establish whole-file validity.
    Preview {
        /// Bounded logical source text, excluding BOM.
        text: String,
        /// Observed source length, before complete validation.
        raw_bytes: u64,
    },
    /// Completed private-copy/validation bytes. Callers may coalesce these updates.
    Preparing {
        /// Raw bytes processed.
        completed: u64,
        /// Observed original source length.
        total: u64,
    },
}
/// One borrowed replacement against a transaction's base revision.
pub struct Edit<'a> {
    /// Checked logical range to replace.
    pub range: TextRange,
    /// Valid UTF-8 insertion; NUL is rejected by the storage layer.
    pub text: &'a str,
}
