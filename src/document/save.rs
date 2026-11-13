//! Streamed guarded save: pre-commit failures preserve the old name; post-commit
//! failures report uncertainty and never attempt a blind rollback.

use super::{
    Cancellation, DocumentId, Error, IoPoint, Revision,
    history::State,
    model::{Baseline, Document},
    platform::{self, Version},
    snapshot::Snapshot,
    source::Reader,
};
use std::{
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Explicit destination intent; no unconditional force-overwrite operation exists.
#[derive(Debug)]
pub enum SaveTarget {
    /// Replace the acknowledged destination only if its complete baseline still matches.
    Current,
    /// Create a path atomically, failing if another writer already created it.
    New(PathBuf),
}
/// Name-publication outcome, distinct from the revision being serialized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveOutcome {
    /// Complete output and parent-directory synchronization acknowledged.
    Saved,
    /// This operation did not publish a destination name.
    NotCommitted,
    /// The destination name was published, but a subsequent check/sync failed.
    CommittedDurabilityUncertain,
}
/// Exact save outcome, including an OS/semantic error when acknowledgement failed.
#[derive(Debug)]
pub struct SaveReport {
    /// Originating document; late UI completion must not revive a closed document.
    pub document: DocumentId,
    /// Immutable revision serialized, which may precede the document's current revision.
    pub revision: Revision,
    /// Whether the destination name was changed.
    pub outcome: SaveOutcome,
    /// Failure detail; present for either unsuccessful acknowledgement outcome.
    pub error: Option<Error>,
}
impl Document {
    /// Serialize exactly the supplied snapshot. Blocking; execute on a storage worker.
    ///
    /// A successful save retains undo and only marks that content state clean. A
    /// save of an older state leaves newer edits dirty. Cancellation is honored
    /// before name publication, then completion reports the actual commit outcome.
    pub fn save(
        &mut self,
        snapshot: &Snapshot,
        target: SaveTarget,
        cancel: &Cancellation,
    ) -> SaveReport {
        match self.prepare_save(snapshot, target) {
            Ok(task) => self.finish_save(task.run(cancel)),
            Err(error) => SaveReport {
                document: self.id(),
                revision: snapshot.revision(),
                outcome: SaveOutcome::NotCommitted,
                error: Some(error),
            },
        }
    }
    /// Capture the destination baseline without holding the mutation lock during I/O.
    pub(super) fn prepare_save(
        &self,
        snapshot: &Snapshot,
        target: SaveTarget,
    ) -> Result<SaveTask, Error> {
        if snapshot.document() != self.id() || !Arc::ptr_eq(&snapshot.store, &self.store) {
            return Err(Error::Closed);
        }
        Ok(SaveTask {
            document: self.id(),
            revision: snapshot.revision(),
            state: *snapshot.state,
            reader: self
                .store
                .lock()
                .map_err(|_| Error::CorruptStorage)?
                .reader()?,
            baseline: self.baseline.clone(),
            target,
        })
    }
    /// Reconcile precisely the state published by a save, preserving newer edits.
    pub(super) fn finish_save(&mut self, completion: SaveCompletion) -> SaveReport {
        match completion.baseline {
            Some(baseline) => {
                self.baseline = Some(baseline);
                self.attention = false;
            }
            None if completion.report.outcome == SaveOutcome::CommittedDurabilityUncertain => {
                self.attention = true
            }
            None => {}
        }
        completion.report
    }
}
/// Independent immutable save input; its caller must retain the snapshot lease until reconciliation.
pub(super) struct SaveTask {
    document: DocumentId,
    revision: Revision,
    state: State,
    reader: Reader,
    baseline: Option<Baseline>,
    target: SaveTarget,
}
/// Worker result, reconciled on the mutable document owner before acknowledgement.
pub(super) struct SaveCompletion {
    baseline: Option<Baseline>,
    report: SaveReport,
}
impl SaveTask {
    /// Perform all potentially slow filesystem work using immutable read handles.
    pub(super) fn run(self, cancel: &Cancellation) -> SaveCompletion {
        let mut committed = false;
        let result = self.save_inner(cancel, &mut committed);
        let (baseline, outcome, error) = match result {
            Ok(baseline) => (Some(baseline), SaveOutcome::Saved, None),
            Err(error) if committed => {
                (None, SaveOutcome::CommittedDurabilityUncertain, Some(error))
            }
            Err(error) => (None, SaveOutcome::NotCommitted, Some(error)),
        };
        SaveCompletion {
            baseline,
            report: SaveReport {
                document: self.document,
                revision: self.revision,
                outcome,
                error,
            },
        }
    }
    /// Execute the ordered preflight/staging/conflict/publication/acknowledgement protocol.
    fn save_inner(&self, cancel: &Cancellation, committed: &mut bool) -> Result<Baseline, Error> {
        cancel.check()?;
        let store = &self.reader;
        let target = &self.target;
        let _buffers = store
            .source
            .resources
            .buffers
            .acquire(store.source.limits.text_page_bytes * 4 + (256 << 10))?;
        let expected = match target {
            SaveTarget::Current => Some(
                self.baseline
                    .as_ref()
                    .ok_or(Error::SaveAsRequired("unnamed document"))?,
            ),
            SaveTarget::New(_) => None,
        };
        let supplied = match target {
            SaveTarget::Current => expected
                .expect("current target has baseline")
                .supplied
                .clone(),
            SaveTarget::New(path) if path.is_absolute() => path.clone(),
            SaveTarget::New(path) => std::env::current_dir()?.join(path),
        };
        let destination = if let Some(baseline) = expected {
            baseline.resolved.clone()
        } else {
            let parent = supplied.parent().ok_or(Error::InvalidRange)?;
            let name = supplied.file_name().ok_or(Error::InvalidRange)?;
            fs::canonicalize(parent)?.join(name)
        };
        let parent_path = destination.parent().ok_or(Error::InvalidRange)?;
        let parent = File::open(parent_path)?;
        let parent_version = Version::of(&parent.metadata()?);
        if let Some(baseline) = expected {
            check_destination(baseline, store, cancel)?;
        } else {
            match fs::symlink_metadata(&destination) {
                Ok(_) => return Err(Error::SaveConflict),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let raw_length = self
            .state
            .root
            .entry
            .bytes
            .checked_add(if self.state.format.bom { 3 } else { 0 })
            .ok_or(Error::ResourceLimit("save length overflow"))?;
        platform::reserve(&parent, raw_length, store.source.limits.disk_reserve_bytes)?;
        let staging = tempfile::Builder::new()
            .prefix(".ede-save-")
            .tempdir_in(parent_path)?;
        platform::secure_directory(staging.path())?;
        let mut output = tempfile::Builder::new()
            .prefix("output-")
            .tempfile_in(staging.path())?;
        output
            .as_file()
            .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let mut dirty_bytes = 0;
        stream(store, self.state, cancel, |bytes| {
            store.source.check(IoPoint::Write)?;
            output.write_all(bytes)?;
            dirty_bytes += bytes.len();
            if dirty_bytes >= 8 << 20 {
                store.source.check(IoPoint::Sync)?;
                output.as_file().sync_data()?;
                dirty_bytes = 0;
            }
            Ok(())
        })?;
        store.source.check(IoPoint::Metadata)?;
        if let Some(baseline) = expected {
            let (original, _, version) = platform::open(&baseline.supplied)?;
            if version != baseline.version {
                return Err(Error::SaveConflict);
            }
            platform::preserve(&original, output.as_file(), &baseline.version)?;
        }
        // Reject inherited metadata too; it must not silently alter the replacement policy.
        platform::replaceable(
            output.as_file(),
            &Version::of(&output.as_file().metadata()?),
        )?;
        store.source.check(IoPoint::Flush)?;
        output.flush()?;
        store.source.check(IoPoint::Sync)?;
        output.as_file().sync_all()?;
        if let Some(baseline) = expected {
            check_destination(baseline, store, cancel)?;
        }
        store.source.check(IoPoint::Publish)?;
        cancel.check()?;
        if !Version::of(&fs::metadata(parent_path)?).same_object(&parent_version) {
            return Err(Error::SaveConflict);
        }
        // Fault hooks may interleave external writers. Recheck after the final hook too.
        if let Some(baseline) = expected {
            check_destination(baseline, store, cancel)?;
        }
        cancel.check()?;
        let staged_version = Version::of(&output.as_file().metadata()?);
        let published = if expected.is_some() {
            output
                .persist(&destination)
                .map_err(|error| Error::Io(error.error))?
        } else {
            output.persist_noclobber(&destination).map_err(|error| {
                if error.error.kind() == io::ErrorKind::AlreadyExists {
                    Error::SaveConflict
                } else {
                    Error::Io(error.error)
                }
            })?
        };
        *committed = true;
        store.source.check(IoPoint::DirectorySync)?;
        parent.sync_all()?;
        store.source.check(IoPoint::Verify)?;
        let version = Version::of(&published.metadata()?);
        if !version.unchanged_after_rename(&staged_version)
            || Version::of(&fs::metadata(&destination)?) != version
            || fs::canonicalize(&supplied)? != destination
        {
            return Err(Error::SaveConflict);
        }
        let baseline = Baseline {
            state: self.state,
            supplied,
            resolved: destination,
            version,
        };
        // The published descriptor is externally writable. Its freshly observed metadata
        // cannot by itself prove that it still contains the state we serialized. Compare
        // against private bytes too, ignoring cancellation after name publication.
        check_destination(&baseline, store, &Cancellation::default())?;
        Ok(baseline)
    }
}
/// Stream preserved BOM followed by logical text using one bounded buffer.
fn stream(
    store: &Reader,
    state: State,
    cancel: &Cancellation,
    mut sink: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    if state.format.bom {
        sink(&[0xef, 0xbb, 0xbf])?;
    }
    let mut offset = 0;
    let mut buffer = vec![0; store.source.limits.text_page_bytes];
    while offset < state.root.entry.bytes {
        cancel.check()?;
        let count = (state.root.entry.bytes - offset).min(buffer.len() as u64) as usize;
        store.read(state, offset, &mut buffer[..count])?;
        sink(&buffer[..count])?;
        offset += count as u64;
    }
    Ok(())
}
/// Match identity/version, symlink resolution, advanced metadata and complete immutable bytes.
fn check_destination(
    baseline: &Baseline,
    store: &Reader,
    cancel: &Cancellation,
) -> Result<(), Error> {
    cancel.check()?;
    let (file, resolved, version) =
        platform::open(&baseline.supplied).map_err(|error| match error {
            Error::Io(ref io) if io.kind() != io::ErrorKind::NotFound => error,
            _ => Error::SaveConflict,
        })?;
    if resolved != baseline.resolved || version != baseline.version {
        return Err(Error::SaveConflict);
    }
    store.source.check(IoPoint::Metadata)?;
    platform::replaceable(&file, &version)?;
    let mut offset = 0;
    let mut actual = vec![0; store.source.limits.text_page_bytes];
    stream(store, baseline.state, cancel, |bytes| {
        store.source.check(IoPoint::Read)?;
        file.read_exact_at(&mut actual[..bytes.len()], offset)?;
        if bytes != &actual[..bytes.len()] {
            return Err(Error::SaveConflict);
        }
        offset += bytes.len() as u64;
        Ok(())
    })?;
    if offset != version.len
        || Version::of(&file.metadata()?) != version
        || fs::canonicalize(&baseline.supplied)? != resolved
        || Version::of(&fs::metadata(Path::new(&resolved))?) != version
    {
        return Err(Error::SaveConflict);
    }
    Ok(())
}
