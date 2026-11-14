//! Private immutable-prefix text arenas, block indexes and independent read handles.
use super::{
    Cancellation, Error, FaultInjector, IoPoint, OpenOptions, SourceId, StorageLimits,
    cache::Key,
    history::State,
    index::LineIndex,
    platform,
    resources::{Permit, Resources},
    tree::Tree,
    types::identity,
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
    dirty_bytes: u64,
    pub generation: SourceId,
    pub limits: StorageLimits,
    pub resources: Arc<Resources>,
    pub faults: Option<Arc<dyn FaultInjector>>,
    pub(super) uncached: bool,
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
            dirty_bytes: 0,
            generation: SourceId(identity()?),
            limits: options.limits.clone(),
            resources,
            faults: options.faults.clone(),
            uncached: false,
        })
    }
    /// Duplicate immutable-prefix handles without retaining the document mutation lock.
    pub(super) fn reader(&self) -> Result<Self, Error> {
        Ok(Self {
            file: self.file.try_clone()?,
            index: self.index.reader()?,
            length: self.length,
            dirty_bytes: 0,
            generation: self.generation,
            limits: self.limits.clone(),
            resources: self.resources.clone(),
            faults: self.faults.clone(),
            uncached: self.uncached,
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
        if !cloned {
            self.dirty_bytes += bytes.len() as u64;
            if self.dirty_bytes >= 8 << 20 {
                self.check(IoPoint::Sync)?;
                self.file.sync_data()?;
                self.dirty_bytes = 0;
            }
        }
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
        self.dirty_bytes = 0;
        Ok(())
    }
    /// Read a physical range using a shared cache; incomplete append pages are never cached.
    pub fn read(&self, mut offset: u64, mut out: &mut [u8]) -> Result<(), Error> {
        if self.uncached {
            self.check(IoPoint::Read)?;
            if offset
                .checked_add(out.len() as u64)
                .is_none_or(|end| end > self.length)
            {
                return Err(Error::CorruptStorage);
            }
            self.file.read_exact_at(out, offset)?;
            return Ok(());
        }
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
        self.index
            .prefix(offset / page, self.length)?
            .checked_add(memchr::memchr_iter(b'\n', &bytes).count() as u64)
            .ok_or(Error::CorruptStorage)
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
    _permit: Permit,
    pub source: Source,
    pub tree: Tree,
    pub directory: TempDir,
    pub original_longest: u64,
    pub cloned: bool,
}
impl Store {
    /// Fork captured immutable prefixes into a private generation; no live-store lock spans I/O.
    pub(super) fn fork(
        reader: &Reader,
        options: &OpenOptions,
        cancel: &Cancellation,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Self, Error> {
        let resources = reader.source.resources.clone();
        let _permit = resources
            .buffers
            .acquire(options.limits.text_page_bytes * 2)?;
        let (mut fresh, cloned) = Self::empty(options, resources, Some(&reader.source.file))?;
        fresh.source.uncached = true;
        fresh
            .source
            .reserve(if cloned { 0 } else { reader.source.length })?;
        if cloned {
            fresh.source.file.set_len(reader.source.length)?;
        }
        let mut bytes = vec![0; options.limits.text_page_bytes];
        let mut offset = 0;
        while offset < reader.source.length {
            cancel.check()?;
            let count = (reader.source.length - offset).min(bytes.len() as u64) as usize;
            reader.source.read(offset, &mut bytes[..count])?;
            fresh.source.append(&bytes[..count], cloned)?;
            offset += count as u64;
            progress(offset, reader.source.length);
        }
        fresh.tree = reader.tree.fork_to(
            private_file(&fresh.directory.path().join("fork-tree"))?,
            &fresh.source,
            cancel,
        )?;
        Ok(fresh)
    }
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
        platform::secure_directory(directory.path())?;
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
                _permit: resources.buffers.acquire(32 << 10)?,
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
    /// Read a logical range through immutable pieces without materializing a line.
    pub fn read(&self, state: State, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        read_text(&self.source, &self.tree, state, offset, out)
    }
    /// Validate a scalar endpoint, optionally requiring CRLF edit atomicity.
    pub(super) fn boundary(&self, state: State, offset: u64, editing: bool) -> Result<(), Error> {
        boundary(&self.source, &self.tree, state, offset, editing)
    }
}
/// Independent immutable-prefix read handles; the snapshot also pins its owner directory.
pub(super) struct Reader {
    pub source: Source,
    pub tree: Tree,
}
impl Reader {
    /// Read a logical range through immutable pieces without materializing a line.
    pub fn read(&self, state: State, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        read_text(&self.source, &self.tree, state, offset, out)
    }
    /// Validate a scalar endpoint, optionally requiring CRLF edit atomicity.
    pub(super) fn boundary(&self, state: State, offset: u64, editing: bool) -> Result<(), Error> {
        boundary(&self.source, &self.tree, state, offset, editing)
    }
}

/// Create a private read/write session file; no accidental reuse of stale paths.
pub(super) fn private_file(path: &Path) -> Result<File, Error> {
    let file = FsOpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(file)
}

/// Traverse logical pieces into an already bounded caller buffer.
fn read_text(
    source: &Source,
    tree: &Tree,
    state: State,
    mut offset: u64,
    mut out: &mut [u8],
) -> Result<(), Error> {
    if offset
        .checked_add(out.len() as u64)
        .is_none_or(|end| end > state.root.entry.bytes)
    {
        return Err(Error::InvalidRange);
    }
    while !out.is_empty() {
        let (piece, origin) = tree.piece_at(state.root, offset)?;
        let within = offset - origin;
        let count = (piece.bytes - within).min(out.len() as u64) as usize;
        let (target, remaining) = out.split_at_mut(count);
        source.read(piece.id + within, target)?;
        offset += count as u64;
        out = remaining;
    }
    Ok(())
}
/// Validate a logical UTF-8 endpoint and, for edits, an indivisible CRLF pair.
fn boundary(
    source: &Source,
    tree: &Tree,
    state: State,
    offset: u64,
    editing: bool,
) -> Result<(), Error> {
    let length = state.root.entry.bytes;
    if offset > length {
        return Err(Error::InvalidRange);
    }
    if offset == 0 || offset == length {
        return Ok(());
    }
    let mut pair = [0; 2];
    read_text(source, tree, state, offset - 1, &mut pair)?;
    if pair[1] & 0xc0 == 0x80 || (editing && pair == *b"\r\n") {
        return Err(Error::InvalidRange);
    }
    Ok(())
}
