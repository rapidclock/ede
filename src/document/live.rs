//! Append-only text for live captures, using the document store's paged source and index.

use super::{
    Cancellation, Document, Error, Format, Newline, OpenOptions, Revision, Snapshot,
    history::{History, State},
    resources::Resources,
    tree::{Entry, Root},
    types::identity,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

/// Per-session allocator for stream sources sharing ordinary document resource pools.
#[derive(Clone)]
pub(crate) struct CaptureFactory {
    options: OpenOptions,
    resources: Arc<Resources>,
    quota: Arc<Quota>,
    per_stream: u64,
}

/// Single-use handoff prepared at clean EOF. Failed admission leaves the capture owned here.
#[derive(Clone)]
pub struct CaptureAdoption(Arc<Mutex<Option<Document>>>);

impl CaptureAdoption {
    pub(crate) fn new(document: Document) -> Self {
        Self(Arc::new(Mutex::new(Some(document))))
    }

    pub(super) fn take(&self, cancel: &Cancellation) -> Result<Document, Error> {
        let mut value = self.0.lock().map_err(|_| Error::CorruptStorage)?;
        let document = value.as_ref().ok_or(Error::Closed)?;
        let mut store = document.store.lock().map_err(|_| Error::CorruptStorage)?;
        cancel.check()?;
        store.captured.take();
        drop(store);
        value.take().ok_or(Error::Closed)
    }
}

impl CaptureFactory {
    pub(super) fn new(options: OpenOptions, resources: Arc<Resources>) -> Self {
        Self {
            options,
            resources,
            quota: Arc::new(Quota {
                limit: 8 << 30,
                used: Mutex::default(),
                settled: Condvar::new(),
            }),
            per_stream: 2 << 30,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_limits(mut self, per_stream: u64, total: u64) -> Self {
        self.per_stream = per_stream;
        self.quota = Arc::new(Quota {
            limit: total,
            used: Mutex::default(),
            settled: Condvar::new(),
        });
        self
    }

    /// Allocate a private contiguous source on its own worker.
    pub(crate) fn open(&self) -> Result<Capture, Error> {
        let document = Document::new_shared(self.options.clone(), self.resources.clone())?;
        let staging = self.resources.buffers.acquire((64 << 10) + 512)?;
        {
            let mut store = document.store.lock().map_err(|_| Error::CorruptStorage)?;
            // Open descriptors pin captured data, even if process exit abandons a blocked write.
            for name in ["text", "lines", "tree", "history"] {
                std::fs::remove_file(store.directory.path().join(name))?;
            }
            store.captured = Some(Payload {
                quota: self.quota.clone(),
                committed: 0,
            });
        }
        Ok(Capture {
            document,
            last_width: 0,
            limit: self.per_stream,
            _staging: staging,
        })
    }
}

#[derive(Default)]
struct Used {
    committed: u64,
    pending: u64,
}

struct Quota {
    limit: u64,
    used: Mutex<Used>,
    settled: Condvar,
}

/// Held by the store generation, so the last lease releases retained capacity.
pub(super) struct Payload {
    quota: Arc<Quota>,
    committed: u64,
}

impl Drop for Payload {
    fn drop(&mut self) {
        let mut used = self.quota.used.lock().unwrap_or_else(|e| e.into_inner());
        used.committed -= self.committed;
        self.quota.settled.notify_all();
    }
}

/// Pending capacity is counted before disk work and returned even on cancellation.
struct Reservation {
    quota: Arc<Quota>,
    bytes: u64,
    committed: bool,
}

impl Reservation {
    fn acquire(
        quota: &Arc<Quota>,
        want: usize,
        choose: impl FnOnce(usize) -> usize,
        cancel: &Cancellation,
    ) -> Result<Self, Error> {
        let mut used = quota.used.lock().map_err(|_| Error::CorruptStorage)?;
        let mut available = quota.limit - used.committed - used.pending;
        while want as u64 > available && used.pending > 0 {
            cancel.check()?;
            (used, _) = quota
                .settled
                .wait_timeout(used, Duration::from_millis(20))
                .map_err(|_| Error::CorruptStorage)?;
            available = quota.limit - used.committed - used.pending;
        }
        cancel.check()?;
        let bytes = choose(want.min(available as usize)) as u64;
        used.pending += bytes;
        Ok(Self {
            quota: quota.clone(),
            bytes,
            committed: false,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut used = self.quota.used.lock().unwrap_or_else(|e| e.into_inner());
        used.pending -= self.bytes;
        if self.committed {
            used.committed += self.bytes;
        }
        self.quota.settled.notify_all();
    }
}

/// One writer privately owns storage until clean stdin EOF allows scratch adoption.
pub(crate) struct Capture {
    document: Document,
    last_width: u64,
    limit: u64,
    _staging: super::Permit,
}

impl Capture {
    /// Serialized bytes committed so far, including a leading BOM.
    pub(crate) fn retained(&self) -> u64 {
        self.document.current.root.entry.bytes + u64::from(self.document.current.format.bom) * 3
    }

    /// Limit source lookahead when the per-source quota is exactly filled.
    pub(crate) fn at_capacity(&self) -> bool {
        self.retained() == self.limit
            || self.document.store.lock().ok().is_some_and(|store| {
                store.captured.as_ref().is_some_and(|payload| {
                    payload
                        .quota
                        .used
                        .lock()
                        .ok()
                        .is_some_and(|used| used.committed == payload.quota.limit)
                })
            })
    }

    /// Prefix lease can be read independently while an append or finish is in flight.
    pub(crate) fn snapshot(&self) -> Result<Snapshot, Error> {
        let mut snapshot = self.document.capture_snapshot(false)?;
        snapshot.reader.tree.stream_reader = true;
        Ok(snapshot)
    }

    /// Commit only complete validated units, with no per-append tree pages.
    pub(crate) fn append(
        &mut self,
        bytes: &[u8],
        bom: bool,
        complete_prefix: impl FnOnce(usize) -> usize,
        cancel: &Cancellation,
    ) -> Result<usize, Error> {
        let previous = self.retained();
        let want = bytes.len().min((self.limit - previous) as usize);
        let mut store = self
            .document
            .store
            .lock()
            .map_err(|_| Error::CorruptStorage)?;
        let payload = store.captured.as_ref().ok_or(Error::CorruptStorage)?;
        let mut reservation = Reservation::acquire(&payload.quota, want, complete_prefix, cancel)?;
        let count = reservation.bytes as usize;
        if count == 0 {
            return Ok(0);
        }
        let bom_len = usize::from(bom) * 3;
        let text = &bytes[bom_len..count];
        let lines = store.source.index.total();
        let next_revision = self
            .document
            .revision
            .0
            .checked_add(1)
            .ok_or(Error::CorruptStorage)?;
        let next_identity = identity()?;
        store.source.reserve(count as u64 + 512)?;
        if let Err(error) = store
            .source
            .append(&bytes[..count], false)
            .and_then(|()| cancel.check())
        {
            let _ = store.source.rollback(previous, lines);
            return Err(error);
        }
        let format = Format {
            bom: self.document.current.format.bom || bom,
            newline: if lines == 0 {
                match memchr::memchr(b'\n', text) {
                    Some(i) if i > 0 && text[i - 1] == b'\r' => Newline::CrLf,
                    Some(_) => Newline::Lf,
                    None => self.document.current.format.newline,
                }
            } else {
                self.document.current.format.newline
            },
        };
        let mut start = 0;
        for lf in memchr::memchr_iter(b'\n', text) {
            self.last_width += (lf - start) as u64;
            store.original_longest = store.original_longest.max(self.last_width);
            self.last_width = 0;
            start = lf + 1;
        }
        self.last_width += (text.len() - start) as u64;
        store.original_longest = store.original_longest.max(self.last_width);
        let skip = if format.bom { 3 } else { 0 };
        self.document.current = State {
            root: Root::contiguous(Entry {
                id: skip,
                bytes: store.source.len() - skip,
                lines: store.source.index.total(),
            }),
            format,
            identity: next_identity,
        };
        self.document.revision = Revision(next_revision);
        store.captured.as_mut().expect("live capture").committed += reservation.bytes;
        reservation.committed = true;
        Ok(count)
    }

    /// Convert its final text into a scratch leaf and start fresh undo history in place.
    pub(crate) fn finish(mut self, cancel: &Cancellation) -> Result<Document, (Box<Self>, Error)> {
        let result = (|| {
            let mut store = self
                .document
                .store
                .lock()
                .map_err(|_| Error::CorruptStorage)?;
            let old = self.document.current;
            let id = identity()?;
            let revision = Revision(
                self.document
                    .revision
                    .0
                    .checked_add(1)
                    .ok_or(Error::CorruptStorage)?,
            );
            let root = store.initial(Entry {
                id: if old.format.bom { 3 } else { 0 },
                ..old.root.entry
            })?;
            let state = State {
                root,
                identity: id,
                ..old
            };
            let history = History::new(tempfile::tempfile_in(store.directory.path())?, state)?;
            cancel.check()?;
            self.document.current = state;
            self.document.revision = revision;
            self.document.history = history;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(self.document),
            Err(error) => Err((Box::new(self), error)),
        }
    }
}
