//! Disk-paged undo roots. Append-only descriptors keep failed edits from destroying redo.

use super::{
    Error, Format, Newline,
    tree::{Entry, Root},
};
use std::{fs::File, os::unix::fs::FileExt};

/// Semantic content identity, independent of its published revision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct State {
    pub root: Root,
    pub format: Format,
    pub identity: u64,
}
/// Append-only root records linked backwards; logical branches have bounded depth.
pub(super) struct History {
    file: File,
    pub begin: u64,
    pub cursor: u64,
    pub end: u64,
}
impl History {
    /// Start history at one accepted state.
    pub fn new(file: File, state: State) -> Result<Self, Error> {
        let history = Self {
            file,
            begin: 0,
            cursor: 0,
            end: 0,
        };
        history.write(0, state, 0)?;
        Ok(history)
    }
    /// Read a validated immutable descriptor and its predecessor.
    fn record(&self, index: u64) -> Result<(State, u64), Error> {
        let mut bytes = [0; 64];
        self.file.read_exact_at(
            &mut bytes,
            index.checked_mul(64).ok_or(Error::CorruptStorage)?,
        )?;
        if &bytes[..8] != b"EDEHIST2" {
            return Err(Error::CorruptStorage);
        }
        let word = |offset| {
            u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed record"))
        };
        let format = match (bytes[42], bytes[43]) {
            (bom @ 0..=1, newline @ 0..=1) => Format {
                bom: bom == 1,
                newline: if newline == 0 {
                    Newline::Lf
                } else {
                    Newline::CrLf
                },
            },
            _ => return Err(Error::CorruptStorage),
        };
        let previous = word(48);
        if previous >= index && index != 0 {
            return Err(Error::CorruptStorage);
        }
        Ok((
            State {
                root: Root {
                    entry: Entry {
                        id: word(8),
                        bytes: word(16),
                        lines: word(24),
                    },
                    height: u16::from_le_bytes([bytes[40], bytes[41]]),
                },
                identity: word(32),
                format,
            },
            previous,
        ))
    }
    /// Decode a retained state. Logical liveness is owned by the document.
    pub fn read(&self, index: u64) -> Result<State, Error> {
        Ok(self.record(index)?.0)
    }
    /// Previous undo root, with a checked backwards link.
    pub fn previous(&self, index: u64) -> Result<u64, Error> {
        Ok(self.record(index)?.1)
    }
    /// Find the next redo root on the current branch; history length is bounded.
    pub fn successor(&self, cursor: u64) -> Result<u64, Error> {
        let mut next = self.end;
        while next != self.begin {
            let previous = self.previous(next)?;
            if previous == cursor {
                return Ok(next);
            }
            next = previous;
        }
        Err(Error::CorruptStorage)
    }
    /// Serialize a complete descriptor before its new root becomes visible.
    fn write(&self, index: u64, state: State, previous: u64) -> Result<(), Error> {
        let mut bytes = [0; 64];
        bytes[..8].copy_from_slice(b"EDEHIST2");
        for (offset, value) in [
            (8, state.root.entry.id),
            (16, state.root.entry.bytes),
            (24, state.root.entry.lines),
            (32, state.identity),
            (48, previous),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[40..42].copy_from_slice(&state.root.height.to_le_bytes());
        bytes[42] = u8::from(state.format.bom);
        bytes[43] = u8::from(state.format.newline == Newline::CrLf);
        self.file.write_all_at(
            &bytes,
            index
                .checked_mul(64)
                .ok_or(Error::ResourceLimit("history offset overflow"))?,
        )?;
        Ok(())
    }
    /// Append rather than overwrite redo, including after a prior partial write failure.
    pub fn stage(&self, state: State) -> Result<u64, Error> {
        let next = self.file.metadata()?.len().div_ceil(64);
        self.write(next, state, self.cursor)?;
        Ok(next)
    }
    /// Publish a staged record and prune whole oldest transactions.
    pub fn publish(&mut self, next: u64, begin: u64) {
        self.cursor = next;
        self.end = next;
        self.begin = begin;
    }
    /// Count pruned roots before changing the retained oldest root.
    pub fn pruned_count(&self, begin: u64) -> Result<u64, Error> {
        let mut count = 0;
        let mut index = begin;
        while index != self.begin {
            index = self.previous(index)?;
            count += 1;
        }
        Ok(count)
    }
    /// Report physical history bytes, including unreachable branch records until compaction.
    pub fn disk_bytes(&self) -> Result<u64, Error> {
        Ok(self.file.metadata()?.len())
    }
}

use super::{Cancellation, model::Document, source::Store};

impl Document {
    /// Admit newest undo first, then older roots, counting unique non-current physical pages.
    pub(super) fn history_budget(
        &self,
        store: &Store,
        candidate: State,
        cancel: &Cancellation,
    ) -> Result<u64, Error> {
        let bitmap = tempfile::tempfile_in(store.directory.path())?;
        let page = store.source.limits.text_page_bytes as u64;
        bitmap.set_len(store.source.length.div_ceil(page))?;
        let mut offset = 0;
        while offset < candidate.root.entry.bytes {
            cancel.check()?;
            let (piece, _) = store.tree.piece_at(candidate.root, offset)?;
            let first = piece.id.div_ceil(page);
            let last = (piece.id + piece.bytes) / page;
            bitmap_range(&bitmap, first, last, 1, 0)?;
            offset += piece.bytes;
        }
        let mut retained = 0u64;
        let mut count = 0;
        let mut index = self.history.cursor;
        let mut begin = index;
        loop {
            cancel.check()?;
            let state = self.history.read(index)?;
            let mut offset = 0;
            while offset < state.root.entry.bytes {
                let (piece, _) = store.tree.piece_at(state.root, offset)?;
                let pages = bitmap_range(
                    &bitmap,
                    piece.id / page,
                    (piece.id + piece.bytes).div_ceil(page),
                    2,
                    3,
                )?;
                retained = retained
                    .checked_add(pages.saturating_mul(page))
                    .ok_or(Error::ResourceLimit("undo accounting overflow"))?;
                offset += piece.bytes;
            }
            if retained > store.source.limits.undo_bytes {
                if count == 0 {
                    return Err(Error::ResourceLimit(
                        "newest transaction exceeds undo budget",
                    ));
                }
                break;
            }
            begin = index;
            count += 1;
            if count >= store.source.limits.undo_transactions || index == self.history.begin {
                break;
            }
            index = self.history.previous(index)?;
        }
        Ok(begin)
    }
}
/// Bounded disk bitmap range update. Count newly marked bytes not covered by skip bits.
fn bitmap_range(file: &File, mut start: u64, end: u64, mark: u8, skip: u8) -> Result<u64, Error> {
    let mut buffer = [0u8; 4096];
    let mut changed = 0;
    while start < end {
        let count = (end - start).min(buffer.len() as u64) as usize;
        file.read_exact_at(&mut buffer[..count], start)?;
        for byte in &mut buffer[..count] {
            if *byte & skip == 0 {
                *byte |= mark;
                changed += 1;
            }
        }
        file.write_all_at(&buffer[..count], start)?;
        start += count as u64;
    }
    Ok(changed)
}
