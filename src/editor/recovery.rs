//! Scratch lifecycle coordination, with all persistent filesystem work on one worker.

use super::*;
use crate::recovery::{self, Entry, Id, View as SavedView};
use std::time::Instant;

const PERSISTENCE_RETRY: Duration = Duration::from_secs(5);

/// Bounded display metadata; scratch contents never cross into the terminal reducer.
#[derive(Clone, Debug)]
pub(crate) struct RecoveryRow {
    pub(crate) id: Id,
    pub(crate) label: String,
}

/// Stable row identities and native catalog cursors for the recovery dialog.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecoveryPage {
    pub(crate) rows: Arc<[RecoveryRow]>,
    pub(crate) before: Option<(u64, Id)>,
    pub(crate) after: Option<(u64, Id)>,
    pub(crate) previous: bool,
    pub(crate) next: bool,
}

/// Runtime identity and last acknowledged checkpoint are independent of named saves.
pub(super) struct RetainedScratch {
    pub(super) id: Id,
    order: u64,
    retained: Option<(document::Revision, SavedView)>,
    pub(super) restore: Option<(Entry, PathBuf)>,
    retry: Option<Instant>,
    pub(super) retiring: bool,
    pub(super) warning: Option<String>,
}

impl RetainedScratch {
    pub(super) fn status(&self, revision: Option<document::Revision>) -> &'static str {
        if self
            .retained
            .is_some_and(|(retained, _)| Some(retained) == revision)
        {
            "Retained"
        } else {
            "Checkpoint pending"
        }
    }
}

enum Pending {
    RecordError,
    Page {
        generation: u64,
        cursor: Option<(u64, Id)>,
    },
    DiscardStored {
        scratch: Id,
        generation: u64,
    },
    Checkpoint {
        tab: TabId,
        scratch: Id,
        revision: document::Revision,
        view: SavedView,
    },
    Discard {
        tab: TabId,
        scratch: Id,
        after: Option<(After, u64)>,
    },
}

struct ListRequest {
    generation: u64,
    cursor: Option<(u64, Id)>,
    previous: bool,
}

/// Bounded pending intentions; the catalog retains unloaded overflow on disk.
pub(super) struct Retention {
    pub(super) service: recovery::Service,
    startup: Vec<(Entry, PathBuf)>,
    pub(super) active: Option<Id>,
    next_order: u64,
    interval: Duration,
    due: Instant,
    pub(super) sweep: bool,
    pub(super) quitting: Option<u64>,
    pending: Option<(u64, Pending)>,
    closes: VecDeque<(TabId, Id, After, u64)>,
    cursor: usize,
    _permit: document::Permit,
    list_request: Option<ListRequest>,
    list_generation: u64,
    listing: Vec<(Entry, PathBuf)>,
    stored_discard: Option<(Id, u64)>,
    restore_errors: VecDeque<(Id, [u8; 28])>,
}

impl Retention {
    pub(super) fn checkpointing(&self, tab: TabId) -> bool {
        self.pending.as_ref().is_some_and(|(_, pending)| matches!(pending, Pending::Checkpoint { tab: current, .. } if *current == tab))
    }
    pub(super) fn request_page(
        &mut self,
        generation: u64,
        cursor: Option<(u64, Id)>,
        previous: bool,
    ) {
        self.list_generation = generation;
        self.list_request = Some(ListRequest {
            generation,
            cursor,
            previous,
        });
    }
}

impl Coordinator {
    /// Revoke pending close intentions and restore authority for any tab kept by Cancel.
    pub(super) fn cancel_retained_close(&mut self) -> Result<(), Error> {
        if let Some(retention) = &mut self.retention {
            retention.quitting = None;
            retention.closes.clear();
            if let Some((_, Pending::Discard { after, .. })) = &mut retention.pending {
                *after = None;
            }
        }
        for index in 0..self.tabs.len() {
            let id = self.tabs[index].id;
            let replacement = if let Some(scratch) = self
                .state_mut(id)
                .and_then(|state| state.scratch.as_mut())
                .filter(|scratch| scratch.retiring)
            {
                let previous = scratch.id;
                scratch.id = Id::fresh()?;
                scratch.retained = None;
                scratch.retiring = false;
                Some((previous, scratch.id))
            } else {
                None
            };
            if let Some((old, new)) = replacement
                && let Some(retention) = &mut self.retention
            {
                if retention.active == Some(old) {
                    retention.active = Some(new);
                }
                retention.sweep = true;
            }
        }
        Ok(())
    }
    /// An explicit quit/retry bypasses checkpoint backoff and freezes the final retention sweep.
    pub(super) fn begin_retained_quit(&mut self, sequence: u64) {
        if let Some(retention) = &mut self.retention {
            retention.quitting = Some(sequence);
            retention.sweep = true;
        }
        for index in 0..self.tabs.len() {
            let id = self.tabs[index].id;
            if let Some(scratch) = self.state_mut(id).and_then(|state| state.scratch.as_mut()) {
                scratch.retry = None;
            }
        }
    }
    /// Initialize recovery on the coordinator, away from the terminal/rendering thread.
    pub(super) fn with_retention(mut self, store: recovery::Store) -> Self {
        let setup = (|| -> std::io::Result<Retention> {
            let startup = store
                .page(None)?
                .into_iter()
                .map(|entry| {
                    let path = store.object_path(&entry);
                    (entry, path)
                })
                .collect();
            let next_order = store.next_order()?;
            let interval = store.interval();
            let permit = self
                .service
                .reserve_interaction(2 << 20)
                .map_err(std::io::Error::other)?;
            Ok(Retention {
                service: recovery::Service::new(store)?,
                startup,
                next_order,
                interval,
                active: None,
                due: Instant::now() + interval,
                sweep: false,
                quitting: None,
                pending: None,
                closes: VecDeque::new(),
                cursor: 0,
                _permit: permit,
                list_request: None,
                list_generation: 0,
                listing: Vec::new(),
                stored_discard: None,
                restore_errors: VecDeque::new(),
            })
        })();
        match setup {
            Ok(retention) => {
                self.retention = Some(retention);
                match self
                    .service
                    .reserve_interaction(1 << 20)
                    .map_err(std::io::Error::other)
                    .and_then(Monitor::new)
                {
                    Ok(monitor) => self.monitor = Some(monitor),
                    Err(error) => {
                        self.event(Event::Failure(format!(
                            "File monitor initialization failed: {error}"
                        )));
                        self.stop.store(true, Ordering::Release);
                    }
                }
            }
            Err(error) => {
                self.event(Event::Failure(format!(
                    "Recovery initialization failed: {error}"
                )));
                self.stop.store(true, Ordering::Release);
            }
        }
        self
    }

    /// Assign a persistent scratch identity without performing filesystem I/O.
    pub(super) fn fresh_scratch(&mut self) -> Result<Option<RetainedScratch>, Error> {
        let Some(retention) = &mut self.retention else {
            return Ok(None);
        };
        let order = retention.next_order;
        retention.next_order = order
            .checked_add(1)
            .ok_or(Error::ResourceLimit("scratch order overflow"))?;
        let id = Id::fresh()?;
        retention.active = Some(id);
        Ok(Some(RetainedScratch {
            id,
            order,
            retained: None,
            restore: None,
            retry: None,
            retiring: false,
            warning: None,
        }))
    }

    /// Admit restored tabs after explicit CLI files, leaving overflow in the catalog.
    pub(super) fn restore_startup(&mut self) -> Result<(), Error> {
        let Some(retention) = &mut self.retention else {
            return Ok(());
        };
        let mut entries = std::mem::take(&mut retention.startup);
        let remaining = entries.split_off((crate::TAB_LIMIT - self.tabs.len()).min(entries.len()));
        retention.startup = remaining;
        let had_cli = !self.tabs.is_empty();
        let mut active = None;
        let mut first = None;
        for (entry, path) in entries.into_iter().take(crate::TAB_LIMIT - self.tabs.len()) {
            let id = self.admit_tab(None)?;
            let state = self.state_mut(id).ok_or(Error::Closed)?;
            state.scratch = Some(RetainedScratch {
                id: entry.id,
                order: entry.view.order,
                retained: None,
                restore: Some((entry, path)),
                retry: None,
                retiring: false,
                warning: None,
            });
            first.get_or_insert(id);
            if entry.view.active {
                active = Some(id);
                if let Some(retention) = &mut self.retention
                    && retention.active.is_none()
                {
                    retention.active = Some(entry.id);
                }
            }
        }
        if !had_cli && let Some(id) = active.or(first) {
            self.activate(id);
        }
        Ok(())
    }

    /// Resolve a row only against the current listing generation, then prepare its immutable object.
    pub(super) fn open_retained(&mut self, generation: u64, scratch: Id) -> Result<(), Error> {
        let Some(retention) = &self.retention else {
            return Err(Error::Closed);
        };
        if generation != retention.list_generation {
            return Err(Error::Cancelled);
        }
        if retention.restore_errors.len() >= recovery::PAGE_ENTRIES {
            return Err(Error::Busy);
        }
        if retention.stored_discard.is_some_and(|(id, _)| id == scratch)
            || retention.pending.as_ref().is_some_and(|(_, operation)| matches!(operation, Pending::DiscardStored { scratch: id, .. } if *id == scratch)) {
            return Err(Error::Busy);
        }
        let (entry, path) = retention
            .listing
            .iter()
            .find(|(entry, _)| entry.id == scratch)
            .cloned()
            .ok_or(Error::Cancelled)?;
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| {
                self.state(tab.id).is_some_and(|state| {
                    state
                        .scratch
                        .as_ref()
                        .is_some_and(|current| current.id == scratch)
                })
            })
            .map(|tab| tab.id)
        {
            return self.switch_tab(tab);
        }
        let id = self.admit_tab(None)?;
        self.state_mut(id).ok_or(Error::Closed)?.scratch = Some(RetainedScratch {
            id: scratch,
            order: entry.view.order,
            retained: None,
            restore: Some((entry, path)),
            retry: None,
            retiring: false,
            warning: None,
        });
        self.activate(id);
        Ok(())
    }

    /// Discard only an unloaded row belonging to the current owner/listing.
    pub(super) fn discard_retained(&mut self, generation: u64, scratch: Id) -> Result<(), Error> {
        let retention = self.retention.as_mut().ok_or(Error::Closed)?;
        if retention.stored_discard.is_some()
            || retention
                .pending
                .as_ref()
                .is_some_and(|(_, operation)| matches!(operation, Pending::DiscardStored { .. }))
        {
            return Err(Error::Busy);
        }
        if generation != retention.list_generation
            || !retention
                .listing
                .iter()
                .any(|(entry, _)| entry.id == scratch)
        {
            return Err(Error::Cancelled);
        }
        if self.tabs.iter().any(|tab| {
            self.state(tab.id).is_some_and(|state| {
                state
                    .scratch
                    .as_ref()
                    .is_some_and(|current| current.id == scratch)
            })
        }) {
            return Err(Error::Busy);
        }
        let retention = self.retention.as_mut().ok_or(Error::Closed)?;
        retention.startup.retain(|(entry, _)| entry.id != scratch);
        retention.stored_discard = Some((scratch, generation));
        Ok(())
    }

    /// Install only validated positions against the restored revision and reset view history.
    pub(super) fn restore_view(&mut self, id: TabId) -> Result<bool, Error> {
        let restored = self
            .state_mut(id)
            .and_then(|state| state.scratch.as_mut())
            .and_then(|scratch| scratch.restore.take());
        let Some((entry, _)) = restored else {
            return Ok(false);
        };
        let document = self
            .state(id)
            .and_then(|state| state.screen.document)
            .ok_or(Error::Closed)?;
        let snapshot = match self.call(Request::Snapshot(document))? {
            Response::Snapshot(snapshot) => snapshot,
            _ => return Err(Error::CorruptStorage),
        };
        let mut text = Text::new(&snapshot, &self.cancel);
        let caret = text.ceil(
            snapshot
                .floor_scalar(ByteOffset(entry.view.caret.min(snapshot.len())))?
                .0,
        )?;
        let anchor = text.ceil(
            snapshot
                .floor_scalar(ByteOffset(entry.view.anchor.min(snapshot.len())))?
                .0,
        )?;
        let state = self.state_mut(id).ok_or(Error::Closed)?;
        state.screen.position = Position { caret, anchor };
        state.top = entry.view.top.min(snapshot.line_count().saturating_sub(1));
        state.left = entry.view.left;
        state.preferred = entry.view.preferred;
        state.undo_views.clear();
        state.redo_views.clear();
        let revision = state.info.as_ref().ok_or(Error::Busy)?.revision;
        let scratch = state.scratch.as_mut().ok_or(Error::Closed)?;
        scratch.retained = Some((revision, entry.view));
        if entry.restore_error.iter().any(|byte| *byte != 0)
            && let Some(retention) = &mut self.retention
        {
            retention.restore_errors.retain(|(id, _)| *id != entry.id);
            retention.restore_errors.push_back((entry.id, [0; 28]));
        }
        Ok(true)
    }

    /// Queue a small persistent diagnostic; catalog I/O stays on the recovery worker.
    pub(super) fn record_restore_error(&mut self, tab: TabId, error: &Error) {
        let Some(scratch) = self
            .state(tab)
            .and_then(|state| state.scratch.as_ref())
            .filter(|scratch| scratch.restore.is_some())
            .map(|scratch| scratch.id)
        else {
            return;
        };
        let Some(retention) = &mut self.retention else {
            return;
        };
        let message = error.to_string();
        let mut summary = [0; 28];
        let mut count = message.len().min(summary.len());
        while !message.is_char_boundary(count) {
            count -= 1;
        }
        summary[..count].copy_from_slice(&message.as_bytes()[..count]);
        retention.restore_errors.retain(|(id, _)| *id != scratch);
        retention.restore_errors.push_back((scratch, summary));
    }

    /// Yield a background lease before compaction rather than rejecting the accepted edit.
    pub(super) fn yield_checkpoint(&mut self, tab: TabId) -> Result<(), Error> {
        if let Some(retention) = &mut self.retention
            && retention.pending.as_ref().is_some_and(|(_, pending)| matches!(pending, Pending::Checkpoint { tab: current, .. } if *current == tab))
        {
            retention.service.cancel();
            retention.sweep = false;
            retention.due = Instant::now() + retention.interval;
        } else { return Ok(()); }
        // The cancelled reader pins its immutable generation. Do not wait here
        // for an uninterruptible write; current-generation compaction is independent.
        Ok(())
    }

    /// Close cannot finish before catalog removal is committed and stale work is revoked.
    pub(super) fn retire_before_close(&mut self, tab: TabId, after: After, sequence: u64) -> bool {
        let Some(scratch) = self
            .state(tab)
            .and_then(|state| state.scratch.as_ref())
            .map(|scratch| scratch.id)
        else {
            return false;
        };
        let Some(retention) = &mut self.retention else {
            return false;
        };
        if !retention.closes.iter().any(|(id, _, _, _)| *id == tab) {
            retention.closes.push_back((tab, scratch, after, sequence));
        }
        if retention.pending.as_ref().is_some_and(
            |(_, pending)| matches!(pending, Pending::Checkpoint { tab: id, .. } if *id == tab),
        ) {
            retention.service.cancel();
        }
        true
    }

    /// Reconcile only the captured revision, then schedule bounded checkpoint/discard work.
    pub(super) fn poll_retention(&mut self) -> Result<bool, Error> {
        let Some(mut retention) = self.retention.take() else {
            return Ok(false);
        };
        let result = (|| {
            let mut changed = false;
            if let Some(reply) = retention.service.poll() {
                let pending = retention
                    .pending
                    .take()
                    .filter(|(request, _)| *request == reply.request)
                    .ok_or(Error::CorruptStorage)?
                    .1;
                match (pending, reply.result) {
                    (Pending::RecordError, result) => {
                        if let Err(error) = result {
                            self.event(Event::OpenFailed(format!(
                                "Recovery error annotation failed: {error}"
                            )));
                        }
                    }
                    (Pending::Page { generation, cursor }, Ok(recovery::Value::Page(entries))) => {
                        if generation == retention.list_generation {
                            let before = entries
                                .first()
                                .map(|(entry, _)| (entry.view.order, entry.id))
                                .or(cursor);
                            let after = entries
                                .last()
                                .map(|(entry, _)| (entry.view.order, entry.id))
                                .or(cursor);
                            let previous = before.is_some_and(|(order, _)| order > 0);
                            let next = entries.len() == recovery::PAGE_ENTRIES;
                            let rows = entries
                                .iter()
                                .filter(|(entry, _)| {
                                    !self.tabs.iter().any(|tab| {
                                        self.state(tab.id).is_some_and(|state| {
                                            state
                                                .scratch
                                                .as_ref()
                                                .is_some_and(|scratch| scratch.id == entry.id)
                                        })
                                    })
                                })
                                .map(|(entry, path)| RecoveryRow {
                                    id: entry.id,
                                    label: format!(
                                        "Scratch {} — {} bytes{} — {}",
                                        entry.view.order + 1,
                                        entry.len,
                                        if entry.restore_error.iter().any(|byte| *byte != 0) {
                                            format!(
                                                " — restore failed: {}",
                                                std::str::from_utf8(&entry.restore_error)
                                                    .unwrap_or("invalid text")
                                                    .trim_end_matches('\0')
                                            )
                                        } else {
                                            String::new()
                                        },
                                        crate::text::safe(&path.to_string_lossy())
                                    ),
                                })
                                .collect();
                            retention.listing = entries;
                            self.event(Event::RecoveryPage(
                                generation,
                                Arc::new(RecoveryPage {
                                    rows,
                                    before,
                                    after,
                                    previous,
                                    next,
                                }),
                            ));
                        }
                    }
                    (
                        Pending::DiscardStored {
                            scratch,
                            generation,
                        },
                        Ok(recovery::Value::Discarded),
                    ) => {
                        retention.listing.retain(|(entry, _)| entry.id != scratch);
                        retention.request_page(generation, None, false);
                    }
                    (
                        Pending::DiscardStored {
                            scratch,
                            generation,
                        },
                        Err(error),
                    ) => {
                        if recovery::removal_published(&error) {
                            retention.listing.retain(|(entry, _)| entry.id != scratch);
                            retention.request_page(generation, None, false);
                        }
                        self.event(Event::Failure(format!("Recovery: {error}")));
                    }
                    (Pending::Page { .. }, Err(error)) => {
                        self.event(Event::Failure(format!("Recovery: {error}")));
                    }
                    (
                        Pending::Checkpoint {
                            tab,
                            scratch,
                            revision,
                            view,
                        },
                        Ok(recovery::Value::Checkpoint(entry)),
                    ) => {
                        if entry.id != scratch {
                            return Err(Error::CorruptStorage);
                        }
                        if let Some(state) = self.state_mut(tab)
                            && let Some(current) = &mut state.scratch
                            && current.id == scratch
                        {
                            current.retained = Some((revision, view));
                            current.retry = None;
                            current.warning = None;
                            state.attention = false;
                            state.screen.message = "Scratch checkpoint retained".into();
                        }
                    }
                    (
                        Pending::Discard {
                            tab,
                            scratch,
                            after,
                        },
                        Ok(recovery::Value::Discarded),
                    ) => {
                        if let Some(state) = self.state_mut(tab)
                            && state
                                .scratch
                                .as_ref()
                                .is_some_and(|current| current.id == scratch)
                        {
                            if state.screen.path.is_some() {
                                state.scratch = None;
                            } else if let Some(current) = &mut state.scratch {
                                current.retained = None;
                                current.retiring = after.is_some();
                                if after.is_none() {
                                    current.id = Id::fresh()?;
                                    if retention.active == Some(scratch) {
                                        retention.active = Some(current.id);
                                    }
                                    retention.sweep = true;
                                }
                            }
                        }
                        if let Some((after, sequence)) = after {
                            self.event(Event::Proceed(sequence, after));
                        }
                    }
                    (Pending::Checkpoint { tab, .. }, Err(error)) => {
                        if let Some(state) = self.state_mut(tab) {
                            if let Some(scratch) = &mut state.scratch {
                                scratch.retry = Some(Instant::now() + PERSISTENCE_RETRY);
                            }
                            if error.kind() != std::io::ErrorKind::Interrupted {
                                state.attention = true;
                                state.screen.message =
                                    format!("Scratch persistence failed: {error}");
                                if let Some(scratch) = &mut state.scratch {
                                    scratch.warning = Some(state.screen.message.clone());
                                }
                            }
                        }
                        if retention.quitting.take().is_some() {
                            self.event(Event::PersistenceFailed(tab, After::Quit, format!("Scratch persistence failed: {error}. Current contents remain open; the previous checkpoint remains available.")));
                        }
                    }
                    (Pending::Discard { tab, after, .. }, Err(error)) => {
                        if let Some(state) = self.state_mut(tab) {
                            if let Some(scratch) = &mut state.scratch {
                                scratch.retry = Some(Instant::now() + PERSISTENCE_RETRY);
                                if state.screen.path.is_none()
                                    && recovery::removal_published(&error)
                                {
                                    let old = scratch.id;
                                    scratch.id = Id::fresh()?;
                                    if retention.active == Some(old) {
                                        retention.active = Some(scratch.id);
                                    }
                                    scratch.retained = None;
                                }
                            }
                            state.attention = true;
                            state.screen.message = if state.screen.path.is_some() {
                                format!(
                                    "Saved file retained; older recovery copy may reappear: {error}"
                                )
                            } else {
                                format!(
                                    "Scratch removal failed; current contents remain open: {error}"
                                )
                            };
                            if let Some(scratch) = &mut state.scratch {
                                scratch.warning = Some(state.screen.message.clone());
                            }
                        }
                        if after.is_some() || retention.quitting.take().is_some() {
                            self.event(Event::PersistenceFailed(tab, after.map_or(After::Quit, |(after, _)| after), format!("Scratch removal failed: {error}. Current contents remain open. Retry, export with Save As, or cancel.")));
                        }
                    }
                    _ => return Err(Error::CorruptStorage),
                }
                if let Some(warning) = reply.warning {
                    self.tab.screen.message = warning;
                }
                changed = true;
            }
            if Instant::now() >= retention.due {
                retention.sweep = true;
            }
            if retention.pending.is_none() {
                let close = retention.closes.pop_front();
                let mut operation = close.map(|(tab, scratch, after, sequence)| {
                    (
                        recovery::Request::Discard(scratch),
                        Pending::Discard {
                            tab,
                            scratch,
                            after: Some((after, sequence)),
                        },
                    )
                });
                if operation.is_none()
                    && let Some((scratch, error)) = retention.restore_errors.pop_front()
                {
                    operation = Some((
                        recovery::Request::RecordError(scratch, error),
                        Pending::RecordError,
                    ));
                }
                if operation.is_none()
                    && let Some((scratch, generation)) = retention.stored_discard.take()
                {
                    operation = Some((
                        recovery::Request::Discard(scratch),
                        Pending::DiscardStored {
                            scratch,
                            generation,
                        },
                    ));
                }
                if operation.is_none()
                    && let Some(ListRequest {
                        generation,
                        cursor,
                        previous,
                    }) = retention.list_request.take()
                {
                    operation = Some((
                        recovery::Request::Page { cursor, previous },
                        Pending::Page { generation, cursor },
                    ));
                }
                if operation.is_none() {
                    for step in 0..self.tabs.len() {
                        let index = (retention.cursor + step) % self.tabs.len();
                        let tab = self.tabs[index].id;
                        let Some(state) = self.state(tab) else {
                            continue;
                        };
                        let Some(scratch) = &state.scratch else {
                            continue;
                        };
                        if state.screen.loading || scratch.retiring {
                            continue;
                        }
                        if state.screen.path.is_some() {
                            if scratch.retry.is_some_and(|retry| Instant::now() < retry) {
                                continue;
                            }
                            operation = Some((
                                recovery::Request::Discard(scratch.id),
                                Pending::Discard {
                                    tab,
                                    scratch: scratch.id,
                                    after: None,
                                },
                            ));
                            retention.cursor = index + 1;
                            break;
                        }
                        if !retention.sweep
                            || scratch.retry.is_some_and(|retry| Instant::now() < retry)
                        {
                            continue;
                        }
                        let view = SavedView {
                            caret: state.screen.position.caret,
                            anchor: state.screen.position.anchor,
                            top: state.top,
                            left: state.left,
                            preferred: state.preferred,
                            order: scratch.order,
                            active: retention.active == Some(scratch.id),
                        };
                        let info = state.info.as_ref().ok_or(Error::Busy)?;
                        if scratch.retained == Some((info.revision, view)) {
                            continue;
                        }
                        let (scratch, document, revision, changed_text) = (
                            scratch.id,
                            state.screen.document.ok_or(Error::Busy)?,
                            info.revision,
                            scratch
                                .retained
                                .is_none_or(|(retained, _)| retained != info.revision),
                        );
                        let snapshot = if changed_text {
                            match self.call(Request::BackgroundSnapshot(document))? {
                                Response::Snapshot(snapshot) => Some(snapshot),
                                _ => return Err(Error::CorruptStorage),
                            }
                        } else {
                            None
                        };
                        operation = Some((
                            recovery::Request::Checkpoint {
                                scratch,
                                snapshot,
                                view,
                            },
                            Pending::Checkpoint {
                                tab,
                                scratch,
                                revision,
                                view,
                            },
                        ));
                        retention.cursor = index + 1;
                        break;
                    }
                }
                if let Some((operation, pending)) = operation {
                    self.counter += 1;
                    retention
                        .service
                        .submit(self.counter, operation)
                        .map_err(|_| Error::Busy)?;
                    retention.pending = Some((self.counter, pending));
                    changed = true;
                } else if retention.sweep {
                    let unretained = self.tabs.iter().any(|tab| {
                        self.state(tab.id).is_some_and(|state| {
                            state.scratch.as_ref().is_some_and(|scratch| {
                                state.screen.path.is_none()
                                    && !scratch.retiring
                                    && !state.screen.loading
                                    && (scratch.retry.is_some() || scratch.retained.is_none())
                            })
                        })
                    });
                    if !unretained {
                        retention.sweep = false;
                        retention.due = Instant::now() + retention.interval;
                        if let Some(sequence) = retention.quitting.take() {
                            self.event(Event::QuitReady(sequence));
                        }
                    }
                }
            }
            Ok(changed)
        })();
        self.retention = Some(retention);
        result
    }
}

#[cfg(test)]
pub(super) mod tests;
