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
    /// Recompute an idle deadline from the start of its existing schedule.
    pub(super) fn set_interval(&mut self, now: Instant, interval: Duration) {
        if interval != self.interval && !self.sweep {
            self.due = self.due - self.interval + interval;
            self.sweep = now >= self.due;
        }
        self.interval = interval;
    }

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
        let had_cli = !self.tabs.is_empty();
        let mut active = None;
        let mut first = None;
        while self.tab_slots_used() < crate::TAB_LIMIT
            && self
                .retention
                .as_ref()
                .is_some_and(|retention| !retention.startup.is_empty())
        {
            let id = self.admit_tab(None)?;
            let (entry, path) = self.retention.as_mut().unwrap().startup.remove(0);
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
        // The catalog page and startup overflow can refer to the same entry.
        // Once admitted here, only this tab may publish its persistent identity.
        self.retention
            .as_mut()
            .unwrap()
            .startup
            .retain(|(entry, _)| entry.id != scratch);
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
                            // Freeze promotions until the UI accepts this quit or
                            // cancels it. A superseded acknowledgement must not stop streams.
                            self.quit_ready = true;
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
pub(super) mod tests {
    //! Synchronized lifecycle checks exercise the same coordinator and storage workers as the TUI.

    /// A manually recovered overflow entry and automatic refill must have one owner.
    #[test]
    fn manual_recovery_and_startup_refill_keep_one_scratch_owner() {
        let root = tempfile::tempdir().unwrap();
        let (mut first, _events) = coordinator(root.path(), None);
        first
            .action(Action::Insert("original retained bytes".into()))
            .unwrap();
        first.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut first, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        let retained = first.tab.scratch.as_ref().unwrap().id;
        shutdown(first);

        let paths = (0..crate::TAB_LIMIT)
            .map(|index| {
                let path = root.path().join(format!("valid-{index}"));
                std::fs::write(&path, "file").unwrap();
                path
            })
            .collect();
        let (mut worker, _events) = coordinator_with_paths(root.path(), None, None, paths);
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.opening.is_some() {
            worker.receive().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(worker.tab.screen.document.is_some());
        worker
            .action(Action::RemoveTab(worker.tab.screen.tab.unwrap()))
            .unwrap();
        worker
            .retention
            .as_mut()
            .unwrap()
            .request_page(30, None, false);
        poll_until(&mut worker, |worker| {
            worker
                .retention
                .as_ref()
                .unwrap()
                .listing
                .iter()
                .any(|(entry, _)| entry.id == retained)
        });
        worker
            .action(Action::OpenRetained {
                generation: 30,
                scratch: retained,
            })
            .unwrap();

        let pending = worker.tabs[0].id;
        worker.action(Action::Switch(pending)).unwrap();
        assert!(worker.tab.screen.loading);
        worker.action(Action::CancelOpen(pending)).unwrap();
        let owners = worker
            .tabs
            .iter()
            .filter(|tab| {
                worker
                    .state(tab.id)
                    .and_then(|state| state.scratch.as_ref())
                    .is_some_and(|scratch| scratch.id == retained)
            })
            .count();
        assert_eq!(
            owners, 1,
            "both tabs claimed the same persistent scratch identity"
        );
        finish_opens(&mut worker);
        worker.action(Action::New).unwrap();
        worker
            .action(Action::Insert("independent new scratch".into()))
            .unwrap();
        worker.begin_retained_quit(200);
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        shutdown(worker);
        let store = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        let mut contents: Vec<_> = store
            .page(None)
            .unwrap()
            .iter()
            .map(|entry| std::fs::read_to_string(store.object_path(entry)).unwrap())
            .collect();
        contents.sort();
        assert_eq!(
            contents,
            ["independent new scratch", "original retained bytes"]
        );
    }

    use super::*;
    use crate::document::{FaultInjector, IoPoint, StorageLimits};

    /// Recovery-only gate keeps document I/O and unrelated editor actions independent.
    #[derive(Debug)]
    struct Gate {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
        armed: AtomicBool,
        point: IoPoint,
    }
    impl FaultInjector for Gate {
        fn check(&self, point: IoPoint) -> std::io::Result<()> {
            if point == self.point && self.armed.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    /// Keep the events receiver live so rare lifecycle acknowledgments never block the fixture.
    pub(crate) fn coordinator(
        root: &Path,
        faults: Option<Arc<dyn FaultInjector>>,
    ) -> (Coordinator, Receiver<Event>) {
        coordinator_with_storage(root, faults, None)
    }

    /// Inject independent storage faults while retaining the real checkpoint worker.
    pub(crate) fn coordinator_with_storage(
        root: &Path,
        faults: Option<Arc<dyn FaultInjector>>,
        storage_faults: Option<Arc<dyn FaultInjector>>,
    ) -> (Coordinator, Receiver<Event>) {
        let (mut coordinator, events) =
            coordinator_with_paths(root, faults, storage_faults, Vec::new());
        if coordinator.tabs.is_empty() {
            coordinator.action(Action::New).unwrap();
        }
        (coordinator, events)
    }

    fn coordinator_with_paths(
        root: &Path,
        faults: Option<Arc<dyn FaultInjector>>,
        storage_faults: Option<Arc<dyn FaultInjector>>,
        paths: Vec<PathBuf>,
    ) -> (Coordinator, Receiver<Event>) {
        let (_sender, inbox) = mpsc::sync_channel(256);
        let (events, receiver) = mpsc::sync_channel(256);
        let service = StorageService::new(OpenOptions {
            faults: storage_faults,
            limits: StorageLimits {
                undo_bytes: 64 << 10,
                undo_transactions: 1,
                ..StorageLimits::default()
            },
            ..OpenOptions::default()
        })
        .unwrap();
        let store = recovery::Store::open(recovery::Options {
            root: root.join("state"),
            reserve_bytes: 0,
            interval: Duration::from_secs(3600),
            faults,
        })
        .unwrap();
        let mut coordinator = Coordinator::new(
            service,
            inbox,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        )
        .with_retention(store);
        coordinator.begin();
        coordinator.start(root.to_owned(), paths);
        coordinator.poll_opens().unwrap();
        (coordinator, receiver)
    }

    /// Release every real worker/owner before another instance claims this test profile.
    pub(crate) fn shutdown(mut worker: Coordinator) {
        if let Some(monitor) = worker.monitor.take() {
            monitor.shutdown();
        }
        if let Some(retention) = worker.retention.take()
            && let Some(thread) = retention.service.shutdown()
        {
            thread.join().unwrap();
        }
        worker.service.shutdown().wait().unwrap();
    }

    fn gate() -> (Arc<Gate>, Receiver<()>, SyncSender<()>) {
        let (entered, observed) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        (
            Arc::new(Gate {
                entered,
                release: Mutex::new(released),
                armed: AtomicBool::new(true),
                point: IoPoint::Write,
            }),
            observed,
            release,
        )
    }

    fn poll_until(worker: &mut Coordinator, condition: impl Fn(&Coordinator) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition(worker) {
            worker.poll_retention().unwrap();
            assert!(Instant::now() < deadline, "checkpoint did not settle");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// K07: preserve the schedule origin and an in-flight sweep across interval changes.
    #[test]
    fn checkpoint_interval_updates_an_existing_schedule_without_interrupting_held_quit() {
        use crate::settings::{Scalar, Values};
        let root = tempfile::tempdir().unwrap();
        let (faults, observed, release) = gate();
        let (mut worker, events) = coordinator(root.path(), Some(faults));
        worker.action(Action::Insert("before".into())).unwrap();
        let now = Instant::now();
        let origin = now - Duration::from_secs(5);
        let retention = worker.retention.as_mut().unwrap();
        retention.due = origin + retention.interval;
        retention.set_interval(now, Duration::from_secs(8));
        assert_eq!(retention.due, origin + Duration::from_secs(8));
        retention.set_interval(now + Duration::from_secs(1), Duration::from_secs(8));
        assert_eq!(
            retention.due,
            origin + Duration::from_secs(8),
            "unrelated updates cannot postpone it"
        );
        retention.set_interval(now, Duration::from_secs(4));
        let due = retention.due;
        assert!(retention.sweep);
        worker.poll_retention().unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();

        let mut updated = Values::default();
        updated.set(Scalar::Checkpoint, Some(12));
        worker.action(Action::ApplySettings(updated, 7)).unwrap();
        assert_eq!(worker.retention.as_ref().unwrap().due, due);
        worker.action(Action::Insert(" after".into())).unwrap();
        worker.begin_retained_quit(88);
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::QuitReady(88)))
        );
        release.send(()).unwrap();
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        let retention = worker.retention.as_ref().unwrap();
        assert_eq!(retention.interval, Duration::from_secs(12));
        assert!(retention.due > Instant::now() + Duration::from_secs(10));
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::QuitReady(88)))
        );
        shutdown(worker);
        let store = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        let entries = store.page(None).unwrap();
        assert_eq!(
            std::fs::read(store.object_path(&entries[0])).unwrap(),
            b"before after"
        );
    }

    /// R02: persistence of A cannot acknowledge a newer edit B as retained.
    #[test]
    fn gated_checkpoint_acknowledges_only_its_captured_revision() {
        let root = tempfile::tempdir().unwrap();
        let (faults, observed, release) = gate();
        let (mut worker, _events) = coordinator(root.path(), Some(faults));
        worker.action(Action::Insert("A".into())).unwrap();
        let first = worker.tab.info.as_ref().unwrap().revision;
        worker.retention.as_mut().unwrap().sweep = true;
        worker.poll_retention().unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.action(Action::Insert("B".into())).unwrap();
        let second = worker.tab.info.as_ref().unwrap().revision;
        assert_ne!(first, second);
        release.send(()).unwrap();
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        assert_eq!(
            worker.tab.scratch.as_ref().unwrap().retained.unwrap().0,
            first
        );
        assert_ne!(
            worker.tab.scratch.as_ref().unwrap().retained.unwrap().0,
            second
        );
        poll_until(&mut worker, |worker| {
            worker
                .tab
                .scratch
                .as_ref()
                .unwrap()
                .retained
                .is_some_and(|(revision, _)| revision == second)
        });
        shutdown(worker);
    }

    /// R07: Quit is not acknowledged until its final text publication succeeds.
    #[test]
    fn final_quit_waits_for_checkpoint_publication() {
        let root = tempfile::tempdir().unwrap();
        let (faults, observed, release) = gate();
        let (mut worker, events) = coordinator(root.path(), Some(faults));
        worker
            .action(Action::Insert("final acknowledged text".into()))
            .unwrap();
        worker.begin_retained_quit(88);
        worker.poll_retention().unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::QuitReady(88))),
            "Quit acknowledged blocked publication"
        );
        release.send(()).unwrap();
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::QuitReady(88)))
        );
        shutdown(worker);
        let store = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        let entries = store.page(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            std::fs::read(store.object_path(&entries[0])).unwrap(),
            b"final acknowledged text"
        );
    }

    /// R02: required compaction cancels the actual checkpoint lease before accepting the next edit.
    #[test]
    fn checkpoint_lease_yields_to_same_scratch_compaction() {
        let root = tempfile::tempdir().unwrap();
        let (faults, observed, release) = gate();
        let (mut worker, _events) = coordinator(root.path(), Some(faults));
        for index in 0..40 {
            worker.action(Action::Command(Command::SelectAll)).unwrap();
            worker
                .action(Action::Insert(
                    if index % 2 == 0 { "a" } else { "b" }.repeat(64 << 10),
                ))
                .unwrap();
            if worker.tab.info.as_ref().unwrap().compaction_pending {
                break;
            }
        }
        assert!(
            worker.tab.info.as_ref().unwrap().compaction_pending,
            "fixture must require real compaction"
        );
        worker.retention.as_mut().unwrap().sweep = true;
        worker.poll_retention().unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let cancel = worker
            .retention
            .as_ref()
            .unwrap()
            .service
            .active_cancellation()
            .unwrap();
        let (completed, completion) = mpsc::sync_channel(1);
        let editing = thread::spawn(move || {
            worker.action(Action::Command(Command::SelectAll)).unwrap();
            worker
                .action(Action::Insert("survives compaction".into()))
                .unwrap();
            completed.send(()).unwrap();
            worker
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cancel.is_cancelled() {
            assert!(
                Instant::now() < deadline,
                "edit did not cancel its checkpoint lease"
            );
            thread::yield_now();
        }
        let progressed = completion.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        let mut worker = editing.join().unwrap();
        assert!(
            progressed.is_ok(),
            "accepted edit waited for blocked checkpoint I/O"
        );
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "survives compaction"
        );
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().pending.is_none()
        });
        worker.tab.scratch.as_mut().unwrap().retry = None;
        worker.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut worker, |worker| {
            worker
                .tab
                .scratch
                .as_ref()
                .unwrap()
                .retained
                .is_some_and(|(revision, _)| revision == worker.tab.info.as_ref().unwrap().revision)
        });
        shutdown(worker);
    }

    /// R06: an empty scratch still needs an explicit Close decision and confirmed retirement.
    #[test]
    fn empty_scratch_close_is_guarded_and_discard_does_not_reappear() {
        let root = tempfile::tempdir().unwrap();
        let (mut worker, events) = coordinator(root.path(), None);
        worker.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        let tab = worker.tab.screen.tab.unwrap();
        worker.guard(After::Close(tab)).unwrap();
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Guard(_, After::Close(id), true) if id == tab)
        );
        worker.discard(After::Close(tab), None);
        poll_until(&mut worker, |worker| {
            worker
                .tab
                .scratch
                .as_ref()
                .is_some_and(|scratch| scratch.retiring)
        });
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Proceed(_, After::Close(id)) if id == tab)
        );
        worker.action(Action::RemoveTab(tab)).unwrap();
        shutdown(worker);
        let retained = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        assert!(retained.page(None).unwrap().is_empty());
    }

    /// An admitted Cut after catalog removal must revoke the retired scratch identity.
    #[test]
    fn clipboard_cut_revokes_already_retired_scratch_close() {
        let root = tempfile::tempdir().unwrap();
        let (mut worker, events) = coordinator(root.path(), None);
        worker
            .action(Action::Insert("preserve this".into()))
            .unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        let tab = worker.tab.screen.tab.unwrap();
        let original = worker.tab.scratch.as_ref().unwrap().id;
        worker.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        worker.discard(After::Close(tab), None);
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retiring
        });
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Proceed(_, After::Close(id)) if id == tab)
        );
        worker
            .action(Action::Clipboard {
                cut: true,
                internal: true,
                system: Some(1),
            })
            .unwrap();
        let scratch = worker.tab.scratch.as_ref().unwrap();
        assert!(
            !scratch.retiring,
            "Cut retained stale scratch retirement authority"
        );
        assert_ne!(scratch.id, original);
        assert!(worker.snapshot().unwrap().is_empty());
        worker.action(Action::Command(Command::Paste)).unwrap();
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), "preserve this".len())
                .unwrap()
                .text(),
            "preserve this"
        );
        shutdown(worker);
    }

    /// Toggle a real post-publication failure without changing the application or test environment.
    #[derive(Debug, Default)]
    struct UncertainRemoval {
        fail: AtomicBool,
        attempts: AtomicUsize,
    }
    impl FaultInjector for UncertainRemoval {
        fn check(&self, point: IoPoint) -> std::io::Result<()> {
            if point == IoPoint::DirectorySync && self.fail.load(Ordering::Acquire) {
                self.attempts.fetch_add(1, Ordering::AcqRel);
                Err(std::io::Error::other("injected uncertain removal"))
            } else {
                Ok(())
            }
        }
    }

    /// R07: a potentially committed discard failure leaves production editing and recovery usable.
    #[test]
    fn uncertain_discard_keeps_editing_and_recheckpoints_under_fresh_identity() {
        let root = tempfile::tempdir().unwrap();
        let faults = Arc::new(UncertainRemoval::default());
        let (mut worker, events) = coordinator(root.path(), Some(faults.clone()));
        worker.action(Action::Insert("A".into())).unwrap();
        worker.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        let original = worker.tab.scratch.as_ref().unwrap().id;
        let tab = worker.tab.screen.tab.unwrap();
        faults.fail.store(true, Ordering::Release);
        worker.discard(After::Close(tab), None);
        poll_until(&mut worker, |worker| {
            worker
                .tab
                .scratch
                .as_ref()
                .is_some_and(|scratch| scratch.id != original)
        });
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::PersistenceFailed(id, After::Close(target), _) if id == tab && target == tab)
        );
        let replacement = worker.tab.scratch.as_ref().unwrap().id;
        faults.fail.store(false, Ordering::Release);
        worker.action(Action::Insert("B".into())).unwrap();
        let revision = worker.tab.info.as_ref().unwrap().revision;
        worker.begin_retained_quit(77);
        poll_until(&mut worker, |worker| {
            worker
                .tab
                .scratch
                .as_ref()
                .unwrap()
                .retained
                .is_some_and(|(retained, _)| retained == revision)
        });
        assert_eq!(worker.tab.scratch.as_ref().unwrap().id, replacement);
        shutdown(worker);
        let restored = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        let entries = restored.page(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, replacement);
        assert!(entries[0].view.active);
        assert_eq!(
            std::fs::read(restored.object_path(&entries[0])).unwrap(),
            b"AB"
        );
    }

    /// R06/R07: a failed named conversion cleanup retries on its deadline, without modal flooding.
    #[test]
    fn failed_save_as_retirement_is_throttled_and_stays_nonmodal() {
        let root = tempfile::tempdir().unwrap();
        let faults = Arc::new(UncertainRemoval::default());
        let (mut worker, events) = coordinator(root.path(), Some(faults.clone()));
        worker.action(Action::Insert("exported".into())).unwrap();
        worker.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut worker, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        faults.fail.store(true, Ordering::Release);
        let path = root.path().join("export.txt");
        worker.action(Action::Save(Some(path.clone()))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker
            .tab
            .scratch
            .as_ref()
            .is_none_or(|scratch| scratch.retry.is_none())
        {
            worker.poll_background();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "exported");
        let attempts = faults.attempts.load(Ordering::Acquire);
        assert_eq!(attempts, 1);
        for _ in 0..50 {
            worker.poll_retention().unwrap();
        }
        assert_eq!(faults.attempts.load(Ordering::Acquire), attempts);
        worker
            .action(Action::Insert(" still editable".into()))
            .unwrap();
        worker.summarize();
        assert!(worker.summaries.iter().any(|tab| {
            tab.warning
                .as_deref()
                .is_some_and(|warning| warning.contains("older recovery copy may reappear"))
        }));
        assert!(
            worker.tab.scratch.as_ref().unwrap().retry.unwrap()
                > Instant::now() + Duration::from_secs(4)
        );
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::Failure(_) | Event::PersistenceFailed(..)))
        );
        worker.tab.scratch.as_mut().unwrap().retry = Some(Instant::now() - Duration::from_secs(1));
        worker.poll_retention().unwrap();
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().pending.is_none()
        });
        assert_eq!(faults.attempts.load(Ordering::Acquire), attempts + 1);
        shutdown(worker);
    }

    /// A canceled Close must keep the surviving tab recoverable, even after publication.
    #[test]
    fn cancel_during_discard_restores_retention_authority() {
        for cancel_before_reply in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let (entered, observed) = mpsc::sync_channel(1);
            let (release, released) = mpsc::sync_channel(1);
            let faults = Arc::new(Gate {
                entered,
                release: Mutex::new(released),
                armed: AtomicBool::new(false),
                point: IoPoint::Publish,
            });
            let (mut worker, _events) = coordinator(root.path(), Some(faults.clone()));
            worker.action(Action::Insert("preserve".into())).unwrap();
            worker.retention.as_mut().unwrap().sweep = true;
            poll_until(&mut worker, |worker| {
                worker.tab.scratch.as_ref().unwrap().retained.is_some()
            });
            let original = worker.tab.scratch.as_ref().unwrap().id;
            let tab = worker.tab.screen.tab.unwrap();
            faults.armed.store(true, Ordering::Release);
            worker.discard(After::Close(tab), None);
            worker.poll_retention().unwrap();
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            if cancel_before_reply {
                worker.action(Action::CancelGuard).unwrap();
            }
            release.send(()).unwrap();
            poll_until(&mut worker, |worker| {
                worker.retention.as_ref().unwrap().pending.is_none()
            });
            if !cancel_before_reply {
                worker.action(Action::CancelGuard).unwrap();
            }
            assert!(
                worker.tab.scratch.is_some(),
                "canceled close lost scratch identity"
            );
            assert_ne!(worker.tab.scratch.as_ref().unwrap().id, original);
            worker.action(Action::Insert(" next".into())).unwrap();
            worker.begin_retained_quit(90);
            let revision = worker.tab.info.as_ref().unwrap().revision;
            poll_until(&mut worker, |worker| {
                worker
                    .tab
                    .scratch
                    .as_ref()
                    .unwrap()
                    .retained
                    .is_some_and(|(saved, _)| saved == revision)
            });
            shutdown(worker);
            let store = recovery::Store::open(recovery::Options {
                root: root.path().join("state"),
                reserve_bytes: 0,
                ..recovery::Options::default()
            })
            .unwrap();
            let entries = store.page(None).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(
                std::fs::read(store.object_path(&entries[0])).unwrap(),
                b"preserve next"
            );
        }
    }

    /// An accepted stored discard revokes Open before its catalog I/O can finish.
    #[test]
    fn stored_discard_blocks_concurrent_open() {
        for uncertain in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (mut first, _events) = coordinator(root.path(), None);
            first.action(Action::Insert("retained".into())).unwrap();
            first.retention.as_mut().unwrap().sweep = true;
            poll_until(&mut first, |worker| {
                worker.tab.scratch.as_ref().unwrap().retained.is_some()
            });
            shutdown(first);
            let faults = Arc::new(UncertainRemoval::default());
            let (mut worker, _events) = coordinator(root.path(), Some(faults.clone()));
            let entry = worker
                .tab
                .scratch
                .as_ref()
                .unwrap()
                .restore
                .as_ref()
                .unwrap()
                .clone();
            let tab = worker.tab.screen.tab.unwrap();
            worker.remove_tab(tab).unwrap();
            let retention = worker.retention.as_mut().unwrap();
            retention.list_generation = 19;
            retention.listing = vec![entry.clone()];
            faults.fail.store(uncertain, Ordering::Release);
            worker.discard_retained(19, entry.0.id).unwrap();
            assert!(
                matches!(
                    worker.open_retained(19, entry.0.id),
                    Err(Error::Busy | Error::Cancelled)
                ),
                "queued discard allowed the same row to open"
            );
            worker.poll_retention().unwrap();
            assert!(
                matches!(
                    worker.open_retained(19, entry.0.id),
                    Err(Error::Busy | Error::Cancelled)
                ),
                "running discard allowed the same row to open"
            );
            poll_until(&mut worker, |worker| {
                worker.retention.as_ref().unwrap().pending.is_none()
            });
            assert!(
                worker.open_retained(19, entry.0.id).is_err(),
                "completed or uncertain discard restored tombstoned identity"
            );
            shutdown(worker);
        }
    }

    /// Failed CLI opens release their reserved capacity for startup recovery.
    #[test]
    fn failed_cli_opens_release_capacity_for_recovery() {
        let root = tempfile::tempdir().unwrap();
        let (mut first, _events) = coordinator(root.path(), None);
        first.action(Action::Insert("recover me".into())).unwrap();
        first.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut first, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        let retained = first.tab.scratch.as_ref().unwrap().id;
        shutdown(first);
        let paths = (0..crate::TAB_LIMIT)
            .map(|i| root.path().join(format!("missing-{i}")))
            .collect();
        let (mut worker, _events) = coordinator_with_paths(root.path(), None, None, paths);
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.opening.is_some()
            || worker
                .tabs
                .iter()
                .any(|tab| worker.state(tab.id).unwrap().screen.loading)
        {
            worker.receive().unwrap();
            worker.poll_opens().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(worker.tabs.len(), 1);
        assert_eq!(worker.tab.scratch.as_ref().unwrap().id, retained);
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "recover me"
        );
        shutdown(worker);
    }

    /// An in-flight cancellation keeps its charge, then hands that slot to retained work.
    #[test]
    fn cancelled_startup_open_releases_slot_for_recovery_after_settlement() {
        let root = tempfile::tempdir().unwrap();
        let (mut first, _events) = coordinator(root.path(), None);
        first
            .action(Action::Insert("recover after cancellation".into()))
            .unwrap();
        first.retention.as_mut().unwrap().sweep = true;
        poll_until(&mut first, |worker| {
            worker.tab.scratch.as_ref().unwrap().retained.is_some()
        });
        let retained = first.tab.scratch.as_ref().unwrap().id;
        shutdown(first);

        let paths: Vec<_> = (0..crate::TAB_LIMIT)
            .map(|index| {
                let path = root.path().join(format!("input-{index}"));
                std::fs::write(&path, "candidate").unwrap();
                path
            })
            .collect();
        let (entered, reached) = mpsc::sync_channel(1);
        let (release, held) = mpsc::sync_channel(1);
        let gate = Arc::new(Gate {
            entered,
            release: Mutex::new(held),
            armed: AtomicBool::new(true),
            point: IoPoint::Read,
        });
        let (mut worker, _events) = coordinator_with_paths(root.path(), None, Some(gate), paths);
        let tab = worker.tab.screen.tab.unwrap();
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.cancel_open(tab).unwrap();
        assert_eq!(worker.tab_slots_used(), crate::TAB_LIMIT);
        assert_eq!(worker.retention.as_ref().unwrap().startup.len(), 1);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            worker.receive().unwrap();
            if worker
                .tabs
                .iter()
                .filter_map(|tab| worker.state(tab.id))
                .any(|state| {
                    state
                        .scratch
                        .as_ref()
                        .is_some_and(|scratch| scratch.id == retained)
                })
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cancelled open did not release recovery capacity"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(worker.tabs.len(), crate::TAB_LIMIT);
        shutdown(worker);
    }

    /// Drain actual asynchronous preparation, rather than bypassing owner acknowledgment.
    fn finish_opens(worker: &mut Coordinator) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            worker.receive().unwrap();
            worker.poll_opens().unwrap();
            if worker.opening.is_none()
                && !worker
                    .tabs
                    .iter()
                    .any(|tab| worker.state(tab.id).unwrap().screen.loading)
            {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// R01: view/order/history survive final flush, including a launch with named files active.
    #[test]
    fn scratch_views_and_remembered_active_survive_named_launch() {
        let root = tempfile::tempdir().unwrap();
        let (mut first, _events) = coordinator(root.path(), None);
        first.action(Action::New).unwrap();
        let a = first.tab.screen.tab.unwrap();
        first
            .action(Action::Insert(
                "界e\u{301} abcdefghijklmnopqrstuvwxyz\n".repeat(40),
            ))
            .unwrap();
        first.tab.screen.position = Position {
            caret: 7,
            anchor: 3,
        };
        first.tab.top = 10;
        first.tab.left = 2;
        first.tab.preferred = Some(9);
        let id = first.tab.scratch.as_ref().unwrap().id;
        first.action(Action::New).unwrap();
        first.switch_tab(a).unwrap();
        let expected = (
            first.tab.screen.position,
            first.tab.top,
            first.tab.left,
            first.tab.preferred,
        );
        first.begin_retained_quit(3);
        poll_until(&mut first, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        shutdown(first);
        let named = root.path().join("named");
        std::fs::write(&named, "named").unwrap();
        let (mut second, _events) =
            coordinator_with_paths(root.path(), None, None, vec![named.clone()]);
        finish_opens(&mut second);
        assert_eq!(second.tab.screen.path.as_deref(), Some(named.as_path()));
        let restored = second
            .tabs
            .iter()
            .find_map(|tab| {
                second.state(tab.id).filter(|state| {
                    state
                        .scratch
                        .as_ref()
                        .is_some_and(|scratch| scratch.id == id)
                })
            })
            .unwrap();
        assert_eq!(
            (
                restored.screen.position,
                restored.top,
                restored.left,
                restored.preferred
            ),
            expected
        );
        assert!(restored.undo_views.is_empty());
        second.begin_retained_quit(4);
        poll_until(&mut second, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        shutdown(second);
        let (mut third, _events) = coordinator(root.path(), None);
        finish_opens(&mut third);
        assert_eq!(third.tabs.len(), 3);
        assert_eq!(
            third.tab.scratch.as_ref().unwrap().id,
            id,
            "named launch erased remembered scratch"
        );
        assert_eq!(
            (
                third.tab.screen.position,
                third.tab.top,
                third.tab.left,
                third.tab.preferred
            ),
            expected
        );
        let before = third.tab.info.as_ref().unwrap().revision;
        third.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(third.tab.info.as_ref().unwrap().revision, before);
        shutdown(third);
    }

    /// R07: a malformed object keeps its entry, error, and actionable recovery location.
    #[test]
    fn failed_object_retains_diagnostic_and_recovery_location() {
        let root = tempfile::tempdir().unwrap();
        let (mut first, _events) = coordinator(root.path(), None);
        first.action(Action::Insert("text".into())).unwrap();
        first.begin_retained_quit(1);
        poll_until(&mut first, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        shutdown(first);
        let (mut worker, events) = coordinator(root.path(), None);
        let (entry, path) = worker
            .tab
            .scratch
            .as_ref()
            .unwrap()
            .restore
            .as_ref()
            .unwrap()
            .clone();
        std::fs::write(&path, b"\xffbad").unwrap();
        finish_opens(&mut worker);
        let diagnostics: Vec<_> = events.try_iter().collect();
        assert!(diagnostics.iter().any(|event| matches!(event, Event::OpenFailed(message) if message.contains(&*path.to_string_lossy()))), "failed restore lost object path: {diagnostics:?}");
        worker
            .retention
            .as_mut()
            .unwrap()
            .request_page(11, None, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            worker.poll_retention().unwrap();
            if let Some(page) = events.try_iter().find_map(|event| match event {
                Event::RecoveryPage(11, page) => Some(page),
                _ => None,
            }) {
                let row = page.rows.iter().find(|row| row.id == entry.id).unwrap();
                assert!(
                    row.label.contains("failed") && row.label.contains(&*path.to_string_lossy())
                );
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        std::fs::write(&path, b"text").unwrap();
        worker.open_retained(11, entry.id).unwrap();
        finish_opens(&mut worker);
        worker.begin_retained_quit(2);
        poll_until(&mut worker, |worker| {
            worker.retention.as_ref().unwrap().quitting.is_none()
        });
        shutdown(worker);
        let store = recovery::Store::open(recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..recovery::Options::default()
        })
        .unwrap();
        let restored = store.page(None).unwrap();
        assert_eq!(
            restored
                .iter()
                .find(|item| item.id == entry.id)
                .unwrap()
                .restore_error,
            [0; 28],
            "successful Retry retained a stale failure"
        );
    }
}
