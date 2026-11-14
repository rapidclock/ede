//! Private-generation named-file refresh with guarded, bounded publication.
//!
//! Preparation forks immutable text/tree/history prefixes, validates disk content,
//! and builds/prunes a replacement without performing I/O under a live owner lock.

use super::{
    ByteOffset, Cancellation, DocumentInfo, Error, OpenOptions, OpenProgress, Revision, Snapshot,
    history::{History, State},
    model::{Baseline, Document},
    platform::{self, Version},
    resources::Resources,
    source::{Store, private_file},
    tree::Entry,
    types::identity,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

/// Observed supplied-path target and metadata, independent of access timestamps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileObservation {
    resolved: PathBuf,
    version: Version,
}

/// Observe regular-file identity on a background worker without reading contents.
pub(crate) fn observe(path: &Path) -> Result<FileObservation, Error> {
    let (_, resolved, version) = platform::open(path)?;
    Ok(FileObservation { resolved, version })
}

/// Immutable input captured by the owner; expensive preparation is independent of it.
pub struct RefreshTask {
    before: Snapshot,
    baseline: Baseline,
    history: History,
    info: DocumentInfo,
    options: OpenOptions,
    resources: Arc<Resources>,
    expected: Option<FileObservation>,
    pruned: u64,
}

/// A complete private replacement with exact metadata and original publication guards.
pub struct PreparedRefresh {
    before: Snapshot,
    baseline: Baseline,
    destination: Baseline,
    replacement: Option<Box<Document>>,
    report: RefreshReport,
    admission: Option<(Arc<AtomicU64>, u64)>,
    verified: bool,
}

/// Exact owner-acknowledged refresh; metadata needs no fallible postcommit read.
#[derive(Clone, Debug)]
pub struct RefreshReport {
    /// Published monotonic revision, unchanged for identical text/format.
    pub revision: Revision,
    /// Whether logical contents or format changed.
    pub changed: bool,
    /// Prior text could not fit the configured undo byte budget.
    pub undo_reset: bool,
    /// Number of old undo transactions removed.
    pub history_pruned: u64,
    /// Metadata prepared before publication and acknowledged with the root swap.
    pub info: DocumentInfo,
}

/// Coalescible progress for validation, generation preparation, or text import.
#[derive(Clone, Copy, Debug)]
pub struct RefreshProgress {
    /// True while importing validated new text; false during other preparation.
    pub importing: bool,
    /// Bytes completed in this phase.
    pub completed: u64,
    /// Total bytes for this phase.
    pub total: u64,
}

impl RefreshTask {
    /// Bind a monitor job to the exact stable source version that requested it.
    pub(crate) fn expect_observation(mut self, observation: FileObservation) -> Self {
        self.expected = Some(observation);
        self
    }

    /// Prepare without holding the mutation owner or its store/cache locks during I/O.
    pub fn run(self, cancel: &Cancellation) -> Result<PreparedRefresh, Error> {
        self.run_with_progress(cancel, |_| {})
    }

    /// Prepare with bounded byte-progress updates; no update history is retained.
    pub fn run_with_progress(
        mut self,
        cancel: &Cancellation,
        mut progress: impl FnMut(RefreshProgress),
    ) -> Result<PreparedRefresh, Error> {
        cancel.check()?;
        let candidate = Document::open_shared(
            &self.baseline.supplied,
            self.options.clone(),
            self.resources.clone(),
            cancel,
            |update| {
                if let OpenProgress::Preparing { completed, total } = update {
                    progress(RefreshProgress {
                        importing: false,
                        completed,
                        total,
                    });
                }
            },
        )?;
        let destination = candidate.baseline.clone().ok_or(Error::CorruptStorage)?;
        let observation = candidate.file_observation().ok_or(Error::CorruptStorage)?;
        if self
            .expected
            .as_ref()
            .is_some_and(|expected| *expected != observation)
        {
            return Err(Error::SourceChanged);
        }
        let input = candidate.background_snapshot()?;
        let mut same = input.format() == self.before.format() && input.len() == self.before.len();
        let mut offset = 0;
        while same && offset < input.len() {
            let new = input.read_window_with_cancel(ByteOffset(offset), 64 << 10, cancel)?;
            let old = self
                .before
                .read_window_with_cancel(ByteOffset(offset), 64 << 10, cancel)?;
            same = old.text() == new.text();
            offset += new.text().len() as u64;
        }
        let (replacement, report) = if same {
            self.info.file_observation = Some(observation);
            let report = RefreshReport {
                revision: self.before.revision(),
                changed: false,
                undo_reset: false,
                history_pruned: 0,
                info: self.info,
            };
            (None, report)
        } else {
            let mut store = Store::fork(
                &self.before.reader,
                &self.options,
                cancel,
                |completed, total| {
                    progress(RefreshProgress {
                        importing: false,
                        completed,
                        total,
                    });
                },
            )?;
            store.original_longest = candidate
                .store
                .lock()
                .map_err(|_| Error::CorruptStorage)?
                .original_longest;
            let history = self.history.fork_to(
                private_file(&store.directory.path().join("history"))?,
                &store.source,
                cancel,
            )?;
            let mut document = Document {
                store: Arc::new(Mutex::new(store)),
                current: *self.before.state,
                baseline: Some(self.baseline.clone()),
                attention: false,
                history,
                id: self.before.document(),
                revision: self.before.revision(),
                leases: Arc::new(AtomicUsize::new(0)),
                blocking_leases: Arc::new(AtomicUsize::new(0)),
                pruned: self.pruned,
            };
            let (reset, pruned) = append_replacement(
                &mut document,
                &input,
                destination.clone(),
                cancel,
                &mut progress,
            )?;
            // A clean tab must reclaim obsolete generations without waiting for a later edit.
            if document.requires_compaction()? {
                document.compact(cancel)?;
            }
            let report = RefreshReport {
                revision: document.revision(),
                changed: true,
                undo_reset: reset,
                history_pruned: pruned,
                info: document.info()?,
            };
            (Some(Box::new(document)), report)
        };
        cancel.check()?;
        let destination = replacement
            .as_ref()
            .and_then(|document| document.baseline.clone())
            .unwrap_or(Baseline {
                state: *self.before.state,
                ..destination
            });
        let mut prepared = PreparedRefresh {
            before: self.before,
            baseline: self.baseline,
            destination,
            replacement,
            report,
            admission: None,
            verified: false,
        };
        prepared.validate_source(cancel)?;
        prepared.verified = false;
        Ok(prepared)
    }
}

/// Build text and history solely in the private generation.
fn append_replacement(
    document: &mut Document,
    input: &Snapshot,
    destination: Baseline,
    cancel: &Cancellation,
    progress: &mut impl FnMut(RefreshProgress),
) -> Result<(bool, u64), Error> {
    let mut store = document.store.lock().map_err(|_| Error::CorruptStorage)?;
    let _permit = store
        .source
        .resources
        .buffers
        .acquire(store.source.limits.text_page_bytes * 4)?;
    let first = store.source.length;
    store.source.reserve(input.len())?;
    let mut offset = 0;
    while offset < input.len() {
        cancel.check()?;
        let window = input.read_window_with_cancel(
            ByteOffset(offset),
            store.source.limits.text_page_bytes,
            cancel,
        )?;
        store.source.append(window.text().as_bytes(), false)?;
        offset += window.text().len() as u64;
        progress(RefreshProgress {
            importing: true,
            completed: offset,
            total: input.len(),
        });
    }
    let piece = Entry {
        id: first,
        bytes: offset,
        lines: store.source.lines(first, offset)?,
    };
    let super::source::Store { tree, source, .. } = &mut *store;
    let state = State {
        root: tree.initial(piece, source)?,
        format: input.format(),
        identity: identity()?,
    };
    let (begin, reset) = match document.history_budget(&store, state, cancel) {
        Ok(begin) => (Some(begin), false),
        Err(Error::ResourceLimit("newest transaction exceeds undo budget")) => (None, true),
        Err(error) => return Err(error),
    };
    let next = document.history.stage(state)?;
    let begin = begin.unwrap_or(next);
    let pruned = document.history.pruned_count(begin)?;
    cancel.check()?;
    document.history.publish(next, begin);
    document.current = state;
    document.baseline = Some(Baseline {
        state,
        ..destination
    });
    document.revision = Revision(
        document
            .revision()
            .0
            .checked_add(1)
            .ok_or(Error::ResourceLimit("revision overflow"))?,
    );
    Ok((reset, pruned))
}

impl PreparedRefresh {
    /// Only a fresh worker-side validation grants the short owner handoff authority.
    pub(crate) fn verified_for_owner(&self) -> bool {
        self.verified
    }
    /// Read original text while mapping the latest view before publication.
    pub(crate) fn before(&self) -> &Snapshot {
        &self.before
    }

    /// Capture the prepared replacement for fallible view mapping before publication.
    pub(crate) fn view_snapshot(&self) -> Result<Snapshot, Error> {
        self.replacement
            .as_ref()
            .ok_or(Error::InvalidRange)?
            .background_snapshot()
    }

    /// Identical contents need no position remapping or undo entry.
    pub(crate) fn changed(&self) -> bool {
        self.report.changed
    }

    /// Revoke publication if newer input is admitted before the owner commits.
    pub(crate) fn guard_admission(&mut self, admitted: Arc<AtomicU64>, expected: u64) {
        self.admission = Some((admitted, expected));
    }

    fn check_admission(&self) -> Result<(), Error> {
        if self
            .admission
            .as_ref()
            .is_some_and(|(admitted, expected)| admitted.load(Ordering::Acquire) != *expected)
        {
            Err(Error::StaleRevision)
        } else {
            Ok(())
        }
    }

    /// Revalidate on a worker immediately before handoff; no owner/cache locks are held.
    pub(crate) fn validate_source(&mut self, cancel: &Cancellation) -> Result<(), Error> {
        cancel.check()?;
        self.before.reader.source.check(super::IoPoint::Verify)?;
        let observed = observe(&self.destination.supplied)?;
        if observed.resolved != self.destination.resolved
            || observed.version != self.destination.version
        {
            return Err(Error::SourceChanged);
        }
        self.verified = true;
        Ok(())
    }
}

impl Document {
    /// Capture a clean named state and immutable branch descriptors without copying.
    pub fn prepare_refresh(&self) -> Result<RefreshTask, Error> {
        if self.is_dirty() {
            return Err(Error::SaveConflict);
        }
        let baseline = self.baseline.clone().ok_or(Error::SaveConflict)?;
        let before = self.background_snapshot()?;
        let store = self.store.lock().map_err(|_| Error::CorruptStorage)?;
        let options = OpenOptions {
            limits: store.source.limits.clone(),
            working_directory: store.directory.path().parent().map(Path::to_owned),
            force_copy: false,
            faults: store.source.faults.clone(),
        };
        let info = DocumentInfo {
            revision: self.revision(),
            dirty: false,
            len: self.current.root.entry.bytes,
            lines: self.current.root.entry.lines + 1,
            path: Some(baseline.supplied.clone()),
            history: self.history.cursor,
            history_begin: self.history.begin,
            compaction_pending: false,
            file_observation: self.file_observation(),
        };
        Ok(RefreshTask {
            before,
            baseline,
            history: self.history.reader()?,
            info,
            options,
            resources: store.source.resources.clone(),
            expected: None,
            pruned: self.pruned,
        })
    }

    /// Blocking standalone publication revalidates the source before adopting the prepared state.
    pub fn finish_refresh(
        &mut self,
        mut prepared: PreparedRefresh,
        cancel: &Cancellation,
    ) -> Result<RefreshReport, Error> {
        prepared.validate_source(cancel)?;
        self.finish_refresh_validated(prepared, cancel)
    }

    /// Owner handoff after worker validation; no file reads or text mapping follow the swap.
    pub(super) fn finish_refresh_validated(
        &mut self,
        mut prepared: PreparedRefresh,
        cancel: &Cancellation,
    ) -> Result<RefreshReport, Error> {
        cancel.check()?;
        prepared.check_admission()?;
        if !prepared.verified
            || prepared.before.document() != self.id
            || prepared.before.revision() != self.revision()
            || !Arc::ptr_eq(&prepared.before.store, &self.store)
            || self.is_dirty()
            || self.baseline.as_ref() != Some(&prepared.baseline)
        {
            return Err(Error::StaleRevision);
        }
        if let Some(mut replacement) = prepared.replacement.take() {
            // Keep one session-wide lease budget across all still-live generations.
            replacement.leases = self.leases.clone();
            replacement.blocking_leases = self.blocking_leases.clone();
            if let Ok(mut store) = replacement.store.lock() {
                store.source.uncached = false;
                store.tree.uncached = false;
            } else {
                return Err(Error::CorruptStorage);
            }
            prepared.check_admission()?;
            *self = *replacement;
        } else {
            self.baseline = Some(prepared.destination);
        }
        Ok(prepared.report)
    }

    /// Owner-consistent metadata captured before a refresh publication or ordinary response.
    pub(super) fn info(&self) -> Result<DocumentInfo, Error> {
        Ok(DocumentInfo {
            revision: self.revision(),
            dirty: self.is_dirty(),
            len: self.current.root.entry.bytes,
            lines: self.current.root.entry.lines + 1,
            path: self.path().map(PathBuf::from),
            history: self.history.cursor,
            history_begin: self.history.begin,
            compaction_pending: self.requires_compaction()?,
            file_observation: self.file_observation(),
        })
    }

    /// Last acknowledged target for the monitor's initial comparison.
    pub(super) fn file_observation(&self) -> Option<FileObservation> {
        self.baseline.as_ref().map(|baseline| FileObservation {
            resolved: baseline.resolved.clone(),
            version: baseline.version.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Edit, SaveOutcome, SaveTarget, StorageLimits, TextRange};

    /// I02: report shared managed high waters and sampled fixture/working disk costs.
    #[test]
    #[ignore = "large storage probe: EDE_BENCH_MIB=1024"]
    fn large_refresh_storage_measurements() {
        use std::{
            io::Write,
            os::unix::fs::{FileExt, MetadataExt},
            time::Instant,
        };
        /// Include visible fixture and working files, reporting logical and allocated bytes.
        fn disk(path: &Path) -> (u64, u64) {
            let mut total = (0, 0);
            for item in std::fs::read_dir(path).unwrap() {
                let item = item.unwrap();
                let metadata = item.metadata().unwrap();
                let next = if metadata.is_dir() {
                    disk(&item.path())
                } else {
                    (metadata.len(), metadata.blocks() * 512)
                };
                total.0 += next.0;
                total.1 += next.1;
            }
            total
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let mib: u64 = std::env::var("EDE_BENCH_MIB")
            .unwrap_or_else(|_| "1024".into())
            .parse()
            .unwrap();
        let mut chunk = vec![b'x'; 1 << 20];
        for index in (63..chunk.len()).step_by(64) {
            chunk[index] = b'\n';
        }
        let mut file = std::fs::File::create(&path).unwrap();
        for index in 0..mib {
            file.write_all(&chunk).unwrap();
            if (index + 1).is_multiple_of(8) {
                file.sync_data().unwrap();
            }
        }
        file.sync_all().unwrap();
        eprintln!("refresh_storage fixture_ready bytes={}", mib * (1 << 20));
        let limits = StorageLimits::default();
        let cancel = Cancellation::default();
        let mut document = Document::open(
            &path,
            OpenOptions {
                working_directory: Some(root.path().to_owned()),
                ..OpenOptions::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap();
        let cloned = document.stats().unwrap().snapshot_cloned;
        file.write_all_at(b"changed!", 0).unwrap();
        file.sync_all().unwrap();
        let started = Instant::now();
        let mut sampled_disk = disk(root.path());
        let mut next_sample = 0;
        let prepared = document
            .prepare_refresh()
            .unwrap()
            .run_with_progress(&cancel, |progress| {
                if progress.completed >= next_sample {
                    let current = disk(root.path());
                    sampled_disk.0 = sampled_disk.0.max(current.0);
                    sampled_disk.1 = sampled_disk.1.max(current.1);
                    next_sample = progress.completed + (64 << 20);
                }
            })
            .unwrap();
        let current = disk(root.path());
        sampled_disk.0 = sampled_disk.0.max(current.0);
        sampled_disk.1 = sampled_disk.1.max(current.1);
        let report = document.finish_refresh(prepared, &cancel).unwrap();
        let stats = document.stats().unwrap();
        assert!(stats.cache_high_water_bytes <= limits.text_cache_bytes + limits.node_cache_bytes);
        assert!(stats.buffer_high_water_bytes <= limits.buffer_bytes);
        eprintln!(
            "refresh_storage bytes={} initial_clone={cloned} refresh_ms={:.3} undo_reset={} cache_high_water={} buffer_high_water={} final_text_bytes={} final_metadata_bytes={} sampled_fixture_and_working_logical_bytes={} sampled_allocated_bytes={}",
            document.snapshot().unwrap().len(),
            started.elapsed().as_secs_f64() * 1000.0,
            report.undo_reset,
            stats.cache_high_water_bytes,
            stats.buffer_high_water_bytes,
            stats.text_bytes,
            stats.metadata_bytes,
            sampled_disk.0,
            sampled_disk.1
        );
    }
    /// Background generation I/O cannot hold a foreground mutation lock.
    #[test]
    fn blocked_refresh_write_does_not_block_foreground_edit() {
        use crate::document::{FaultInjector, IoPoint};
        use std::sync::{Condvar, mpsc};
        use std::time::Duration;
        #[derive(Debug, Default)]
        struct Gate {
            armed: std::sync::atomic::AtomicBool,
            state: Mutex<(bool, bool)>,
            wake: Condvar,
        }
        impl FaultInjector for Gate {
            fn check(&self, point: IoPoint) -> std::io::Result<()> {
                if point == IoPoint::Write && self.armed.swap(false, Ordering::SeqCst) {
                    let mut state = self.state.lock().unwrap();
                    state.0 = true;
                    self.wake.notify_all();
                    while !state.1 {
                        state = self.wake.wait(state).unwrap();
                    }
                }
                Ok(())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "old").unwrap();
        let gate = Arc::new(Gate::default());
        let mut document = Document::open(
            &path,
            OpenOptions {
                faults: Some(gate.clone()),
                ..OpenOptions::default()
            },
            &Cancellation::default(),
            |_| {},
        )
        .unwrap();
        std::fs::write(&path, "external version").unwrap();
        let task = document.prepare_refresh().unwrap();
        let hook = gate.clone();
        let cancelled = Cancellation::default();
        let running = cancelled.clone();
        let refresh = std::thread::spawn(move || {
            task.run_with_progress(&running, |progress| {
                if !progress.importing && progress.completed == progress.total {
                    hook.armed.store(true, Ordering::SeqCst);
                }
            })
        });
        let mut state = gate.state.lock().unwrap();
        while !state.0 {
            state = gate.wake.wait(state).unwrap();
        }
        drop(state);
        let (sender, receiver) = mpsc::channel();
        let edit = std::thread::spawn(move || {
            let result = document.apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text: "local ",
                }],
                &Cancellation::default(),
            );
            sender.send(result).unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_millis(250));
        cancelled.cancel();
        gate.state.lock().unwrap().1 = true;
        gate.wake.notify_all();
        edit.join().unwrap();
        let _ = refresh.join().unwrap();
        assert!(
            matches!(result, Ok(Ok(_))),
            "foreground edit waited for gated refresh I/O: {result:?}"
        );
    }
    /// F02: the standalone API also rejects a newer source before installation.
    #[test]
    fn newer_disk_version_revokes_prepared_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "original").unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(&path, OpenOptions::default(), &cancel, |_| {}).unwrap();
        std::fs::write(&path, "candidate A").unwrap();
        let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
        std::fs::write(&path, "newer version BBB").unwrap();
        let result = document.finish_refresh(prepared, &cancel);
        assert!(
            matches!(result, Err(Error::SourceChanged | Error::StaleRevision)),
            "older prepared content installed: {result:?}"
        );
    }

    /// F03: unattended refreshes collect unreachable text without waiting for a user edit.
    #[test]
    fn repeated_refresh_reclaims_unreachable_working_text() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, vec![b'a'; 128 << 10]).unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(
            &path,
            OpenOptions {
                limits: StorageLimits {
                    undo_bytes: 1024,
                    ..StorageLimits::default()
                },
                ..OpenOptions::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap();
        for value in b'b'..=b'z' {
            std::fs::write(&path, vec![value; 128 << 10]).unwrap();
            let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
            assert!(
                document
                    .finish_refresh(prepared, &cancel)
                    .unwrap()
                    .undo_reset
            );
        }
        assert!(
            !document.requires_compaction().unwrap(),
            "unreachable arena grew to {} bytes for 128 KiB text with reset history",
            document.stats().unwrap().text_bytes
        );
    }

    /// F03: refresh is one undo operation and the refreshed state is the file baseline.
    #[test]
    fn refresh_undo_redo_preserves_new_disk_baseline() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "old text").unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(&path, OpenOptions::default(), &cancel, |_| {}).unwrap();
        let task = document.prepare_refresh().unwrap();
        std::fs::write(&path, "new text").unwrap();
        let prepared = task.run(&cancel).unwrap();
        let report = document.finish_refresh(prepared, &cancel).unwrap();
        assert!(report.changed);
        assert!(!report.undo_reset);
        assert!(!document.is_dirty());
        document.undo(document.revision()).unwrap();
        assert!(document.is_dirty());
        assert_eq!(
            document
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "old text"
        );
        document.redo(document.revision()).unwrap();
        assert!(!document.is_dirty());
        assert_eq!(
            document
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "new text"
        );
    }

    /// F03: format-only replacement is undoable; identical-content probes preserve history.
    #[test]
    fn format_only_and_identical_refresh_preserve_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "same\r\n").unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(&path, OpenOptions::default(), &cancel, |_| {}).unwrap();
        document
            .apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text: "!",
                }],
                &cancel,
            )
            .unwrap();
        let snapshot = document.snapshot().unwrap();
        assert_eq!(
            document
                .save(&snapshot, SaveTarget::Current, &cancel)
                .outcome,
            SaveOutcome::Saved
        );
        drop(snapshot);
        std::fs::write(&path, b"\xef\xbb\xbf!same\r\n").unwrap();
        let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
        assert!(document.finish_refresh(prepared, &cancel).unwrap().changed);
        let revision = document.revision();
        std::fs::write(&path, b"\xef\xbb\xbf!same\r\n").unwrap();
        let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
        assert!(!document.finish_refresh(prepared, &cancel).unwrap().changed);
        assert_eq!(document.revision(), revision);
        document.undo(document.revision()).unwrap();
        assert!(!document.snapshot().unwrap().format().bom);
        assert!(document.is_dirty());
        document.undo(document.revision()).unwrap();
        assert_eq!(
            document
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "same\r\n"
        );
        document.redo(document.revision()).unwrap();
        document.redo(document.revision()).unwrap();
        assert!(document.snapshot().unwrap().format().bom);
        assert!(!document.is_dirty());
    }

    /// F02: a candidate prepared before local edits cannot replace their acknowledged root.
    #[test]
    fn local_edit_revokes_prepared_refresh_and_keeps_guarded_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "old").unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(&path, OpenOptions::default(), &cancel, |_| {}).unwrap();
        std::fs::write(&path, "external").unwrap();
        let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
        document
            .apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text: "local ",
                }],
                &cancel,
            )
            .unwrap();
        assert!(matches!(
            document.finish_refresh(prepared, &cancel),
            Err(Error::StaleRevision)
        ));
        let snapshot = document.snapshot().unwrap();
        assert_eq!(
            snapshot.read_window(ByteOffset(0), 64).unwrap().text(),
            "local old"
        );
        assert_eq!(
            document
                .save(&snapshot, SaveTarget::Current, &cancel)
                .outcome,
            SaveOutcome::NotCommitted
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
    }

    /// F03: the explicit old-text cap resets history rather than blocking a valid refresh.
    #[test]
    fn refresh_above_undo_budget_reports_history_reset() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, vec![b'a'; 128 << 10]).unwrap();
        let cancel = Cancellation::default();
        let mut document = Document::open(
            &path,
            OpenOptions {
                limits: StorageLimits {
                    undo_bytes: 64 << 10,
                    ..StorageLimits::default()
                },
                ..OpenOptions::default()
            },
            &cancel,
            |_| {},
        )
        .unwrap();
        std::fs::write(&path, "replacement").unwrap();
        let prepared = document.prepare_refresh().unwrap().run(&cancel).unwrap();
        let report = document.finish_refresh(prepared, &cancel).unwrap();
        assert!(report.undo_reset);
        assert!(!document.is_dirty());
        let revision = document.revision();
        assert_eq!(document.undo(revision).unwrap(), revision);
        assert_eq!(
            document
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "replacement"
        );
    }
}
