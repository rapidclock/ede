//! Persistent byte/LF-weighted B+ tree with immutable fixed-width disk pages.
//!
//! Nodes contain disk IDs, never recursively retained heap nodes. Splicing copies
//! affected paths and repairs underfull siblings before publishing a root.

use super::{
    Cancellation, Error, IoPoint,
    cache::{Cache, Key},
    source::Source,
};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    sync::{Arc, Mutex},
};

/// Immutable piece or child reference: physical offset/page ID, byte weight, LF weight.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Entry {
    pub id: u64,
    pub bytes: u64,
    pub lines: u64,
}
/// A tree root, including height to validate every traversed child.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Root {
    pub entry: Entry,
    pub height: u16,
}
impl Root {
    /// A stream's immutable contiguous extent needs no node write on every append.
    /// Such roots are used only for reads. Promotion installs an ordinary leaf.
    pub(super) fn contiguous(piece: Entry) -> Self {
        debug_assert!(matches!(piece.id, 0 | 3));
        Self {
            entry: Entry {
                id: u64::MAX - piece.id,
                ..piece
            },
            height: 0,
        }
    }

    fn stream_piece(self) -> Option<Entry> {
        match u64::MAX - self.entry.id {
            offset @ (0 | 3) if self.height == 0 => Some(Entry {
                id: offset,
                ..self.entry
            }),
            _ => None,
        }
    }
}
/// Disk old-to-new IDs used during one atomic compaction attempt.
pub(super) struct Remapping {
    /// Old node page ID to new page ID plus one (zero means not copied).
    pub nodes: File,
    /// Old text block to new byte offset plus one; None means identity mapping.
    pub text: Option<File>,
}
/// One transient decoded page; at most a bounded path and sibling set is resident.
struct Node {
    height: u16,
    entries: Vec<Entry>,
}
/// Append-only node arena. Unpublished pages may be reclaimed by compaction.
pub(super) struct Tree {
    file: File,
    pub pages: u64,
    page_size: usize,
    capacity: usize,
    generation: u64,
    cache: Arc<Mutex<Cache>>,
    pub(super) uncached: bool,
    pub(super) stream_reader: bool,
}
impl Tree {
    /// Create an empty arena; each page contains a header and 24-byte entries.
    pub fn new(file: File, page_size: usize, generation: u64, cache: Arc<Mutex<Cache>>) -> Self {
        let capacity = ((page_size - 32) / 24) & !1;
        Self {
            file,
            pages: 0,
            page_size,
            capacity,
            generation,
            cache,
            uncached: false,
            stream_reader: false,
        }
    }
    /// Independent reader of this already-published immutable page prefix.
    pub fn reader(&self) -> Result<Self, Error> {
        Ok(Self {
            file: self.file.try_clone()?,
            pages: self.pages,
            page_size: self.page_size,
            capacity: self.capacity,
            generation: self.generation,
            cache: self.cache.clone(),
            uncached: self.uncached,
            stream_reader: self.stream_reader,
        })
    }
    /// A background copy must never hold the foreground cache mutex during filesystem I/O.
    pub(super) fn uncached_reader(&self) -> Result<Self, Error> {
        let mut reader = self.reader()?;
        reader.uncached = true;
        Ok(reader)
    }
    /// Fork immutable node IDs into a private writable generation using bounded storage buffers.
    pub(super) fn fork_to(
        &self,
        file: File,
        source: &Source,
        cancel: &Cancellation,
    ) -> Result<Self, Error> {
        let _permit = source
            .resources
            .buffers
            .acquire(source.limits.text_page_bytes)?;
        source.reserve(self.disk_bytes())?;
        let mut bytes = vec![0; source.limits.text_page_bytes];
        super::platform::copy_prefix(
            &self.file,
            &file,
            self.disk_bytes(),
            &mut bytes,
            cancel,
            source.faults.as_deref(),
        )?;
        let mut tree = Self::new(
            file,
            self.page_size,
            source.generation.0,
            source.resources.nodes.clone(),
        );
        tree.pages = self.pages;
        tree.uncached = true;
        Ok(tree)
    }
    /// Allocated disk bytes, including not-yet-reclaimed old versions.
    pub fn disk_bytes(&self) -> u64 {
        self.pages * self.page_size as u64
    }
    /// Initialize one range, including the empty leaf used by new documents.
    pub fn initial(&mut self, piece: Entry, source: &Source) -> Result<Root, Error> {
        self.write(
            Node {
                height: 0,
                entries: if piece.bytes == 0 {
                    vec![]
                } else {
                    vec![piece]
                },
            },
            source,
        )
    }
    /// Read and validate a disk page, its aggregate weights, and height.
    fn read(&self, root: Root) -> Result<Node, Error> {
        if self.stream_reader
            && let Some(piece) = root.stream_piece()
        {
            return Ok(Node {
                height: 0,
                entries: (piece.bytes != 0).then_some(piece).into_iter().collect(),
            });
        }
        if root.entry.id >= self.pages {
            return Err(Error::CorruptStorage);
        }
        let mut page = vec![0; self.page_size];
        if self.uncached {
            self.file
                .read_exact_at(&mut page, root.entry.id * self.page_size as u64)?;
        } else {
            self.cache.lock().map_err(|_| Error::CorruptStorage)?.read(
                Key(self.generation, 1, root.entry.id),
                &self.file,
                root.entry.id * self.page_size as u64,
                self.page_size,
                &mut page,
            )?;
        }
        if &page[..8] != b"EDEPAGE2" {
            return Err(Error::CorruptStorage);
        }
        let height = u16::from_le_bytes([page[8], page[9]]);
        let count = u16::from_le_bytes([page[10], page[11]]) as usize;
        if height != root.height || count > self.capacity || (height > 0 && count == 0) {
            return Err(Error::CorruptStorage);
        }
        let mut entries = Vec::with_capacity(count);
        for chunk in page[32..32 + count * 24].as_chunks::<24>().0.iter() {
            let entry = Entry {
                id: word(chunk, 0),
                bytes: word(chunk, 8),
                lines: word(chunk, 16),
            };
            if entry.bytes == 0
                || entry.lines > entry.bytes
                || (height == 0 && entry.id.checked_add(entry.bytes).is_none())
            {
                return Err(Error::CorruptStorage);
            }
            entries.push(entry);
        }
        let sum = totals(&entries)?;
        if sum.bytes != root.entry.bytes || sum.lines != root.entry.lines {
            return Err(Error::CorruptStorage);
        }
        Ok(Node { height, entries })
    }
    /// Append a fully serialized node before making its ID reachable by a caller.
    fn write(&mut self, node: Node, source: &Source) -> Result<Root, Error> {
        if node.entries.len() > self.capacity {
            return Err(Error::CorruptStorage);
        }
        source.check(IoPoint::Write)?;
        let mut page = vec![0; self.page_size];
        page[..8].copy_from_slice(b"EDEPAGE2");
        page[8..10].copy_from_slice(&node.height.to_le_bytes());
        page[10..12].copy_from_slice(&(node.entries.len() as u16).to_le_bytes());
        for (entry, chunk) in node
            .entries
            .iter()
            .zip(page[32..].as_chunks_mut::<24>().0.iter_mut())
        {
            chunk[..8].copy_from_slice(&entry.id.to_le_bytes());
            chunk[8..16].copy_from_slice(&entry.bytes.to_le_bytes());
            chunk[16..24].copy_from_slice(&entry.lines.to_le_bytes());
        }
        let mut entry = totals(&node.entries)?;
        entry.id = self.pages;
        source.reserve(self.page_size as u64)?;
        self.file.write_all_at(&page, self.disk_bytes())?;
        self.pages = self
            .pages
            .checked_add(1)
            .ok_or(Error::ResourceLimit("tree page IDs"))?;
        Ok(Root {
            entry,
            height: node.height,
        })
    }
    /// Find a piece and its logical origin in logarithmic tree depth.
    pub fn piece_at(&self, root: Root, offset: u64) -> Result<(Entry, u64), Error> {
        if offset >= root.entry.bytes {
            return Err(Error::InvalidRange);
        }
        let mut node = root;
        let mut origin: u64 = 0;
        loop {
            let decoded = self.read(node)?;
            let mut found = None;
            for entry in decoded.entries {
                let end = origin
                    .checked_add(entry.bytes)
                    .ok_or(Error::CorruptStorage)?;
                if offset < end {
                    found = Some(entry);
                    break;
                }
                origin = end;
            }
            let entry = found.ok_or(Error::CorruptStorage)?;
            if node.height == 0 {
                return Ok((entry, origin));
            }
            node = Root {
                entry,
                height: node.height - 1,
            };
        }
    }
    /// Validate a retained descriptor before publishing it as current content.
    pub fn check_root(&self, root: Root) -> Result<(), Error> {
        self.read(root).map(|_| ())
    }
    /// Count line endings before a logical byte offset without indexing each line.
    pub fn lines_before(&self, root: Root, offset: u64, source: &Source) -> Result<u64, Error> {
        if offset > root.entry.bytes {
            return Err(Error::InvalidRange);
        }
        let mut node = root;
        let mut remaining = offset;
        let mut lines = 0;
        loop {
            let decoded = self.read(node)?;
            let mut next = None;
            for entry in decoded.entries {
                if remaining >= entry.bytes {
                    remaining -= entry.bytes;
                    lines += entry.lines;
                } else {
                    next = Some(entry);
                    break;
                }
            }
            let Some(entry) = next else {
                return Ok(lines);
            };
            if node.height == 0 {
                return Ok(lines + source.lines(entry.id, remaining)?);
            }
            node = Root {
                entry,
                height: node.height - 1,
            };
        }
    }
    /// Find the byte immediately after the requested zero-based LF.
    pub fn after_lf(
        &self,
        root: Root,
        mut lf: u64,
        source: &Source,
        cancel: &Cancellation,
    ) -> Result<u64, Error> {
        if lf >= root.entry.lines {
            return Err(Error::InvalidRange);
        }
        let mut node = root;
        let mut offset = 0;
        loop {
            cancel.check()?;
            let decoded = self.read(node)?;
            let mut next = None;
            for entry in decoded.entries {
                if lf >= entry.lines {
                    lf -= entry.lines;
                    offset += entry.bytes;
                } else {
                    next = Some(entry);
                    break;
                }
            }
            let entry = next.ok_or(Error::CorruptStorage)?;
            if node.height == 0 {
                return Ok(offset + source.after_lf(entry.id, entry.bytes, lf, cancel)?);
            }
            node = Root {
                entry,
                height: node.height - 1,
            };
        }
    }
    /// Replace a checked range. No root is published if any append or validation fails.
    pub fn replace(
        &mut self,
        root: Root,
        start: u64,
        end: u64,
        piece: Option<Entry>,
        source: &Source,
        cancel: &Cancellation,
    ) -> Result<Root, Error> {
        let mut insertion = piece;
        let mut roots = self.splice(root, start, end, &mut insertion, source, cancel)?;
        let mut result = match roots.len() {
            0 => return self.initial(Entry::default(), source),
            1 => roots.remove(0),
            _ => self.write(
                Node {
                    height: root.height.checked_add(1).ok_or(Error::CorruptStorage)?,
                    entries: roots.into_iter().map(|r| r.entry).collect(),
                },
                source,
            )?,
        };
        while result.height > 0 {
            let node = self.read(result)?;
            if node.entries.len() != 1 {
                break;
            }
            result = Root {
                entry: node.entries[0],
                height: result.height - 1,
            };
        }
        Ok(result)
    }
    /// Recursive splice returns zero, one or two same-height nodes to its parent.
    fn splice(
        &mut self,
        root: Root,
        start: u64,
        end: u64,
        insertion: &mut Option<Entry>,
        source: &Source,
        cancel: &Cancellation,
    ) -> Result<Vec<Root>, Error> {
        cancel.check()?;
        let node = self.read(root)?;
        let mut output = Vec::with_capacity(self.capacity + 2);
        let mut position: u64 = 0;
        if root.height == 0 {
            for entry in node.entries {
                let stop = position + entry.bytes;
                if position < start {
                    self.push_piece(
                        &mut output,
                        slice(entry, 0, (start - position).min(entry.bytes), source)?,
                    );
                }
                if start <= stop && insertion.is_some() {
                    self.push_piece(&mut output, insertion.take().expect("checked insertion"));
                }
                if stop > end {
                    self.push_piece(
                        &mut output,
                        slice(
                            entry,
                            end.saturating_sub(position),
                            stop - end.max(position),
                            source,
                        )?,
                    );
                }
                position = stop;
            }
            if let Some(entry) = insertion.take() {
                self.push_piece(&mut output, entry);
            }
        } else {
            for entry in node.entries {
                let stop = position + entry.bytes;
                let includes_insert = insertion.is_some() && start >= position && start <= stop;
                let overlap = start < stop && end > position;
                if !overlap && !includes_insert {
                    output.push(entry);
                } else if !(start <= position && end >= stop && !includes_insert) {
                    let child = Root {
                        entry,
                        height: root.height - 1,
                    };
                    output.extend(
                        self.splice(
                            child,
                            start.saturating_sub(position).min(entry.bytes),
                            end.saturating_sub(position).min(entry.bytes),
                            insertion,
                            source,
                            cancel,
                        )?
                        .into_iter()
                        .map(|r| r.entry),
                    );
                }
                position = stop;
            }
            self.repair(&mut output, root.height - 1, source)?;
        }
        self.pack(root.height, output, source)
    }
    /// Coalesce contiguous immutable source ranges at leaf construction boundaries.
    fn push_piece(&self, output: &mut Vec<Entry>, entry: Entry) {
        if entry.bytes == 0 {
            return;
        }
        if let Some(last) = output.last_mut()
            && last.id.checked_add(last.bytes) == Some(entry.id)
        {
            last.bytes += entry.bytes;
            last.lines += entry.lines;
            return;
        }
        output.push(entry);
    }
    /// Restore minimum occupancy by merging/redistributing adjacent sibling contents.
    fn repair(
        &mut self,
        children: &mut Vec<Entry>,
        height: u16,
        source: &Source,
    ) -> Result<(), Error> {
        let mut i = 0;
        while children.len() > 1 && i < children.len() {
            let node = self.read(Root {
                entry: children[i],
                height,
            })?;
            if node.entries.len() >= self.capacity / 2 {
                i += 1;
                continue;
            }
            let left = if i == 0 { 0 } else { i - 1 };
            let a = self.read(Root {
                entry: children[left],
                height,
            })?;
            let b = self.read(Root {
                entry: children[left + 1],
                height,
            })?;
            let mut entries = a.entries;
            if height == 0 {
                for entry in b.entries {
                    self.push_piece(&mut entries, entry);
                }
            } else {
                entries.extend(b.entries);
                self.repair(&mut entries, height - 1, source)?;
            }
            let packed = self.pack(height, entries, source)?;
            children.splice(left..left + 2, packed.into_iter().map(|r| r.entry));
            i = left.saturating_sub(1);
        }
        Ok(())
    }
    /// Split at the midpoint so both non-root pages satisfy minimum occupancy.
    fn pack(
        &mut self,
        height: u16,
        mut entries: Vec<Entry>,
        source: &Source,
    ) -> Result<Vec<Root>, Error> {
        if entries.is_empty() {
            return Ok(vec![]);
        }
        if entries.len() <= self.capacity {
            return Ok(vec![self.write(Node { height, entries }, source)?]);
        }
        let right = entries.split_off(entries.len() / 2);
        Ok(vec![
            self.write(Node { height, entries }, source)?,
            self.write(
                Node {
                    height,
                    entries: right,
                },
                source,
            )?,
        ])
    }
    /// Traverse a root while allowing shared/already-accounted subtrees to be skipped.
    pub fn visit(
        &self,
        root: Root,
        cancel: &Cancellation,
        admit: &mut impl FnMut(u64) -> Result<bool, Error>,
        piece: &mut impl FnMut(Entry) -> Result<(), Error>,
    ) -> Result<(), Error> {
        cancel.check()?;
        if !admit(root.entry.id)? {
            return Ok(());
        }
        let node = self.read(root)?;
        if root.height == 0 {
            for entry in node.entries {
                piece(entry)?;
            }
        } else {
            for entry in node.entries {
                self.visit(
                    Root {
                        entry,
                        height: root.height - 1,
                    },
                    cancel,
                    admit,
                    piece,
                )?;
            }
        }
        Ok(())
    }
    /// Copy each reachable immutable node once, preserving B+ shape and shared history paths.
    /// Text blocks are repacked in physical order, so every old piece remains one range.
    pub fn copy_live(
        &self,
        root: Root,
        target: &mut Tree,
        source: &Source,
        maps: &Remapping,
        cancel: &Cancellation,
    ) -> Result<Root, Error> {
        cancel.check()?;
        let mut encoded = [0; 8];
        maps.nodes.read_exact_at(
            &mut encoded,
            root.entry.id.checked_mul(8).ok_or(Error::CorruptStorage)?,
        )?;
        let mapped = u64::from_le_bytes(encoded);
        if mapped != 0 {
            let result = Root {
                entry: Entry {
                    id: mapped - 1,
                    ..root.entry
                },
                height: root.height,
            };
            target.read(result)?;
            return Ok(result);
        }
        let mut node = self.read(root)?;
        for entry in &mut node.entries {
            if root.height > 0 {
                *entry = self
                    .copy_live(
                        Root {
                            entry: *entry,
                            height: root.height - 1,
                        },
                        target,
                        source,
                        maps,
                        cancel,
                    )?
                    .entry;
            } else if let Some(text) = &maps.text {
                let page = source.limits.text_page_bytes as u64;
                text.read_exact_at(
                    &mut encoded,
                    (entry.id / page)
                        .checked_mul(8)
                        .ok_or(Error::CorruptStorage)?,
                )?;
                let location = u64::from_le_bytes(encoded);
                if location == 0 {
                    return Err(Error::CorruptStorage);
                }
                entry.id = (location - 1)
                    .checked_add(entry.id % page)
                    .ok_or(Error::CorruptStorage)?;
                if entry
                    .id
                    .checked_add(entry.bytes)
                    .is_none_or(|end| end > source.len())
                {
                    return Err(Error::CorruptStorage);
                }
            }
        }
        let result = target.write(node, source)?;
        maps.nodes
            .write_all_at(&(result.entry.id + 1).to_le_bytes(), root.entry.id * 8)?;
        Ok(result)
    }
    /// Verify occupancy, source bounds and weights recursively; used by integrity tests.
    #[cfg(test)]
    pub fn validate(&self, root: Root, source: &Source, is_root: bool) -> Result<(), Error> {
        let node = self.read(root)?;
        if !is_root && node.entries.len() < self.capacity / 2 {
            return Err(Error::CorruptStorage);
        }
        if is_root && root.height > 0 && node.entries.len() < 2 {
            return Err(Error::CorruptStorage);
        }
        for entry in node.entries {
            if root.height == 0 {
                if entry
                    .id
                    .checked_add(entry.bytes)
                    .is_none_or(|n| n > source.len())
                    || entry.lines != source.lines(entry.id, entry.bytes)?
                {
                    return Err(Error::CorruptStorage);
                }
            } else {
                self.validate(
                    Root {
                        entry,
                        height: root.height - 1,
                    },
                    source,
                    false,
                )?;
            }
        }
        Ok(())
    }
}
/// Decode one fixed-width integer from a validated page region.
fn word(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("fixed-width region"),
    )
}
/// Aggregate checked byte/LF weights; page ID is assigned by the caller.
fn totals(entries: &[Entry]) -> Result<Entry, Error> {
    entries.iter().try_fold(Entry::default(), |mut sum, entry| {
        sum.bytes = sum
            .bytes
            .checked_add(entry.bytes)
            .ok_or(Error::ResourceLimit("document length overflow"))?;
        sum.lines = sum
            .lines
            .checked_add(entry.lines)
            .ok_or(Error::CorruptStorage)?;
        Ok(sum)
    })
}
/// Preserve physical source coordinates while cutting a leaf piece.
fn slice(entry: Entry, start: u64, len: u64, source: &Source) -> Result<Entry, Error> {
    let id = entry.id.checked_add(start).ok_or(Error::CorruptStorage)?;
    Ok(Entry {
        id,
        bytes: len,
        lines: source.lines(id, len)?,
    })
}
