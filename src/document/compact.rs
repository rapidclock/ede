//! Atomic generation compaction: mark shared roots, pack live text pages in physical
//! order, and copy each tree node once. Never replay retained edits to rebuild history.

use super::{
    Cancellation, Error, OpenOptions,
    bitmap::Bitmap,
    history::{History, State},
    model::Document,
    source::{Store, private_file},
    tree::Remapping,
};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
};

impl Document {
    /// Reclaim unreachable text, nodes and history without materializing the document.
    /// External leases require retry; cancellation/failure preserves the old generation.
    pub fn compact(&mut self, cancel: &Cancellation) -> Result<(), Error> {
        if self.blocking_leases.load(Ordering::Acquire) != 0 {
            return Err(Error::Busy);
        }
        let old_arc = self.store.clone();
        let old = old_arc.lock().map_err(|_| Error::CorruptStorage)?;
        let _buffers = old.source.resources.buffers.acquire(
            old.source.limits.text_page_bytes * 6
                + old.source.limits.node_page_bytes * 8
                + (128 << 10),
        )?;
        let options = OpenOptions {
            limits: old.source.limits.clone(),
            working_directory: old.directory.path().parent().map(Path::to_owned),
            force_copy: true,
            faults: old.source.faults.clone(),
        };
        let (mut fresh, _) = Store::empty(&options, old.source.resources.clone(), None)?;
        fresh.source.uncached = old.source.uncached;
        fresh.tree.uncached = old.tree.uncached;
        fresh.original_longest = old.original_longest;
        fresh.cloned = old.cloned;
        let page = old.source.limits.text_page_bytes as u64;
        let blocks = old.source.len().div_ceil(page);
        let mut text_live = Bitmap::new(fresh.directory.path(), blocks)?;
        let mut nodes_live = Bitmap::new(fresh.directory.path(), old.tree.pages)?;
        let mut text_count = 0;
        let mut mark = |state: State| -> Result<(), Error> {
            old.tree.visit(
                state.root,
                cancel,
                &mut |id| Ok(nodes_live.mark(id, id + 1, 1, 1)? != 0),
                &mut |piece| {
                    text_count += text_live.mark(
                        piece.id / page,
                        (piece.id + piece.bytes).div_ceil(page),
                        1,
                        1,
                    )?;
                    Ok(())
                },
            )
        };
        let mut index = self.history.end;
        loop {
            mark(self.history.read(index)?)?;
            if index == self.history.begin {
                break;
            }
            index = self.history.previous(index)?;
        }
        if let Some(saved) = &self.baseline {
            mark(saved.state)?;
        }
        let nodes = tempfile::tempfile_in(fresh.directory.path())?;
        nodes.set_len(
            old.tree
                .pages
                .checked_mul(8)
                .ok_or(Error::ResourceLimit("node remapping overflow"))?,
        )?;
        let text = if text_count == blocks {
            // No text holes: retain immutable prefixes through duplicated handles. The old
            // directory can be unlinked after the swap; these live handles own the bytes.
            // This also avoids copying a GiB of live text just to collect dead tree pages.
            fresh.source = old.source.reader()?;
            None
        } else {
            let mapping = tempfile::tempfile_in(fresh.directory.path())?;
            mapping.set_len(
                blocks
                    .checked_mul(8)
                    .ok_or(Error::ResourceLimit("text remapping overflow"))?,
            )?;
            fresh.source.reserve(
                text_count
                    .checked_mul(page)
                    .ok_or(Error::ResourceLimit("compaction size overflow"))?,
            )?;
            pack_text(&old, &mut fresh, &mut text_live, &mapping, cancel)?;
            Some(mapping)
        };
        let maps = Remapping { nodes, text };
        let first = copy_state(
            &old,
            &mut fresh,
            &maps,
            self.history.read(self.history.begin)?,
            cancel,
        )?;
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
            let state = copy_state(
                &old,
                &mut fresh,
                &maps,
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
        let mut baseline = self.baseline.clone();
        if let Some(saved) = &mut baseline {
            saved.state = copy_state(&old, &mut fresh, &maps, saved.state, cancel)?;
        }
        cancel.check()?;
        self.store = Arc::new(Mutex::new(fresh));
        self.current = current;
        self.baseline = baseline;
        self.history = history;
        Ok(())
    }
}
/// Pack marked source blocks in their original physical order. A piece's full block
/// interval is marked live, so it remains physically contiguous after remapping.
fn pack_text(
    old: &Store,
    fresh: &mut Store,
    live: &mut Bitmap,
    mapping: &File,
    cancel: &Cancellation,
) -> Result<(), Error> {
    let page = old.source.limits.text_page_bytes as u64;
    let mut buffer = vec![0; page as usize];
    for block in 0..old.source.len().div_ceil(page) {
        cancel.check()?;
        if live.get(block)? == 0 {
            continue;
        }
        let start = block * page;
        let count = (old.source.len() - start).min(page) as usize;
        buffer.fill(0);
        old.source.read(start, &mut buffer[..count])?;
        let offset = fresh
            .source
            .len()
            .checked_add(1)
            .ok_or(Error::ResourceLimit("compaction offset overflow"))?;
        fresh.source.append(&buffer, false)?;
        mapping.write_all_at(&offset.to_le_bytes(), block * 8)?;
    }
    Ok(())
}
/// Remap one semantic state, preserving its identity, format, weights and shared tree pages.
fn copy_state(
    old: &Store,
    fresh: &mut Store,
    maps: &Remapping,
    state: State,
    cancel: &Cancellation,
) -> Result<State, Error> {
    Ok(State {
        root: old
            .tree
            .copy_live(state.root, &mut fresh.tree, &fresh.source, maps, cancel)?,
        ..state
    })
}
