//! Off-owner batch application with a short revision-guarded generation swap.
use super::{
    Cancellation, Document, DocumentInfo, Error, OpenOptions, Snapshot, StagedTransaction,
    history::History,
    model::Baseline,
    source::{Store, private_file},
};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicUsize},
};

/// Immutable branch descriptors captured by the owner before expensive work.
pub struct EditTask {
    before: Snapshot,
    baseline: Option<Baseline>,
    history: History,
    options: OpenOptions,
    attention: bool,
    pruned: u64,
}

/// Fully applied candidate; no live document changes until `finish_edit`.
pub struct PreparedEdit {
    data: Option<Candidate>,
    cleanup: Arc<super::cleanup::Cleanup>,
}
/// The intact bundle retains the old generation after publication until worker disposal.
struct Candidate {
    before: Snapshot,
    candidate: Box<Document>,
    mapping: Option<File>,
    length: u64,
    count: u64,
    changed: bool,
    pruned: u64,
    cancel: Cancellation,
    publish: bool,
    compacted: bool,
    baseline: Option<Baseline>,
}
impl super::cleanup::Garbage for Candidate {
    fn observe(&self) {
        let _ = self.before.reader.source.check(super::IoPoint::Cleanup);
    }
}
impl Drop for PreparedEdit {
    fn drop(&mut self) {
        if let Some(data) = self.data.take() {
            self.cleanup.retire(data);
        }
    }
}

/// Exact acknowledged metadata; no fallible post-publication lookup is required.
pub struct EditReport {
    /// Resulting live document metadata.
    pub info: DocumentInfo,
    /// Whether text changed; false for identical batches.
    pub changed: bool,
    /// Whole old history transactions pruned to preserve the newest edit.
    pub history_pruned: u64,
}

impl EditTask {
    /// Copy captured prefixes without taking the live owner's store lock.
    fn fork_document(&self, cancel: &Cancellation) -> Result<Box<Document>, Error> {
        let store = Store::fork(&self.before.reader, &self.options, cancel, |_, _| {})?;
        let history = self.history.fork_to(
            private_file(&store.directory.path().join("history"))?,
            &store.source,
            cancel,
        )?;
        Ok(Box::new(Document {
            store: Arc::new(Mutex::new(store)),
            current: *self.before.state,
            baseline: self.baseline.clone(),
            attention: self.attention,
            history,
            id: self.before.document(),
            revision: self.before.revision(),
            leases: Arc::new(AtomicUsize::new(0)),
            blocking_leases: Arc::new(AtomicUsize::new(0)),
            pruned: self.pruned,
        }))
    }
    /// Reclaim a private generation without changing logical text, revision or undo.
    /// Publication separately checks the saved baseline and live blocking leases.
    pub fn compact(self, cancel: &Cancellation) -> Result<PreparedEdit, Error> {
        let mut candidate = self.fork_document(cancel)?;
        candidate.compact(cancel)?;
        let cleanup = self.before.reader.source.resources.cleanup.clone();
        Ok(PreparedEdit {
            cleanup,
            data: Some(Candidate {
                before: self.before,
                candidate,
                mapping: None,
                length: 0,
                count: 0,
                changed: false,
                pruned: 0,
                cancel: cancel.clone(),
                publish: true,
                compacted: true,
                baseline: self.baseline,
            }),
        })
    }
    /// Captured text used by the search worker and transaction planner.
    pub fn snapshot(&self) -> &Snapshot {
        &self.before
    }

    /// Fork storage/history and apply the staged batch away from the live owner.
    pub fn run(
        self,
        transaction: StagedTransaction,
        cancel: &Cancellation,
    ) -> Result<PreparedEdit, Error> {
        cancel.check()?;
        let mapping = transaction.file.try_clone()?;
        let length = transaction.length;
        let count = transaction.count;
        let mut candidate = self.fork_document(cancel)?;
        candidate.apply_private_staged(transaction, cancel)?;
        let changed = candidate.revision() != self.before.revision();
        let pruned = candidate.take_history_pruned();
        let cleanup = self.before.reader.source.resources.cleanup.clone();
        Ok(PreparedEdit {
            cleanup,
            data: Some(Candidate {
                before: self.before,
                candidate,
                mapping: Some(mapping),
                length,
                count,
                changed,
                pruned,
                cancel: cancel.clone(),
                publish: changed,
                compacted: false,
                baseline: None,
            }),
        })
    }
}

impl PreparedEdit {
    /// Original captured revision for validation and mapping the old viewport.
    pub fn before(&self) -> &Snapshot {
        &self.data.as_ref().expect("live candidate").before
    }
    /// Candidate revision for fallible view mapping, never a publication authority.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.data
            .as_ref()
            .expect("live candidate")
            .candidate
            .background_snapshot()
    }
    /// Map a small fixed set of latest view offsets through all replacements.
    ///
    /// Descriptors are read backwards, as in storage application, without a
    /// resident result list. Caller maps offsets to grapheme boundaries afterward.
    pub fn map_offsets(&self, offsets: &mut [u64], cancel: &Cancellation) -> Result<(), Error> {
        let data = self.data.as_ref().expect("live candidate");
        if !data.changed {
            return Ok(());
        }
        let mut tail = data.length;
        let mut footer = [0; 32];
        for _ in 0..data.count {
            cancel.check()?;
            tail = tail.checked_sub(32).ok_or(Error::CorruptStorage)?;
            data.mapping
                .as_ref()
                .ok_or(Error::CorruptStorage)?
                .read_exact_at(&mut footer, tail)?;
            let number = |i| u64::from_le_bytes(footer[i..i + 8].try_into().expect("fixed footer"));
            let (start, end, inserted, previous) = (number(0), number(8), number(16), number(24));
            for offset in offsets.iter_mut() {
                if *offset >= end {
                    *offset = offset
                        .checked_sub(end - start)
                        .and_then(|n| n.checked_add(inserted))
                        .ok_or(Error::ResourceLimit("mapped offset overflow"))?;
                } else if *offset >= start {
                    *offset = start
                        .checked_add(inserted)
                        .ok_or(Error::ResourceLimit("mapped offset overflow"))?;
                }
            }
            tail = previous;
        }
        Ok(())
    }
}

impl Document {
    /// Capture immutable history/text descriptors for an off-thread atomic batch.
    pub fn prepare_edit(&self) -> Result<EditTask, Error> {
        let before = self.background_snapshot()?;
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let options = OpenOptions {
            limits: store.source.limits.clone(),
            working_directory: store.directory.path().parent().map(Path::to_owned),
            force_copy: false,
            faults: store.source.faults.clone(),
        };
        Ok(EditTask {
            before,
            baseline: self.baseline.clone(),
            history: self.history.reader()?,
            options,
            attention: self.attention,
            pruned: self.pruned,
        })
    }

    /// Publish only if the live identity, generation and revision still match.
    pub fn finish_edit(
        &mut self,
        mut prepared: PreparedEdit,
        cancel: &Cancellation,
    ) -> Result<EditReport, Error> {
        let prepared = prepared.data.as_mut().expect("live candidate");
        cancel.check()?;
        prepared.cancel.check()?;
        if prepared.before.document() != self.id
            || prepared.before.revision() != self.revision()
            || !Arc::ptr_eq(&prepared.before.store, &self.store)
        {
            return Err(Error::StaleRevision);
        }
        // Saves may finish while planning without changing the text revision.
        // Preserve their current destination/save point and conflict attention.
        if prepared.compacted {
            if self
                .blocking_leases
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
            {
                return Err(Error::Busy);
            }
            if self.baseline != prepared.baseline {
                return Err(Error::StaleRevision);
            }
        } else {
            prepared.candidate.baseline = self.baseline.clone();
        }
        prepared.candidate.attention = self.attention;
        let info = if prepared.publish {
            prepared.candidate.info()?
        } else {
            self.info()?
        };
        let report = EditReport {
            info,
            changed: prepared.changed,
            history_pruned: prepared.pruned,
        };
        if prepared.publish {
            prepared.candidate.leases = self.leases.clone();
            prepared.candidate.blocking_leases = self.blocking_leases.clone();
            {
                let mut store = prepared
                    .candidate
                    .store
                    .lock()
                    .map_err(|_| Error::CorruptStorage)?;
                store.source.uncached = false;
                store.tree.uncached = false;
            }
            cancel.check()?;
            prepared.cancel.check()?;
            std::mem::swap(self, prepared.candidate.as_mut());
            // Publication is final: an observer cannot turn the committed edit
            // into a failure or discard its acknowledgement.
            let _ = prepared
                .before
                .reader
                .source
                .check(super::IoPoint::EditCommitted);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Edit, SaveOutcome, SaveTarget, TextRange};

    /// Explicitly order Save capture, batch publication, and late Save acknowledgement.
    #[test]
    fn save_finishes_after_private_batch_publication() {
        let cancel = Cancellation::default();
        let directory = tempfile::tempdir().unwrap();
        let mut options = OpenOptions::default();
        options.limits.undo_transactions = 1;
        options.limits.undo_bytes = 128 << 10;
        let mut document = Document::new(options).unwrap();
        for index in 0..22 {
            let text = if index % 2 == 0 { "a" } else { "b" }.repeat(64 << 10);
            document
                .apply(
                    document.revision(),
                    [Edit {
                        range: TextRange::new(0, document.current.root.entry.bytes).unwrap(),
                        text: &text,
                    }],
                    &cancel,
                )
                .unwrap();
        }
        assert!(document.requires_compaction().unwrap());
        let saved = document.snapshot().unwrap();
        let save = document
            .prepare_save(&saved, SaveTarget::New(directory.path().join("saved")))
            .unwrap();
        let task = document.prepare_edit().unwrap();
        let mut transaction = document.transaction(document.revision()).unwrap();
        transaction
            .replace(TextRange::new(0, 1).unwrap(), b"z".as_slice(), &cancel)
            .unwrap();
        let prepared = task.run(transaction, &cancel).unwrap();
        document.finish_edit(prepared, &cancel).unwrap();
        assert_eq!(
            document.finish_save(save.run(&cancel)).outcome,
            SaveOutcome::Saved
        );
        assert!(document.is_dirty());
        drop(saved);
        let report = document.save(&document.snapshot().unwrap(), SaveTarget::Current, &cancel);
        assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
    }
}
