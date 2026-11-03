//! Cancellable generation compaction using a disk page-remapping table.
use super::{
    Cancellation, Error, OpenOptions,
    history::{History, State},
    model::Document,
    source::{Store, private_file},
    tree::Entry,
};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
};

impl Document {
    /// Incrementally copy live ranges into a fresh private generation, preserving
    /// every retained history/save state. External leases require an explicit retry.
    /// Cancellation/failure leaves the old generation and all roots unchanged.
    pub fn compact(&mut self, cancel: &Cancellation) -> Result<(), Error> {
        if self.leases.load(Ordering::Acquire) != 0 {
            return Err(Error::Busy);
        }
        let old_arc = self.store.clone();
        let old = old_arc.lock().map_err(|_| Error::CorruptStorage)?;
        let _buffers = old.source.resources.buffers.acquire(
            old.source.limits.text_page_bytes * 6 + old.source.limits.node_page_bytes * 8,
        )?;
        let options = OpenOptions {
            limits: old.source.limits.clone(),
            working_directory: old.directory.path().parent().map(Path::to_owned),
            force_copy: true,
            faults: old.source.faults.clone(),
        };
        old.source.reserve(old.source.length)?;
        let (mut fresh, _) = Store::empty(&options, old.source.resources.clone(), None)?;
        fresh.original_longest = old.original_longest;
        let mapping = tempfile::tempfile_in(fresh.directory.path())?;
        mapping.set_len(
            old.source
                .length
                .div_ceil(old.source.limits.text_page_bytes as u64)
                .checked_mul(8)
                .ok_or(Error::ResourceLimit("compaction map overflow"))?,
        )?;
        let first = self.history.read(self.history.begin)?;
        let first = compact_state(&old, &mut fresh, &mapping, first, cancel)?;
        let mut history = History::new(
            private_file(&fresh.directory.path().join("history"))?,
            first,
        )?;
        let mut old_index = self.history.begin;
        let mut cursor = if old_index == self.history.cursor {
            Some(0)
        } else {
            None
        };
        let mut current = first;
        while old_index != self.history.end {
            cancel.check()?;
            old_index = self.history.successor(old_index)?;
            let state = compact_state(
                &old,
                &mut fresh,
                &mapping,
                self.history.read(old_index)?,
                cancel,
            )?;
            let next = history.stage(state)?;
            history.publish(next, 0);
            if old_index == self.history.cursor {
                cursor = Some(next);
                current = state;
            }
        }
        history.cursor = cursor.ok_or(Error::CorruptStorage)?;
        if self.history.cursor == self.history.begin {
            current = first;
        }
        let mut baseline = self.baseline.clone();
        if let Some(saved) = &mut baseline {
            saved.state = compact_state(&old, &mut fresh, &mapping, saved.state, cancel)?;
        }
        cancel.check()?;
        self.store = Arc::new(Mutex::new(fresh));
        self.current = current;
        self.baseline = baseline;
        self.history = history;
        Ok(())
    }
}
/// Translate one root with a disk remapping table, copying each live physical page once.
fn compact_state(
    old: &Store,
    fresh: &mut Store,
    mapping: &File,
    state: State,
    cancel: &Cancellation,
) -> Result<State, Error> {
    let mut root = fresh.tree.initial(Entry::default(), &fresh.source)?;
    let page = old.source.limits.text_page_bytes as u64;
    let mut offset = 0;
    let mut logical_end = 0;
    let mut pending: Option<Entry> = None;
    let mut buffer = vec![0; page as usize];
    while offset < state.root.entry.bytes {
        let (piece, _) = old.tree.piece_at(state.root, offset)?;
        let mut physical = piece.id;
        let mut remaining = piece.bytes;
        while remaining != 0 {
            cancel.check()?;
            let block = physical / page;
            let within = physical % page;
            let count = remaining.min(page - within);
            let mut mapped = [0; 8];
            mapping.read_exact_at(&mut mapped, block * 8)?;
            let mut new_offset = u64::from_le_bytes(mapped);
            if new_offset == 0 {
                let start = block * page;
                let available = (old.source.length - start).min(page) as usize;
                buffer.fill(0);
                old.source.read(start, &mut buffer[..available])?;
                new_offset = fresh
                    .source
                    .length
                    .checked_add(1)
                    .ok_or(Error::ResourceLimit("compaction offset overflow"))?;
                fresh.source.reserve(page)?;
                fresh.source.append(&buffer, false)?;
                mapping.write_all_at(&new_offset.to_le_bytes(), block * 8)?;
            }
            let entry = Entry {
                id: new_offset - 1 + within,
                bytes: count,
                lines: old.source.lines(physical, count)?,
            };
            if let Some(previous) = &mut pending {
                if previous.id + previous.bytes == entry.id {
                    previous.bytes += entry.bytes;
                    previous.lines += entry.lines;
                } else {
                    root = fresh.tree.replace(
                        root,
                        logical_end,
                        logical_end,
                        Some(*previous),
                        &fresh.source,
                        cancel,
                    )?;
                    logical_end += previous.bytes;
                    pending = Some(entry);
                }
            } else {
                pending = Some(entry);
            }
            physical += count;
            remaining -= count;
        }
        offset += piece.bytes;
    }
    if let Some(entry) = pending {
        root = fresh.tree.replace(
            root,
            logical_end,
            logical_end,
            Some(entry),
            &fresh.source,
            cancel,
        )?;
    }
    Ok(State { root, ..state })
}
