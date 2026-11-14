//! Document owner and independent workers for reads, file preparation, and saves.
//!
//! Submission/poll/close never wait for filesystem work. Result backpressure may
//! stop workers, but closing/cancelling remains independent of queue capacity.

use super::{
    ByteOffset, Cancellation, DocumentId, Error, JobId, OpenOptions, OpenProgress, Revision,
    SaveReport, SaveTarget, Snapshot, StagedTransaction, StorageLimits, StorageStats, TextRange,
    Window,
    model::Document,
    resources::{Permit, Resources},
    types::identity,
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Storage operation. Small UI insertions have explicit payload admission;
/// arbitrarily large batches use an already disk-spooled transaction.
pub enum Request {
    /// Prepare a named document, reporting provisional progress through its Job.
    Open(PathBuf),
    /// Create an unnamed empty document.
    New,
    /// Obtain current path, dirty state and retained history positions.
    Info(DocumentId),
    /// Obtain managed cache usage and working-store disk sizes on the owner.
    Stats(DocumentId),
    /// Capture an immutable bounded read/save lease.
    Snapshot(DocumentId),
    /// Read a bounded window from a captured state.
    Read {
        /// Captured immutable state.
        snapshot: Snapshot,
        /// Scalar-aligned logical byte position.
        start: ByteOffset,
        /// Maximum output bytes, at least four.
        max_bytes: usize,
    },
    /// Locate a zero-based line in an immutable captured state.
    LocateLine {
        /// Captured immutable state.
        snapshot: Snapshot,
        /// Zero-based line number.
        line: u64,
    },
    /// Begin a disk-spooled transaction for a current document revision.
    BeginTransaction {
        /// Document to edit.
        document: DocumentId,
        /// Required current revision.
        expected: Revision,
    },
    /// Apply a bounded, single replacement from the UI.
    Replace {
        /// Document to edit.
        document: DocumentId,
        /// Required current revision.
        expected: Revision,
        /// Logical replacement range.
        range: TextRange,
        /// Inserted UTF-8 text; charged before queue admission.
        text: String,
    },
    /// Commit a fully disk-staged batch against its captured revision.
    Apply {
        /// Document to edit.
        document: DocumentId,
        /// Completed staged transaction.
        transaction: StagedTransaction,
    },
    /// Undo one transaction.
    Undo {
        /// Document to edit.
        document: DocumentId,
        /// Required current revision.
        expected: Revision,
    },
    /// Redo one transaction.
    Redo {
        /// Document to edit.
        document: DocumentId,
        /// Required current revision.
        expected: Revision,
    },
    /// Save precisely a captured immutable revision.
    Save {
        /// Captured immutable state.
        snapshot: Snapshot,
        /// Explicit target intent.
        target: SaveTarget,
    },
    /// Reclaim unreachable working data; external leases must first be released.
    Compact(DocumentId),
}
impl Request {
    /// Originating identity, except when opening/creating a new one.
    fn document(&self) -> Option<DocumentId> {
        match self {
            Self::Open(_) | Self::New => None,
            Self::Info(id) | Self::Stats(id) | Self::Snapshot(id) | Self::Compact(id) => Some(*id),
            Self::Read { snapshot, .. }
            | Self::LocateLine { snapshot, .. }
            | Self::Save { snapshot, .. } => Some(snapshot.document()),
            Self::BeginTransaction { document, .. }
            | Self::Replace { document, .. }
            | Self::Apply { document, .. }
            | Self::Undo { document, .. }
            | Self::Redo { document, .. } => Some(*document),
        }
    }
    /// Conservative owned payload charge. Staged file contents do not reside in memory.
    fn payload(&self) -> usize {
        match self {
            Self::Replace { text, .. } => text.capacity(),
            Self::Open(path) => path.capacity(),
            Self::Save {
                target: SaveTarget::New(path),
                ..
            } => path.capacity(),
            _ => 0,
        }
    }
}
/// Successful operation result; semantic/OS failures are carried by Completion.
pub enum Response {
    /// A newly opened/created document is ready and owned by its worker.
    Ready {
        /// New identity.
        document: DocumentId,
        /// Initial monotonic epoch.
        revision: Revision,
    },
    /// Current document metadata, read on the owning worker.
    Info(DocumentInfo),
    /// Measured managed usage for the owner's current revision.
    Stats {
        /// Current monotonic content epoch.
        revision: Revision,
        /// Managed caches and working-file sizes; OS page cache is separate.
        usage: StorageStats,
    },
    /// Bounded immutable state lease.
    Snapshot(Snapshot),
    /// Valid scalar-aligned text and continuation.
    Window(Window),
    /// Exact logical byte location of a line.
    Line(ByteOffset),
    /// An editable disk-spooled transaction plan.
    Transaction(StagedTransaction),
    /// A published content epoch, including undo/redo or a no-op.
    Changed {
        /// Current epoch.
        revision: Revision,
        /// Number of whole undo roots pruned by this edit.
        history_pruned: u64,
    },
    /// Precise pre/post-commit persistence outcome.
    Saved(SaveReport),
    /// Compaction finished without changing text, revision, or undo semantics.
    Compacted,
}
/// Small owner-consistent metadata for interactive state and bounded undo view history.
pub struct DocumentInfo {
    /// Current monotonic revision.
    pub revision: Revision,
    /// Current content needs saving, including any uncertain publication.
    pub dirty: bool,
    /// Logical UTF-8 byte length, excluding any leading BOM.
    pub len: u64,
    /// Exact number of logical lines, including the final empty line after LF.
    pub lines: u64,
    /// Supplied save destination, separate from its display label.
    pub path: Option<PathBuf>,
    /// Current retained undo descriptor identifier.
    pub history: u64,
    /// Oldest retained undo descriptor identifier.
    pub history_begin: u64,
}
/// A bounded progress slot keeps preview until consumed and coalesces numeric progress.
struct Progress {
    _permit: Permit,
    preview: Option<OpenProgress>,
    latest: Option<OpenProgress>,
}
/// Job handle; cancellation never needs a free queue slot.
pub struct Job {
    /// Unique execution identity.
    pub id: JobId,
    /// Originating document identity, allocated before an open begins.
    pub document: DocumentId,
    cancellation: Cancellation,
    progress: Arc<Mutex<Progress>>,
}
impl Job {
    /// Request cooperative cancellation independently of worker/result backpressure.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    #[cfg(test)]
    /// Observe the job's cooperative cancellation in synchronized fault tests.
    pub(crate) fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    /// Take the initial preview, then latest coalesced preparation progress.
    pub fn take_progress(&self) -> Option<OpenProgress> {
        let mut progress = self.progress.lock().ok()?;
        progress.preview.take().or_else(|| progress.latest.take())
    }
}
/// Structured completion; retaining it also retains its aggregate admission charge.
pub struct Completion {
    /// Unique execution identity.
    pub job: JobId,
    /// Caller-supplied request identity, useful for discarding obsolete viewports.
    pub request: u64,
    /// Originating document, never reused for a later tab.
    pub document: DocumentId,
    /// Captured/requested revision where applicable.
    pub revision: Option<Revision>,
    /// Precise result; close converts stale pre-commit results to Closed.
    pub result: Result<Response, Error>,
    closed: Arc<AtomicBool>,
    opening: bool,
    _admission: Admission,
}
/// Failed admission returns ownership of the request so the caller can retry.
pub struct SubmissionError {
    /// Busy, Closed, resource exhaustion or worker failure.
    pub error: Error,
    /// Original request; large staged transactions are not lost on backpressure.
    pub request: Box<Request>,
}
impl std::fmt::Debug for SubmissionError {
    /// Show the failure and target without dumping text payloads or private snapshots.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmissionError")
            .field("error", &self.error)
            .field("document", &self.request.document())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for SubmissionError {
    /// Present the admission failure; the owned request remains available for retry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for SubmissionError {
    /// Retain the underlying typed storage error in the standard error chain.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
/// Global entry reservation, including queued, running, and unconsumed completions.
struct Admission {
    entries: Arc<AtomicUsize>,
    _buffer: Permit,
}
impl Drop for Admission {
    /// Release exactly one successfully admitted entry.
    fn drop(&mut self) {
        self.entries.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Internal envelope routes a document to the same owner for its entire lifetime.
struct Envelope {
    job: JobId,
    request_id: u64,
    document: DocumentId,
    request: Request,
    cancel: Cancellation,
    closed: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    admission: Admission,
}
/// The only worker-owned mutable document state and its resident metadata reservation.
struct OwnedDocument {
    document: Document,
    closed: Arc<AtomicBool>,
    _permit: Permit,
}
/// A named document prepared off the owner, still subject to cancellation on transfer.
struct PreparedOpen {
    envelope: Envelope,
    result: Result<OwnedDocument, Error>,
}
/// Bounded asynchronous storage bridge. Drop cancels and detaches; shutdown().wait()
/// provides orderly joining outside the terminal event loop.
pub struct StorageService {
    mutations: SyncSender<Envelope>,
    reads: SyncSender<Envelope>,
    inbox: Receiver<Completion>,
    workers: Vec<JoinHandle<()>>,
    documents: HashMap<DocumentId, Arc<AtomicBool>>,
    entries: Arc<AtomicUsize>,
    resources: Arc<Resources>,
    limits: StorageLimits,
    stopping: Arc<AtomicBool>,
}
/// Pending orderly shutdown. Waiting may block on OS I/O and must occur off the UI thread.
pub struct Shutdown {
    workers: Vec<JoinHandle<()>>,
}
impl Shutdown {
    /// Resolve in-flight publishing saves and release owner-scoped working storage.
    pub fn wait(self) -> Result<(), Error> {
        let mut panicked = false;
        for worker in self.workers {
            // Join every owner even if an earlier one panicked. Returning early would
            // detach later workers while they still own in-flight operations/resources.
            panicked |= worker.join().is_err();
        }
        if panicked {
            Err(Error::Io(std::io::Error::other("storage worker panicked")))
        } else {
            Ok(())
        }
    }
}
impl StorageService {
    /// Charge editor-owned state to the same aggregate buffer pool as storage.
    pub(crate) fn reserve_interaction(&self, bytes: usize) -> Result<Permit, Error> {
        self.resources.buffers.acquire(bytes)
    }

    /// Spawn the mutation, read, and preparation workers over one resource pool.
    pub fn new(options: OpenOptions) -> Result<Self, Error> {
        Self::with_stop_flag(options, Arc::new(AtomicBool::new(false)))
    }
    /// Allow the terminal owner to cancel all storage jobs even if the coordinator is busy.
    pub(crate) fn with_stop_flag(
        options: OpenOptions,
        stopping: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let resources = Resources::new(&options.limits)?;
        let entries = Arc::new(AtomicUsize::new(0));
        let (outbox, inbox) = mpsc::sync_channel(options.limits.queue_entries);
        let capacity = options.limits.queue_entries;
        let (mutations, mutation_inbox) = mpsc::sync_channel(capacity);
        let (reads, read_inbox) = mpsc::sync_channel(capacity);
        let (opens, open_inbox) = mpsc::sync_channel(capacity);
        // Prepared results still own their original admission. This channel can
        // hold all admitted jobs, including when shutdown has stopped the owner.
        let (prepared, prepared_inbox) = mpsc::sync_channel(capacity);
        let owner_output = outbox.clone();
        let owner_options = options.clone();
        let owner_resources = resources.clone();
        let owner = thread::Builder::new()
            .name("ede-storage".into())
            .spawn(move || {
                worker(
                    mutation_inbox,
                    opens,
                    prepared_inbox,
                    owner_output,
                    owner_options,
                    owner_resources,
                )
            })?;
        let reader = thread::Builder::new()
            .name("ede-read".into())
            .spawn(move || read_worker(read_inbox, outbox))?;
        let open_options = options.clone();
        let open_resources = resources.clone();
        let opener = thread::Builder::new()
            .name("ede-open".into())
            .spawn(move || prepare_opens(open_inbox, prepared, open_options, open_resources))?;
        Ok(Self {
            mutations,
            reads,
            inbox,
            workers: vec![owner, reader, opener],
            documents: HashMap::new(),
            entries,
            resources,
            limits: options.limits,
            stopping,
        })
    }
    /// Try to enqueue work without filesystem I/O or waiting for queue capacity.
    /// Requests for an opening document wait for its preparation to settle.
    pub fn submit(&mut self, request_id: u64, request: Request) -> Result<Job, SubmissionError> {
        match self.admit(request_id, request) {
            Ok(job) => Ok(job),
            Err((error, request)) => Err(SubmissionError { error, request }),
        }
    }
    /// Reserve entry and payload capacity before sending an envelope to its owner.
    fn admit(&mut self, request_id: u64, request: Request) -> Result<Job, (Error, Box<Request>)> {
        if self
            .entries
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limits.queue_entries).then_some(n + 1)
            })
            .is_err()
        {
            return Err((Error::Busy, Box::new(request)));
        }
        let charge = request.payload().checked_add(4096);
        let permit = match charge.and_then(|n| self.resources.buffers.acquire(n).ok()) {
            Some(permit) => permit,
            None => {
                self.entries.fetch_sub(1, Ordering::AcqRel);
                return Err((
                    Error::ResourceLimit("queued payload budget"),
                    Box::new(request),
                ));
            }
        };
        let admission = Admission {
            entries: self.entries.clone(),
            _buffer: permit,
        };
        let is_new = request.document().is_none();
        let document = match request
            .document()
            .map(Ok)
            .unwrap_or_else(|| identity().map(DocumentId))
        {
            Ok(id) => id,
            Err(error) => return Err((error, Box::new(request))),
        };
        let closed = if is_new {
            Arc::new(AtomicBool::new(false))
        } else {
            match self.documents.get(&document) {
                Some(closed) => closed.clone(),
                None => return Err((Error::Closed, Box::new(request))),
            }
        };
        let job_id = match identity() {
            Ok(id) => JobId(id),
            Err(error) => return Err((error, Box::new(request))),
        };
        let cancel = Cancellation::linked(closed.clone(), self.stopping.clone());
        let progress_permit = match self
            .resources
            .buffers
            .acquire(self.limits.text_page_bytes * 2 + 1024)
        {
            Ok(permit) => permit,
            Err(error) => return Err((error, Box::new(request))),
        };
        let progress = Arc::new(Mutex::new(Progress {
            _permit: progress_permit,
            preview: None,
            latest: None,
        }));
        let envelope = Envelope {
            job: job_id,
            request_id,
            document,
            request,
            cancel: cancel.clone(),
            closed: closed.clone(),
            progress: progress.clone(),
            admission,
        };
        let sender = match envelope.request {
            Request::Read { .. } | Request::LocateLine { .. } => &self.reads,
            _ => &self.mutations,
        };
        match sender.try_send(envelope) {
            Ok(()) => {
                if is_new {
                    self.documents.insert(document, closed);
                }
                Ok(Job {
                    id: job_id,
                    document,
                    cancellation: cancel,
                    progress,
                })
            }
            Err(TrySendError::Full(envelope)) => Err((Error::Busy, Box::new(envelope.request))),
            Err(TrySendError::Disconnected(envelope)) => {
                Err((Error::Closed, Box::new(envelope.request)))
            }
        }
    }
    /// Poll a completion without waiting. Callers compare request/revision for viewport staleness.
    pub fn try_recv(&mut self) -> Result<Option<Completion>, Error> {
        match self.inbox.try_recv() {
            Ok(mut completion) => {
                if completion.closed.load(Ordering::Acquire)
                    && !matches!(completion.result, Ok(Response::Saved(_)))
                {
                    completion.result = Err(Error::Closed);
                }
                if completion.result.is_err() && completion.opening {
                    self.documents.remove(&completion.document);
                }
                Ok(Some(completion))
            }
            Err(TryRecvError::Empty) if self.workers.iter().any(JoinHandle::is_finished) => {
                Err(Error::Closed)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(Error::Closed),
        }
    }
    /// Immediately invalidate a document and cancel pre-commit work, even with full queues.
    pub fn close(&mut self, document: DocumentId) {
        if let Some(closed) = self.documents.remove(&document) {
            closed.store(true, Ordering::Release);
        }
    }
    /// Cancel queued/pre-commit work and return join handles for an off-UI wait.
    pub fn shutdown(mut self) -> Shutdown {
        self.cancel_all();
        Shutdown {
            workers: std::mem::take(&mut self.workers),
        }
    }
    /// Signal every live owner without waiting for queue slots or filesystem calls.
    fn cancel_all(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for closed in self.documents.values() {
            closed.store(true, Ordering::Release);
        }
        self.documents.clear();
    }
}
impl Drop for StorageService {
    /// Never indefinitely join on a terminal thread; workers own in-progress resources.
    fn drop(&mut self) {
        self.cancel_all();
    }
}
/// Save thread ownership that cannot silently detach while its document owner unwinds.
struct SaveWorker {
    handle: Option<JoinHandle<super::save::SaveCompletion>>,
    cancel: Cancellation,
}
impl SaveWorker {
    /// Test readiness without blocking the mutation owner.
    fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }
    /// Reconcile an ordinary completion without cancelling a successful operation.
    fn join(mut self) -> thread::Result<super::save::SaveCompletion> {
        self.handle.take().expect("save worker handle").join()
    }
}
impl Drop for SaveWorker {
    /// Runs only on the storage owner: cancel pre-publication work and join even on panic.
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cancel.cancel();
            let _ = handle.join();
        }
    }
}
/// Field order keeps the save joined before its document/lease admission can be released.
struct PendingSave {
    worker: SaveWorker,
    envelope: Envelope,
    snapshot: Snapshot,
}
/// Prepare one named file at a time. Only the mutation owner can install it.
fn prepare_opens(
    inbox: Receiver<Envelope>,
    prepared: SyncSender<PreparedOpen>,
    options: OpenOptions,
    resources: Arc<Resources>,
) {
    for mut envelope in inbox {
        let Request::Open(path) = std::mem::replace(&mut envelope.request, Request::New) else {
            unreachable!("only named opens go to the preparation worker");
        };
        let result = (|| {
            envelope.cancel.check()?;
            let permit = resources.buffers.acquire(4096)?;
            let document = Document::open_shared(
                &path,
                options.clone(),
                resources.clone(),
                &envelope.cancel,
                |update| {
                    if let Ok(mut slot) = envelope.progress.lock() {
                        match update {
                            OpenProgress::Preview { .. } => slot.preview = Some(update),
                            _ => slot.latest = Some(update),
                        }
                    }
                },
            )?;
            Ok(OwnedDocument {
                document,
                closed: envelope.closed.clone(),
                _permit: permit,
            })
        })();
        if prepared.send(PreparedOpen { envelope, result }).is_err() {
            break;
        }
    }
}

/// Run immutable reads without acquiring the mutation owner's document map.
fn read_worker(inbox: Receiver<Envelope>, outbox: SyncSender<Completion>) {
    for mut envelope in inbox {
        let revision = request_revision(&envelope.request);
        let request = std::mem::replace(&mut envelope.request, Request::New);
        let result = envelope.cancel.check().and_then(|()| match request {
            Request::Read {
                snapshot,
                start,
                max_bytes,
            } => Ok(Response::Window(snapshot.read_window_with_cancel(
                start,
                max_bytes,
                &envelope.cancel,
            )?)),
            Request::LocateLine { snapshot, line } => Ok(Response::Line(
                snapshot.locate_line(line, &envelope.cancel)?,
            )),
            _ => unreachable!("only immutable reads go to the read worker"),
        });
        if outbox
            .send(envelope.complete(revision, result, false))
            .is_err()
        {
            break;
        }
    }
}

impl Envelope {
    /// Keep the admission charged until the caller consumes the completion.
    fn complete(
        self,
        revision: Option<Revision>,
        result: Result<Response, Error>,
        opening: bool,
    ) -> Completion {
        Completion {
            job: self.job,
            request: self.request_id,
            document: self.document,
            revision,
            result,
            closed: self.closed,
            opening,
            _admission: self.admission,
        }
    }
}

/// Assign the service identity only when the owner accepts the prepared document.
fn install(
    id: DocumentId,
    mut owned: OwnedDocument,
    documents: &mut HashMap<DocumentId, OwnedDocument>,
) -> Response {
    owned.document.id = id;
    let revision = owned.document.revision();
    documents.insert(id, owned);
    Response::Ready {
        document: id,
        revision,
    }
}

/// Owner loop: bounded messages, per-document serialization, no result loss under backpressure.
fn worker(
    inbox: Receiver<Envelope>,
    opens: SyncSender<Envelope>,
    prepared: Receiver<PreparedOpen>,
    outbox: SyncSender<Completion>,
    options: OpenOptions,
    resources: Arc<Resources>,
) {
    let mut documents: HashMap<DocumentId, OwnedDocument> = HashMap::new();
    // Requests can use Job.document before Ready. Preserve admission order for
    // that identity while allowing unrelated documents to keep making progress.
    let mut opening: HashMap<DocumentId, VecDeque<Envelope>> = HashMap::new();
    let mut ready = VecDeque::new();
    let mut saving: Option<PendingSave> = None;
    loop {
        // Process at most one transfer before giving mutations and saves a turn.
        if let Ok(PreparedOpen { envelope, result }) = prepared.try_recv() {
            if let Some(mut queued) = opening.remove(&envelope.document) {
                ready.append(&mut queued);
            }
            let result = envelope
                .cancel
                .check()
                .and(result)
                .map(|owned| install(envelope.document, owned, &mut documents));
            let revision = match &result {
                Ok(Response::Ready { revision, .. }) => Some(*revision),
                _ => None,
            };
            if outbox
                .send(envelope.complete(revision, result, true))
                .is_err()
            {
                break;
            }
        }
        if saving
            .as_ref()
            .is_some_and(|save| save.worker.is_finished())
        {
            let PendingSave {
                envelope,
                snapshot,
                worker,
            } = saving.take().expect("finished save");
            let result = match worker.join() {
                Ok(saved) => documents
                    .get_mut(&envelope.document)
                    .map(|owned| Response::Saved(owned.document.finish_save(saved)))
                    .ok_or(Error::Closed),
                Err(_) => {
                    if let Some(owned) = documents.get_mut(&envelope.document) {
                        owned.document.attention = true;
                    }
                    Err(Error::SaveWorkerPanicked)
                }
            };
            let completion = envelope.complete(Some(snapshot.revision()), result, false);
            if outbox.send(completion).is_err() {
                break;
            }
        }
        documents.retain(|id, owned| {
            !owned.closed.load(Ordering::Acquire)
                || saving
                    .as_ref()
                    .is_some_and(|save| save.envelope.document == *id)
        });
        let mut envelope = match ready
            .pop_front()
            .map(Ok)
            .unwrap_or_else(|| inbox.recv_timeout(Duration::from_millis(10)))
        {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(PendingSave {
                    envelope,
                    snapshot: _snapshot,
                    worker,
                }) = saving.take()
                    && let Ok(saved) = worker.join()
                    && let Some(owned) = documents.get_mut(&envelope.document)
                {
                    owned.document.finish_save(saved);
                }
                break;
            }
        };
        if matches!(envelope.request, Request::Open(_)) {
            let document = envelope.document;
            // All envelopes share the admission bound, including this open.
            if let Err(error) = opens.try_send(envelope) {
                let (envelope, error) = match error {
                    TrySendError::Disconnected(envelope) => (envelope, Error::Closed),
                    TrySendError::Full(envelope) => (envelope, Error::Busy),
                };
                if outbox
                    .send(envelope.complete(None, Err(error), true))
                    .is_err()
                {
                    break;
                }
            } else {
                opening.insert(document, VecDeque::new());
            }
            continue;
        }
        if let Some(queued) = opening.get_mut(&envelope.document) {
            queued.push_back(envelope);
            continue;
        }
        let revision = request_revision(&envelope.request);
        let creating = matches!(envelope.request, Request::New);
        let request = std::mem::replace(&mut envelope.request, Request::New);
        let request = if let Request::Save { snapshot, target } = request {
            let task = if saving.is_some() {
                Err(Error::Busy)
            } else {
                documents
                    .get(&envelope.document)
                    .ok_or(Error::Closed)
                    .and_then(|owned| owned.document.prepare_save(&snapshot, target))
            };
            if let Ok(task) = task {
                let cancel = envelope.cancel.clone();
                match thread::Builder::new()
                    .name("ede-save".into())
                    .spawn(move || task.run(&cancel))
                {
                    Ok(handle) => {
                        let worker = SaveWorker {
                            handle: Some(handle),
                            cancel: envelope.cancel.clone(),
                        };
                        saving = Some(PendingSave {
                            envelope,
                            snapshot,
                            worker,
                        });
                        continue;
                    }
                    Err(error) => {
                        let completion =
                            envelope.complete(Some(snapshot.revision()), Err(error.into()), false);
                        if outbox.send(completion).is_err() {
                            break;
                        }
                        continue;
                    }
                }
            } else {
                let completion = envelope.complete(
                    Some(snapshot.revision()),
                    Err(task.err().expect("failed save preparation")),
                    false,
                );
                if outbox.send(completion).is_err() {
                    break;
                }
                continue;
            }
        } else {
            request
        };
        let result = execute(
            request,
            &envelope,
            &mut documents,
            &options,
            resources.clone(),
        );
        let revision = match &result {
            Ok(Response::Ready { revision, .. } | Response::Changed { revision, .. }) => {
                Some(*revision)
            }
            Ok(Response::Snapshot(snapshot)) => Some(snapshot.revision()),
            Ok(Response::Info(info)) => Some(info.revision),
            Ok(Response::Stats { revision, .. }) => Some(*revision),
            Ok(Response::Transaction(transaction)) => Some(transaction.expected),
            Ok(Response::Saved(report)) => Some(report.revision),
            _ => revision,
        };
        let completion = envelope.complete(revision, result, creating);
        if outbox.send(completion).is_err() {
            break;
        }
    }
    // Result receiver loss is another shutdown path; never detach a publishing save.
    if let Some(PendingSave {
        envelope,
        snapshot: _snapshot,
        worker,
    }) = saving.take()
        && let Ok(saved) = worker.join()
        && let Some(owned) = documents.get_mut(&envelope.document)
    {
        owned.document.finish_save(saved);
    }
}
/// Captured revision for stale viewport checks; Ready gets its revision in its response.
fn request_revision(request: &Request) -> Option<Revision> {
    match request {
        Request::Read { snapshot, .. }
        | Request::LocateLine { snapshot, .. }
        | Request::Save { snapshot, .. } => Some(snapshot.revision()),
        Request::BeginTransaction { expected, .. }
        | Request::Replace { expected, .. }
        | Request::Undo { expected, .. }
        | Request::Redo { expected, .. } => Some(*expected),
        _ => None,
    }
}
/// Execute an owned request on its document owner. Request ownership avoids payload cloning.
fn execute(
    request: Request,
    envelope: &Envelope,
    documents: &mut HashMap<DocumentId, OwnedDocument>,
    options: &OpenOptions,
    resources: Arc<Resources>,
) -> Result<Response, Error> {
    envelope.cancel.check()?;
    match request {
        Request::New => {
            let permit = resources.buffers.acquire(4096)?;
            let document = Document::new_shared(options.clone(), resources)?;
            Ok(install(
                envelope.document,
                OwnedDocument {
                    document,
                    closed: envelope.closed.clone(),
                    _permit: permit,
                },
                documents,
            ))
        }
        request => {
            let document = &mut documents
                .get_mut(&envelope.document)
                .ok_or(Error::Closed)?
                .document;
            match request {
                Request::Info(_) => Ok(Response::Info(DocumentInfo {
                    revision: document.revision(),
                    dirty: document.is_dirty(),
                    len: document.current.root.entry.bytes,
                    lines: document.current.root.entry.lines + 1,
                    path: document.path().map(PathBuf::from),
                    history: document.history.cursor,
                    history_begin: document.history.begin,
                })),
                Request::Snapshot(_) => Ok(Response::Snapshot(document.snapshot()?)),
                Request::Stats(_) => Ok(Response::Stats {
                    revision: document.revision(),
                    usage: document.stats()?,
                }),
                Request::BeginTransaction { expected, .. } => {
                    Ok(Response::Transaction(document.transaction(expected)?))
                }
                Request::Replace {
                    expected,
                    range,
                    text,
                    ..
                } => {
                    let revision = document.apply(
                        expected,
                        [super::Edit { range, text: &text }],
                        &envelope.cancel,
                    )?;
                    Ok(Response::Changed {
                        revision,
                        history_pruned: document.take_history_pruned(),
                    })
                }
                Request::Apply { transaction, .. } => {
                    let revision = document.apply_staged(transaction, &envelope.cancel)?;
                    Ok(Response::Changed {
                        revision,
                        history_pruned: document.take_history_pruned(),
                    })
                }
                Request::Undo { expected, .. } => Ok(Response::Changed {
                    revision: document.undo(expected)?,
                    history_pruned: 0,
                }),
                Request::Redo { expected, .. } => Ok(Response::Changed {
                    revision: document.redo(expected)?,
                    history_pruned: 0,
                }),
                Request::Compact(_) => {
                    document.compact(&envelope.cancel)?;
                    Ok(Response::Compacted)
                }
                Request::Read { .. }
                | Request::LocateLine { .. }
                | Request::Save { .. }
                | Request::Open(_)
                | Request::New => unreachable!("routed before executing a document mutation"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panicked owner cannot cause shutdown to detach a second still-running worker.
    #[test]
    fn shutdown_joins_every_worker_after_a_panic() {
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let (release, gate) = mpsc::channel();
        let failing = thread::spawn(|| panic!("injected worker panic"));
        let later = thread::spawn(move || {
            gate.recv().unwrap();
            worker_finished.store(true, Ordering::Release);
        });
        let (result_sender, result_receiver) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let outcome = Shutdown {
                workers: vec![failing, later],
            }
            .wait();
            result_sender
                .send((outcome, finished.load(Ordering::Acquire)))
                .unwrap();
        });
        // An early-return implementation completes while the later worker is gated.
        let early = result_receiver.recv_timeout(Duration::from_millis(50)).ok();
        release.send(()).unwrap();
        let (outcome, finished_at_return) = early.unwrap_or_else(|| {
            result_receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        });
        waiter.join().unwrap();
        assert!(outcome.is_err());
        assert!(
            finished_at_return,
            "shutdown returned before every worker finished"
        );
    }
}
