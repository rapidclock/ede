//! Document ownership, stable opening, revisions and transaction publication.
use super::{
    Cancellation, DocumentId, Edit, Error, Format, IoPoint, OpenOptions, OpenProgress, Revision,
    Snapshot, StagedTransaction, StorageStats, TextRange,
    history::{History, State},
    index::Validator,
    platform::{self, Version},
    resources::Resources,
    source::{Store, private_file},
    tree::{Entry, Root},
    types::identity,
};
use std::{
    fs,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Immutable expected destination content plus observed path/target identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Baseline {
    pub state: State,
    pub supplied: PathBuf,
    pub resolved: PathBuf,
    pub version: Version,
}
/// Mutable document owner. All methods performing I/O must run on a storage worker.
pub struct Document {
    pub(super) store: Arc<Mutex<Store>>,
    pub(super) current: State,
    pub(super) baseline: Option<Baseline>,
    pub(super) attention: bool,
    pub(super) history: History,
    pub(super) id: DocumentId,
    pub(super) revision: Revision,
    pub(super) leases: Arc<AtomicUsize>,
    pub(super) blocking_leases: Arc<AtomicUsize>,
    pub(super) pruned: u64,
}
impl Document {
    /// Prepare a stable private snapshot. Preview callbacks precede full validation.
    pub fn open(
        path: impl AsRef<Path>,
        options: OpenOptions,
        cancel: &Cancellation,
        progress: impl FnMut(OpenProgress),
    ) -> Result<Self, Error> {
        let resources = Resources::new(&options.limits)?;
        Self::open_shared(path.as_ref(), options, resources, cancel, progress)
    }
    /// Service entry point sharing cache and allocation budgets across documents.
    pub(super) fn open_shared(
        path: &Path,
        options: OpenOptions,
        resources: Arc<Resources>,
        cancel: &Cancellation,
        mut progress: impl FnMut(OpenProgress),
    ) -> Result<Self, Error> {
        cancel.check()?;
        let (original, resolved, version) = platform::open(path)?;
        let _buffers = resources
            .buffers
            .acquire(options.limits.text_page_bytes * 6 + 512)?;
        let mut preview = vec![0; version.len.min(options.limits.text_page_bytes as u64) as usize];
        original.read_exact_at(&mut preview, 0)?;
        let mut validator = Validator::new();
        validator.feed(&preview, preview.len() as u64 == version.len)?;
        let skip = if validator.format.bom { 3 } else { 0 };
        let valid = match std::str::from_utf8(&preview[skip..]) {
            Ok(_) => preview.len() - skip,
            Err(e) => e.valid_up_to(),
        };
        progress(OpenProgress::Preview {
            text: std::str::from_utf8(&preview[skip..skip + valid])
                .map_err(|_| Error::CorruptStorage)?
                .to_owned(),
            raw_bytes: version.len,
        });
        cancel.check()?;
        let (mut store, cloned) = Store::empty(&options, resources, Some(&original))?;
        store.source.reserve(
            (if cloned { 0 } else { version.len })
                .saturating_add(version.len / options.limits.text_page_bytes as u64 * 8),
        )?;
        let mut validator = Validator::new();
        let mut bytes = vec![0; options.limits.text_page_bytes];
        let mut offset = 0;
        loop {
            cancel.check()?;
            let count = (version.len - offset).min(bytes.len() as u64) as usize;
            store.source.check(IoPoint::Read)?;
            if cloned {
                store
                    .source
                    .file
                    .read_exact_at(&mut bytes[..count], offset)?;
            } else {
                original.read_exact_at(&mut bytes[..count], offset)?;
            }
            let eof = offset + count as u64 == version.len;
            validator.feed(&bytes[..count], eof)?;
            store.source.append(&bytes[..count], cloned)?;
            offset += count as u64;
            progress(OpenProgress::Preparing {
                completed: offset,
                total: version.len,
            });
            if eof {
                break;
            }
        }
        cancel.check()?;
        if Version::of(&original.metadata()?) != version
            || fs::canonicalize(path)? != resolved
            || Version::of(&fs::metadata(&resolved)?) != version
        {
            return Err(Error::SourceChanged);
        }
        let skip = if validator.format.bom { 3 } else { 0 };
        let piece = Entry {
            id: skip,
            bytes: version.len - skip,
            lines: store.source.index.total(),
        };
        let root = store.tree.initial(piece, &store.source)?;
        store.original_longest = validator.longest;
        let state = State {
            root,
            format: validator.format,
            identity: identity()?,
        };
        let baseline = Baseline {
            state,
            supplied: absolute(path)?,
            resolved,
            version,
        };
        Self::from_store(store, state, Some(baseline))
    }
    /// Create an unnamed empty document; the recovery service persists its snapshots.
    pub fn new(options: OpenOptions) -> Result<Self, Error> {
        let resources = Resources::new(&options.limits)?;
        Self::new_shared(options, resources)
    }
    /// Create an unnamed document using service-wide shared resource pools.
    pub(super) fn new_shared(
        options: OpenOptions,
        resources: Arc<Resources>,
    ) -> Result<Self, Error> {
        let (mut store, _) = Store::empty(&options, resources, None)?;
        let root = store.tree.initial(Entry::default(), &store.source)?;
        let state = State {
            root,
            format: Format::default(),
            identity: identity()?,
        };
        Self::from_store(store, state, None)
    }
    /// Finish publication only after private bytes, tree and initial history exist.
    fn from_store(store: Store, current: State, baseline: Option<Baseline>) -> Result<Self, Error> {
        let history = History::new(
            private_file(&store.directory.path().join("history"))?,
            current,
        )?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            current,
            baseline,
            attention: false,
            history,
            id: DocumentId(identity()?),
            revision: Revision(0),
            leases: Arc::new(AtomicUsize::new(0)),
            blocking_leases: Arc::new(AtomicUsize::new(0)),
            pruned: 0,
        })
    }
    /// Remove an object's temporary path and start history at the checkpointed format.
    pub(super) fn restore_scratch(&mut self, format: Format, len: u64) -> Result<(), Error> {
        if self.current.root.entry.bytes != len || self.current.format.bom != format.bom {
            return Err(Error::CorruptStorage);
        }
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let state = State {
            format,
            ..self.current
        };
        let history = History::new(tempfile::tempfile_in(store.directory.path())?, state)?;
        self.current = state;
        self.baseline = None;
        self.history = history;
        Ok(())
    }
    /// Stable document identity.
    pub fn id(&self) -> DocumentId {
        self.id
    }
    /// Monotonic content revision.
    pub fn revision(&self) -> Revision {
        self.revision
    }
    /// Whether current content differs from its acknowledged save point.
    pub fn is_dirty(&self) -> bool {
        self.attention
            || self.baseline.as_ref().is_none_or(|saved| {
                saved.state.identity != self.current.identity
                    || saved.state.format != self.current.format
            })
    }
    /// Supplied destination path, retained separately from a resolved symlink target.
    pub fn path(&self) -> Option<&Path> {
        self.baseline.as_ref().map(|b| b.supplied.as_path())
    }
    /// Create a bounded immutable read/save lease; the caller must release it.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.capture_snapshot(true)
    }
    /// Background readers pin their old immutable generation without blocking a new one.
    pub(super) fn background_snapshot(&self) -> Result<Snapshot, Error> {
        self.capture_snapshot(false)
    }
    fn capture_snapshot(&self, blocking: bool) -> Result<Snapshot, Error> {
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let limit = store.source.limits.leases;
        let mut reader = store.reader()?;
        if !blocking {
            reader.source.uncached = true;
            reader.tree = store.tree.uncached_reader()?;
        }
        let reader = Box::new(reader);
        self.leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .map_err(|_| Error::Busy)?;
        if blocking {
            self.blocking_leases.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Snapshot {
            store: self.store.clone(),
            state: Box::new(self.current),
            reader,
            document: self.id,
            revision: self.revision,
            leases: self.leases.clone(),
            blocking_leases: blocking.then(|| self.blocking_leases.clone()),
        })
    }
    /// Start an on-disk transaction plan for incremental, bounded-memory staging.
    pub fn transaction(&self, expected: Revision) -> Result<StagedTransaction, Error> {
        if expected != self.revision {
            return Err(Error::StaleRevision);
        }
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        Ok(StagedTransaction {
            file: tempfile::tempfile_in(store.directory.path())?,
            document: self.id,
            expected,
            count: 0,
            last_end: 0,
            length: 0,
            max_buffer: store.source.limits.text_page_bytes,
            reserve: store.source.limits.disk_reserve_bytes,
            resources: store.source.resources.clone(),
            _permit: store.source.resources.buffers.acquire(1024)?,
        })
    }
    /// Convenience transaction for borrowed replacements. Large batches spool to disk.
    pub fn apply<'a>(
        &mut self,
        expected: Revision,
        edits: impl IntoIterator<Item = Edit<'a>>,
        cancel: &Cancellation,
    ) -> Result<Revision, Error> {
        let mut transaction = self.transaction(expected)?;
        for edit in edits {
            transaction.replace(edit.range, edit.text.as_bytes(), cancel)?;
        }
        self.apply_staged(transaction, cancel)
    }
    /// Commit one fully staged transaction or leave content, revision and history unchanged.
    pub fn apply_staged(
        &mut self,
        mut transaction: StagedTransaction,
        cancel: &Cancellation,
    ) -> Result<Revision, Error> {
        if transaction.document != self.id || transaction.expected != self.revision {
            return Err(Error::StaleRevision);
        }
        cancel.check()?;
        let next_revision = self.next_revision()?;
        let needs_compaction = self.requires_compaction()?;
        if needs_compaction {
            self.compact(cancel)?;
        }
        let mut store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let _buffers = store.source.resources.buffers.acquire(
            store.source.limits.text_page_bytes * 6
                + store.source.limits.node_page_bytes * 8
                + (128 << 10),
        )?;
        store.source.reserve(transaction.length)?;
        let old_len = store.source.length;
        let old_lines = store.source.index.total();
        let result = self.stage_apply(&mut store, &mut transaction, cancel);
        let (root, changed) = match result {
            Ok(result) => result,
            Err(error) => {
                store.source.rollback(old_len, old_lines)?;
                return Err(error);
            }
        };
        let same = if changed && root.entry.bytes == self.current.root.entry.bytes {
            match same_text(&store, self.current.root, root, cancel) {
                Ok(same) => same,
                Err(error) => {
                    store.source.rollback(old_len, old_lines)?;
                    return Err(error);
                }
            }
        } else {
            !changed
        };
        if same {
            store.source.rollback(old_len, old_lines)?;
            return Ok(self.revision);
        }
        let state = State {
            root,
            format: self.current.format,
            identity: identity()?,
        };
        // History budgeting is checked before logical publication; newest undo is mandatory.
        let begin = match self.history_budget(&store, state, cancel) {
            Ok(begin) => begin,
            Err(error) => {
                store.source.rollback(old_len, old_lines)?;
                return Err(error);
            }
        };
        store.source.check(IoPoint::Write)?;
        let next = self.history.stage(state)?;
        cancel.check()?;
        self.pruned = self
            .pruned
            .checked_add(self.history.pruned_count(begin)?)
            .ok_or(Error::ResourceLimit("pruning counter overflow"))?;
        self.history.publish(next, begin);
        self.current = state;
        self.revision = next_revision;
        Ok(self.revision)
    }
    /// Let interactive clients yield background snapshot leases before mandatory compaction.
    pub(super) fn requires_compaction(&self) -> Result<bool, Error> {
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let live_upper = self
            .current
            .root
            .entry
            .bytes
            .saturating_add(
                self.baseline
                    .as_ref()
                    .map_or(0, |b| b.state.root.entry.bytes),
            )
            .saturating_add(store.source.limits.undo_bytes.saturating_mul(2));
        Ok(store.source.length > live_upper.saturating_add(1 << 20)
            || store
                .tree
                .disk_bytes()
                .saturating_add(self.history.disk_bytes()?)
                > 64 << 20)
    }
    /// Validate and apply reverse-ordered disk descriptors against one base revision.
    fn stage_apply(
        &self,
        store: &mut Store,
        transaction: &mut StagedTransaction,
        cancel: &Cancellation,
    ) -> Result<(Root, bool), Error> {
        let mut root = self.current.root;
        let mut changed = false;
        let mut tail = transaction.length;
        let mut buffer = vec![0; store.source.limits.text_page_bytes];
        for _ in 0..transaction.count {
            cancel.check()?;
            let mut footer = [0; 32];
            transaction.file.read_exact_at(&mut footer, tail - 32)?;
            let word = |i| u64::from_le_bytes(footer[i..i + 8].try_into().expect("staged footer"));
            let (start, end, length, record_start) = (word(0), word(8), word(16), word(24));
            let range = TextRange::new(start, end)?;
            store.boundary(self.current, start, true)?;
            store.boundary(self.current, end, true)?;
            let mut input_offset = record_start;
            let source_start = store.source.length;
            let mut remaining = length;
            let mut same = length == range.len();
            let mut compared = 0;
            while remaining != 0 {
                cancel.check()?;
                let count = remaining.min(buffer.len() as u64) as usize;
                transaction
                    .file
                    .read_exact_at(&mut buffer[..count], input_offset)?;
                if same {
                    let mut old = vec![0; count];
                    store.read(self.current, start + compared, &mut old)?;
                    same = old == buffer[..count];
                }
                store.source.append(&buffer[..count], false)?;
                input_offset += count as u64;
                remaining -= count as u64;
                compared += count as u64;
            }
            if !same {
                let piece = if length == 0 {
                    None
                } else {
                    Some(Entry {
                        id: source_start,
                        bytes: length,
                        lines: store.source.lines(source_start, length)?,
                    })
                };
                root = store
                    .tree
                    .replace(root, start, end, piece, &store.source, cancel)?;
                changed = true;
            }
            tail = record_start;
        }
        Ok((root, changed))
    }
    /// Check an expected revision and allocate its next epoch before doing writes.
    fn next_revision(&self) -> Result<Revision, Error> {
        self.revision
            .0
            .checked_add(1)
            .map(Revision)
            .ok_or(Error::ResourceLimit("revision overflow"))
    }
    /// Undo one complete transaction, retaining redo and saved-state identity.
    pub fn undo(&mut self, expected: Revision) -> Result<Revision, Error> {
        if expected != self.revision {
            return Err(Error::StaleRevision);
        }
        if self.history.cursor == self.history.begin {
            return Ok(self.revision);
        }
        let revision = self.next_revision()?;
        let previous = self.history.previous(self.history.cursor)?;
        let state = self.history.read(previous)?;
        self.store
            .lock()
            .map_err(|_| Error::CorruptStorage)?
            .tree
            .check_root(state.root)?;
        self.history.cursor = previous;
        self.current = state;
        self.revision = revision;
        Ok(revision)
    }
    /// Redo one retained transaction without assigning an old revision number.
    pub fn redo(&mut self, expected: Revision) -> Result<Revision, Error> {
        if expected != self.revision {
            return Err(Error::StaleRevision);
        }
        if self.history.cursor == self.history.end {
            return Ok(self.revision);
        }
        let revision = self.next_revision()?;
        let next = self.history.successor(self.history.cursor)?;
        let state = self.history.read(next)?;
        self.store
            .lock()
            .map_err(|_| Error::CorruptStorage)?
            .tree
            .check_root(state.root)?;
        self.history.cursor = next;
        self.current = state;
        self.revision = revision;
        Ok(revision)
    }
    /// Consume the number of whole undo transactions pruned by successful edits.
    pub fn take_history_pruned(&mut self) -> u64 {
        std::mem::take(&mut self.pruned)
    }
    /// Current measured disk/cache/line state without an unbounded metadata walk.
    pub fn stats(&self) -> Result<StorageStats, Error> {
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        // End each cache lock before taking another. Temporary guards inside a
        // struct initializer otherwise live to its end and can relock the same mutex.
        let (text_bytes, text_peak) = {
            let cache = store
                .source
                .resources
                .text
                .lock()
                .map_err(|_| Error::CorruptStorage)?;
            (cache.bytes(), cache.high_water())
        };
        let (node_bytes, node_peak) = {
            let cache = store
                .source
                .resources
                .nodes
                .lock()
                .map_err(|_| Error::CorruptStorage)?;
            (cache.bytes(), cache.high_water())
        };
        Ok(StorageStats {
            snapshot_cloned: store.cloned,
            cache_bytes: text_bytes + node_bytes,
            text_bytes: store.source.len(),
            cache_high_water_bytes: text_peak + node_peak,
            buffer_high_water_bytes: store.source.resources.buffers.high_water(),
            metadata_bytes: store.tree.disk_bytes()
                + store.source.index.disk_bytes(store.source.length)
                + self.history.disk_bytes()?,
            lines: self.current.root.entry.lines + 1,
            original_longest_line: store.original_longest,
        })
    }
}
/// Compare equal-length candidate text exactly, skipping unchanged physical ranges.
/// Buffers stay page-sized; cancelling a net-zero batch still cannot publish a root.
fn same_text(
    store: &Store,
    before: Root,
    after: Root,
    cancel: &Cancellation,
) -> Result<bool, Error> {
    let mut offset = 0;
    let mut a = vec![0; store.source.limits.text_page_bytes];
    let mut b = vec![0; a.len()];
    while offset < before.entry.bytes {
        cancel.check()?;
        let (left, left_origin) = store.tree.piece_at(before, offset)?;
        let (right, right_origin) = store.tree.piece_at(after, offset)?;
        let left_start = left.id + (offset - left_origin);
        let right_start = right.id + (offset - right_origin);
        let remaining =
            (left.bytes - (offset - left_origin)).min(right.bytes - (offset - right_origin));
        if left_start == right_start {
            offset += remaining;
            continue;
        }
        let count = remaining.min(a.len() as u64) as usize;
        store.source.read(left_start, &mut a[..count])?;
        store.source.read(right_start, &mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
        offset += count as u64;
    }
    Ok(true)
}
/// Make a supplied relative path stable against later process cwd changes.
pub(super) fn absolute(path: &Path) -> Result<PathBuf, Error> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::StorageLimits;

    /// Exercise structural occupancy after fragmented edits and broad deletion/repair.
    #[test]
    fn bplus_occupancy_and_cache_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let options = OpenOptions {
            working_directory: Some(directory.path().to_owned()),
            force_copy: true,
            limits: StorageLimits {
                text_page_bytes: 32,
                node_page_bytes: 256,
                text_cache_bytes: 1024,
                node_cache_bytes: 2048,
                buffer_bytes: 1 << 20,
                disk_reserve_bytes: 0,
                ..StorageLimits::default()
            },
            faults: None,
        };
        let mut document = Document::new(options).unwrap();
        let cancel = Cancellation::default();
        document
            .apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text: &"a".repeat(2000),
                }],
                &cancel,
            )
            .unwrap();
        document
            .apply(
                document.revision(),
                (0..1000).map(|i| Edit {
                    range: TextRange::new(i * 2, i * 2).unwrap(),
                    text: "b\n",
                }),
                &cancel,
            )
            .unwrap();
        for (start, end) in [(99, 3088), (0, 95), (0, 300), (1, 2)] {
            document
                .apply(
                    document.revision(),
                    [Edit {
                        range: TextRange::new(start, end).unwrap(),
                        text: "z",
                    }],
                    &cancel,
                )
                .unwrap();
            let store = document.store.lock().unwrap();
            store
                .tree
                .validate(document.current.root, &store.source, true)
                .unwrap();
        }
        let old = document.snapshot().unwrap();
        let revision = document.revision();
        // The later descriptor is staged before the invalid earlier endpoint is encountered.
        assert!(
            document
                .apply(
                    revision,
                    [Edit {
                        range: TextRange::new(0, u64::MAX).unwrap(),
                        text: "bad"
                    }],
                    &cancel
                )
                .is_err()
        );
        drop(old);
        document.compact(&cancel).unwrap();
        let store = document.store.lock().unwrap();
        store
            .tree
            .validate(document.current.root, &store.source, true)
            .unwrap();
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    use crate::document::ByteOffset;

    /// Corrupt private node headers are reported rather than trusted or interpreted as text.
    #[test]
    fn corrupt_node_is_a_typed_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        fs::write(&path, b"safe text").unwrap();
        let document = Document::open(
            path,
            OpenOptions {
                working_directory: Some(directory.path().to_owned()),
                ..OpenOptions::default()
            },
            &Cancellation::default(),
            |_| {},
        )
        .unwrap();
        let store = document.store.lock().unwrap();
        let file = fs::OpenOptions::new()
            .write(true)
            .open(store.directory.path().join("tree"))
            .unwrap();
        file.write_all_at(b"badmagic", 0).unwrap();
        store
            .source
            .resources
            .nodes
            .lock()
            .unwrap()
            .discard_generation(store.source.generation.0);
        drop(store);
        assert!(matches!(
            document.snapshot().unwrap().read_window(ByteOffset(0), 8),
            Err(Error::CorruptStorage)
        ));
    }
}
