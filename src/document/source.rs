//! Private immutable-prefix text arenas, block indexes and independent read handles.
use super::{
    Cancellation, Error, FaultInjector, IoPoint, OpenOptions, SourceId, StorageLimits, cache::Key,
    history::State, index::LineIndex, platform, resources::Resources, tree::Tree, types::identity,
};
use std::{
    fs::{File, OpenOptions as FsOpenOptions},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};
use tempfile::TempDir;

/// Immutable-prefix text arena and block index. Only unpublished suffix bytes are mutable.
pub(super) struct Source {
    pub file: File,
    pub(super) index: LineIndex,
    pub(super) length: u64,
    pub generation: SourceId,
    pub limits: StorageLimits,
    pub resources: Arc<Resources>,
    pub faults: Option<Arc<dyn FaultInjector>>,
}
impl Source {
    /// New source; all file handles remain open for the lifetime of its generation.
    pub(super) fn new(
        file: File,
        index: File,
        options: &OpenOptions,
        resources: Arc<Resources>,
    ) -> Result<Self, Error> {
        Ok(Self {
            file,
            index: LineIndex::new(index, options.limits.text_page_bytes)?,
            length: 0,
            generation: SourceId(identity()?),
            limits: options.limits.clone(),
            resources,
            faults: options.faults.clone(),
        })
    }
    /// Duplicate immutable-prefix handles without retaining the document mutation lock.
    pub(super) fn reader(&self) -> Result<Self, Error> {
        Ok(Self {
            file: self.file.try_clone()?,
            index: self.index.reader()?,
            length: self.length,
            generation: self.generation,
            limits: self.limits.clone(),
            resources: self.resources.clone(),
            faults: self.faults.clone(),
        })
    }
    /// Current immutable prefix length, including a possible raw BOM.
    pub fn len(&self) -> u64 {
        self.length
    }
    /// Visit an injectable filesystem boundary.
    pub fn check(&self, point: IoPoint) -> Result<(), Error> {
        if let Some(faults) = &self.faults {
            faults.check(point)?;
        }
        Ok(())
    }
    /// Check working filesystem space, including configured reserve.
    pub fn reserve(&self, bytes: u64) -> Result<(), Error> {
        platform::reserve(&self.file, bytes, self.limits.disk_reserve_bytes)
    }
    /// Append immutable bytes, or only index bytes already copied by a filesystem clone.
    pub(super) fn append(&mut self, bytes: &[u8], cloned: bool) -> Result<(), Error> {
        self.check(IoPoint::Write)?;
        if !cloned {
            self.file.write_all_at(bytes, self.length)?;
        }
        self.index.append(self.length, bytes)?;
        self.length = self
            .length
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ResourceLimit("source offset overflow"))?;
        Ok(())
    }
    /// Discard an unpublished failed suffix without modifying previously referenced bytes.
    pub(super) fn rollback(&mut self, length: u64, lines: u64) -> Result<(), Error> {
        self.resources
            .text
            .lock()
            .map_err(|_| Error::CorruptStorage)?
            .discard_generation(self.generation.0);
        self.file.set_len(length)?;
        self.index.rollback(length, lines)?;
        self.length = length;
        Ok(())
    }
    /// Read a physical range using a shared cache; incomplete append pages are never cached.
    pub fn read(&self, mut offset: u64, mut out: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(out.len() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(Error::CorruptStorage);
        }
        self.check(IoPoint::Read)?;
        let page = self.limits.text_page_bytes;
        while !out.is_empty() {
            let page_id = offset / page as u64;
            let within = (offset % page as u64) as usize;
            let count = out.len().min(page - within);
            let (target, remaining) = out.split_at_mut(count);
            let key = Key(self.generation.0, 0, page_id);
            let mut cache = self
                .resources
                .text
                .lock()
                .map_err(|_| Error::CorruptStorage)?;
            if !cache.get(key, within, target) {
                if (page_id + 1) * page as u64 <= self.length {
                    let mut bytes = vec![0; page];
                    self.file.read_exact_at(&mut bytes, page_id * page as u64)?;
                    target.copy_from_slice(&bytes[within..within + count]);
                    cache.insert(key, bytes);
                } else {
                    self.file.read_exact_at(target, offset)?;
                }
            }
            offset += count as u64;
            out = remaining;
        }
        Ok(())
    }
    /// LF prefix before one physical byte position; scans at most one text page.
    fn prefix(&self, offset: u64) -> Result<u64, Error> {
        if offset > self.length {
            return Err(Error::CorruptStorage);
        }
        let page = self.limits.text_page_bytes as u64;
        let count = (offset % page) as usize;
        let mut bytes = vec![0; count];
        self.read(offset - count as u64, &mut bytes)?;
        Ok(self.index.prefix(offset / page, self.length)?
            + memchr::memchr_iter(b'\n', &bytes).count() as u64)
    }
    /// Exact range LF count, independent of a per-line object array.
    pub fn lines(&self, offset: u64, length: u64) -> Result<u64, Error> {
        let end = offset.checked_add(length).ok_or(Error::CorruptStorage)?;
        self.prefix(end)?
            .checked_sub(self.prefix(offset)?)
            .ok_or(Error::CorruptStorage)
    }
    /// Locate one LF via disk prefix binary search plus a bounded block scan.
    pub fn after_lf(
        &self,
        start: u64,
        length: u64,
        lf: u64,
        cancel: &Cancellation,
    ) -> Result<u64, Error> {
        let target = self
            .prefix(start)?
            .checked_add(lf)
            .ok_or(Error::CorruptStorage)?;
        let page = self.limits.text_page_bytes as u64;
        let end = start.checked_add(length).ok_or(Error::CorruptStorage)?;
        let mut low = start / page;
        let mut high = end / page;
        while low < high {
            cancel.check()?;
            let middle = low + (high - low).div_ceil(2);
            if self.index.prefix(middle, self.length)? <= target {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let block = low * page;
        let count = (end - block).min(page) as usize;
        let mut bytes = vec![0; count];
        self.read(block, &mut bytes)?;
        let ordinal = target - self.index.prefix(low, self.length)?;
        let offset = memchr::memchr_iter(b'\n', &bytes)
            .nth(usize::try_from(ordinal).map_err(|_| Error::CorruptStorage)?)
            .ok_or(Error::CorruptStorage)?;
        Ok(block + offset as u64 + 1 - start)
    }
}

/// A generation of private working files. Dropping the last reference removes only its directory.
pub(super) struct Store {
    pub source: Source,
    pub tree: Tree,
    pub directory: TempDir,
    pub original_longest: u64,
    pub cloned: bool,
}
impl Store {
    /// Create owner-scoped working files (directory 0700, files 0600).
    pub(super) fn empty(
        options: &OpenOptions,
        resources: Arc<Resources>,
        original: Option<&File>,
    ) -> Result<(Self, bool), Error> {
        let directory = match &options.working_directory {
            Some(parent) => tempfile::Builder::new().prefix("ede-").tempdir_in(parent)?,
            None => tempfile::Builder::new().prefix("ede-").tempdir()?,
        };
        let path = directory.path().join("text");
        let cloned = if !options.force_copy {
            original
                .map(|file| platform::clone_file(file, &path))
                .transpose()?
                .flatten()
        } else {
            None
        };
        let is_clone = cloned.is_some();
        let file = match cloned {
            Some(file) => file,
            None => private_file(&path)?,
        };
        let source = Source::new(
            file,
            private_file(&directory.path().join("lines"))?,
            options,
            resources.clone(),
        )?;
        let tree = Tree::new(
            private_file(&directory.path().join("tree"))?,
            options.limits.node_page_bytes,
            source.generation.0,
            resources.nodes.clone(),
        );
        Ok((
            Self {
                source,
                tree,
                directory,
                original_longest: 0,
                cloned: is_clone,
            },
            is_clone,
        ))
    }
    /// Capture immutable source/index/tree prefixes for lock-independent reads.
    pub(super) fn reader(&self) -> Result<Reader, Error> {
        Ok(Reader {
            source: self.source.reader()?,
            tree: self.tree.reader()?,
        })
    }
    /// Read logical bytes through piece boundaries without materializing a line or document.
    pub fn read(&self, state: State, mut offset: u64, mut out: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(out.len() as u64)
            .is_none_or(|end| end > state.root.entry.bytes)
        {
            return Err(Error::InvalidRange);
        }
        while !out.is_empty() {
            let (piece, origin) = self.tree.piece_at(state.root, offset)?;
            let within = offset - origin;
            let count = (piece.bytes - within).min(out.len() as u64) as usize;
            let (target, remaining) = out.split_at_mut(count);
            self.source.read(piece.id + within, target)?;
            offset += count as u64;
            out = remaining;
        }
        Ok(())
    }
    /// Scalar boundary check, optionally also protecting a CRLF editing boundary.
    pub(super) fn boundary(&self, state: State, offset: u64, editing: bool) -> Result<(), Error> {
        let length = state.root.entry.bytes;
        if offset > length {
            return Err(Error::InvalidRange);
        }
        if offset == 0 || offset == length {
            return Ok(());
        }
        let mut pair = [0; 2];
        self.read(state, offset - 1, &mut pair)?;
        if pair[1] & 0xc0 == 0x80 || (editing && pair == *b"\r\n") {
            return Err(Error::InvalidRange);
        }
        Ok(())
    }
}
/// Independent immutable-prefix read handles; the snapshot also pins its owner directory.
pub(super) struct Reader {
    pub source: Source,
    pub tree: Tree,
}
impl Reader {
    /// Read logical bytes through piece boundaries without materializing a line or document.
    pub fn read(&self, state: State, mut offset: u64, mut out: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(out.len() as u64)
            .is_none_or(|end| end > state.root.entry.bytes)
        {
            return Err(Error::InvalidRange);
        }
        while !out.is_empty() {
            let (piece, origin) = self.tree.piece_at(state.root, offset)?;
            let within = offset - origin;
            let count = (piece.bytes - within).min(out.len() as u64) as usize;
            let (target, remaining) = out.split_at_mut(count);
            self.source.read(piece.id + within, target)?;
            offset += count as u64;
            out = remaining;
        }
        Ok(())
    }
    /// Scalar boundary check, optionally also protecting a CRLF editing boundary.
    pub(super) fn boundary(&self, state: State, offset: u64, editing: bool) -> Result<(), Error> {
        let length = state.root.entry.bytes;
        if offset > length {
            return Err(Error::InvalidRange);
        }
        if offset == 0 || offset == length {
            return Ok(());
        }
        let mut pair = [0; 2];
        self.read(state, offset - 1, &mut pair)?;
        if pair[1] & 0xc0 == 0x80 || (editing && pair == *b"\r\n") {
            return Err(Error::InvalidRange);
        }
        Ok(())
    }
}
/// Create a private read/write session file; no accidental reuse of stale paths.
pub(super) fn private_file(path: &Path) -> Result<File, Error> {
    Ok(FsOpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}
