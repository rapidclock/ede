//! Two serialized document-owner workers with aggregate queue and payload admission.
//!
//! Submission/poll/close never wait for filesystem work. Result backpressure may
//! stop workers, but closing/cancelling remains independent of queue capacity.

use super::{
    ByteOffset, Cancellation, DocumentId, Error, JobId, OpenOptions, OpenProgress, Revision,
    SaveReport, SaveTarget, Snapshot, StagedTransaction, StorageLimits, TextRange, Window,
    model::Document,
    resources::{Permit, Resources},
    types::identity,
};
use std::{
    collections::HashMap,
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
            Self::Snapshot(id) | Self::Compact(id) => Some(*id),
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
            Self::Open(path) => path.as_os_str().len(),
            Self::Save {
                target: SaveTarget::New(path),
                ..
            } => path.as_os_str().len(),
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
/// Bounded asynchronous storage bridge. Drop cancels and detaches; shutdown().wait()
/// provides orderly joining outside the terminal event loop.
pub struct StorageService {
    senders: Vec<SyncSender<Envelope>>,
    inbox: Receiver<Completion>,
    workers: Vec<JoinHandle<()>>,
    documents: HashMap<DocumentId, Arc<AtomicBool>>,
    entries: Arc<AtomicUsize>,
    resources: Arc<Resources>,
    limits: StorageLimits,
}
/// Pending orderly shutdown. Waiting may block on OS I/O and must occur off the UI thread.
pub struct Shutdown {
    workers: Vec<JoinHandle<()>>,
}
impl Shutdown {
    /// Resolve in-flight publishing saves and release owner-scoped working storage.
    pub fn wait(self) -> Result<(), Error> {
        for worker in self.workers {
            worker
                .join()
                .map_err(|_| Error::Io(std::io::Error::other("storage worker panicked")))?;
        }
        Ok(())
    }
}
impl StorageService {
    /// Spawn two workers, sharing caches and buffers; no synthetic document is opened.
    pub fn new(options: OpenOptions) -> Result<Self, Error> {
        let resources = Resources::new(&options.limits)?;
        let entries = Arc::new(AtomicUsize::new(0));
        let (outbox, inbox) = mpsc::sync_channel(options.limits.queue_entries);
        let mut senders = Vec::new();
        let mut workers = Vec::new();
        for number in 0..2 {
            let (sender, receiver) = mpsc::sync_channel(options.limits.queue_entries);
            let options = options.clone();
            let resources = resources.clone();
            let outbox = outbox.clone();
            workers.push(
                thread::Builder::new()
                    .name(format!("ede-storage-{number}"))
                    .spawn(move || worker(receiver, outbox, options, resources))?,
            );
            senders.push(sender);
        }
        Ok(Self {
            senders,
            inbox,
            workers,
            documents: HashMap::new(),
            entries,
            resources,
            limits: options.limits,
        })
    }
    /// Try to enqueue work. No filesystem work or blocking queue operation occurs here.
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
        let cancel = Cancellation::linked(closed.clone());
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
        let owner = if matches!(
            envelope.request,
            Request::Read { .. } | Request::LocateLine { .. }
        ) {
            1
        } else {
            0
        };
        match self.senders[owner].try_send(envelope) {
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
        self.senders.clear();
        Shutdown {
            workers: std::mem::take(&mut self.workers),
        }
    }
    /// Signal every live owner without waiting for queue slots or filesystem calls.
    fn cancel_all(&mut self) {
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
/// Owner loop: bounded messages, per-document serialization, no result loss under backpressure.
fn worker(
    inbox: Receiver<Envelope>,
    outbox: SyncSender<Completion>,
    options: OpenOptions,
    resources: Arc<Resources>,
) {
    let mut documents: HashMap<DocumentId, OwnedDocument> = HashMap::new();
    loop {
        documents.retain(|_, owned| !owned.closed.load(Ordering::Acquire));
        let mut envelope = match inbox.recv_timeout(Duration::from_millis(10)) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let revision = request_revision(&envelope.request);
        let opening = matches!(envelope.request, Request::Open(_) | Request::New);
        let request = std::mem::replace(&mut envelope.request, Request::New);
        let result = execute(
            request,
            &envelope,
            &mut documents,
            &options,
            resources.clone(),
        );
        let completion = Completion {
            job: envelope.job,
            request: envelope.request_id,
            document: envelope.document,
            revision,
            result,
            closed: envelope.closed,
            opening,
            _admission: envelope.admission,
        };
        if outbox.send(completion).is_err() {
            break;
        }
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
        Request::Read {
            snapshot,
            start,
            max_bytes,
        } => Ok(Response::Window(snapshot.read_window(start, max_bytes)?)),
        Request::LocateLine { snapshot, line } => Ok(Response::Line(
            snapshot.locate_line(line, &envelope.cancel)?,
        )),
        Request::Open(_) | Request::New => {
            let permit = resources.buffers.acquire(16 << 10)?;
            let mut document = if let Request::Open(path) = request {
                Document::open_shared(
                    &path,
                    options.clone(),
                    resources,
                    &envelope.cancel,
                    |update| {
                        if let Ok(mut slot) = envelope.progress.lock() {
                            match update {
                                OpenProgress::Preview { .. } => slot.preview = Some(update),
                                _ => slot.latest = Some(update),
                            }
                        }
                    },
                )?
            } else {
                Document::new_shared(options.clone(), resources)?
            };
            document.id = envelope.document;
            let revision = document.revision();
            documents.insert(
                envelope.document,
                OwnedDocument {
                    document,
                    closed: envelope.closed.clone(),
                    _permit: permit,
                },
            );
            Ok(Response::Ready {
                document: envelope.document,
                revision,
            })
        }
        request => {
            let document = &mut documents
                .get_mut(&envelope.document)
                .ok_or(Error::Closed)?
                .document;
            match request {
                Request::Snapshot(_) => Ok(Response::Snapshot(document.snapshot()?)),
                Request::Read {
                    snapshot,
                    start,
                    max_bytes,
                } => Ok(Response::Window(snapshot.read_window(start, max_bytes)?)),
                Request::LocateLine { snapshot, line } => Ok(Response::Line(
                    snapshot.locate_line(line, &envelope.cancel)?,
                )),
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
                Request::Save { snapshot, target } => Ok(Response::Saved(document.save(
                    &snapshot,
                    target,
                    &envelope.cancel,
                ))),
                Request::Compact(_) => {
                    document.compact(&envelope.cancel)?;
                    Ok(Response::Compacted)
                }
                Request::Open(_) | Request::New => unreachable!("creation handled above"),
            }
        }
    }
}
