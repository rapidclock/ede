//! Single-worker bridge; terminal clients never wait for catalog or checkpoint I/O.

use super::{Entry, Id, Store, View};
use crate::document::{Cancellation, Snapshot};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// One bounded recovery operation; content bytes remain on disk behind a snapshot.
pub(crate) enum Request {
    Checkpoint {
        scratch: Id,
        snapshot: Option<Snapshot>,
        view: View,
    },
    Discard(Id),
    RecordError(Id, [u8; 28]),
    Page {
        cursor: Option<(u64, Id)>,
        previous: bool,
    },
}

/// Acknowledged recovery outcome, matched to its submitting operation.
pub(crate) enum Value {
    Checkpoint(Entry),
    Discarded,
    Recorded,
    Page(Vec<(Entry, std::path::PathBuf)>),
}

/// Filesystem failure and publication outcome remain associated with a unique request.
pub(crate) struct Reply {
    pub(crate) request: u64,
    pub(crate) result: io::Result<Value>,
    pub(crate) warning: Option<String>,
}

struct Work {
    request: u64,
    operation: Request,
    cancel: Cancellation,
}

/// At most one running operation and one result; callers retain coalesced intentions.
pub(crate) struct Service {
    sender: Option<SyncSender<Work>>,
    inbox: Receiver<Reply>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    active: Option<Cancellation>,
}

impl Service {
    /// Transfer the exclusive owner into a worker and keep its lock through shutdown.
    pub(crate) fn new(mut store: Store) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(1);
        let (outbox, inbox) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("ede-recovery".into())
            .spawn(move || {
                for work in receiver {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let result = match work.operation {
                        Request::Checkpoint {
                            scratch,
                            snapshot,
                            view,
                        } => store
                            .checkpoint(scratch, snapshot, view, &work.cancel)
                            .map(Value::Checkpoint),
                        Request::Discard(id) => {
                            store.discard(id, &work.cancel).map(|()| Value::Discarded)
                        }
                        Request::RecordError(id, error) => store
                            .record_error(id, error, &work.cancel)
                            .map(|()| Value::Recorded),
                        Request::Page { cursor, previous } => (if previous {
                            store.previous_page(cursor)
                        } else {
                            store.page(cursor)
                        })
                        .map(|entries| {
                            Value::Page(
                                entries
                                    .into_iter()
                                    .map(|entry| {
                                        let path = store.object_path(&entry);
                                        (entry, path)
                                    })
                                    .collect(),
                            )
                        }),
                    };
                    let mut reply = Reply {
                        request: work.request,
                        result,
                        warning: store.take_warning(),
                    };
                    loop {
                        if stopping.load(Ordering::Acquire) {
                            return;
                        }
                        match outbox.try_send(reply) {
                            Ok(()) => break,
                            Err(TrySendError::Disconnected(_)) => return,
                            Err(TrySendError::Full(value)) => reply = value,
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            inbox,
            stop,
            worker: Some(worker),
            active: None,
        })
    }

    /// Submit only when the previous reply has been consumed; never blocks on I/O.
    pub(crate) fn submit(&mut self, request: u64, operation: Request) -> Result<(), Box<Request>> {
        if self.active.is_some() {
            return Err(Box::new(operation));
        }
        let Some(sender) = &self.sender else {
            return Err(Box::new(operation));
        };
        let cancel = Cancellation::default();
        match sender.try_send(Work {
            request,
            operation,
            cancel: cancel.clone(),
        }) {
            Ok(()) => {
                self.active = Some(cancel);
                Ok(())
            }
            Err(TrySendError::Full(work) | TrySendError::Disconnected(work)) => {
                Err(Box::new(work.operation))
            }
        }
    }

    /// Nonblocking polling releases the running slot, including for cancelled work.
    pub(crate) fn poll(&mut self) -> Option<Reply> {
        let reply = self.inbox.try_recv().ok()?;
        self.active = None;
        Some(reply)
    }

    /// Cancellation does not require queue capacity and preserves completed publication.
    pub(crate) fn cancel(&self) {
        if let Some(cancel) = &self.active {
            cancel.cancel();
        }
    }

    /// Observe the actual cancellation token in synchronized worker integration checks.
    #[cfg(test)]
    pub(crate) fn active_cancellation(&self) -> Option<Cancellation> {
        self.active.clone()
    }

    /// Return an off-terminal join handle after revoking outstanding publication work.
    pub(crate) fn shutdown(mut self) -> Option<JoinHandle<()>> {
        self.cancel();
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        self.worker.take()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.cancel();
        self.stop.store(true, Ordering::Release);
        self.sender.take();
    }
}
