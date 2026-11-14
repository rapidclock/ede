//! Periodic observations, private-generation preparation, and short guarded refresh handoffs.

use super::*;
use std::{io, time::Instant};

/// Each tab retains its observed version, stability check, and bounded retry state.
#[derive(Default)]
pub(super) struct Watch {
    observed: Option<document::FileObservation>,
    stable: Option<Instant>,
    confirmed: bool,
    retry: Option<Instant>,
    failures: u8,
    pub(super) warning: Option<String>,
}

struct Target {
    tab: TabId,
    document: DocumentId,
    path: PathBuf,
}
struct Observation {
    target: Target,
    result: Result<document::FileObservation, String>,
}

/// All view work is completed before the owner may publish a replacement.
#[derive(Clone, Copy)]
struct RefreshView {
    before: Position,
    position: Position,
    top: u64,
    left: u64,
    preferred: Option<u64>,
    publication: u64,
}

enum PreparedStage {
    Root(document::PreparedRefresh),
    Mapped {
        prepared: document::PreparedRefresh,
        view: Result<RefreshView, Error>,
    },
}

struct Preparing {
    tab: TabId,
    document: DocumentId,
    path: PathBuf,
    revision: document::Revision,
    observation: document::FileObservation,
    cancel: Cancellation,
    worker: Option<JoinHandle<Result<PreparedStage, Error>>>,
    ready: Option<document::PreparedRefresh>,
    progress: Arc<Mutex<Option<document::RefreshProgress>>>,
}

/// One metadata worker and one preparation/mapping worker, with bounded retained state.
pub(super) struct Monitor {
    sender: Option<SyncSender<Vec<Target>>>,
    inbox: Receiver<Vec<Observation>>,
    worker: Option<JoinHandle<()>>,
    probing: bool,
    due: Instant,
    preparing: Option<Preparing>,
    cursor: usize,
    _permit: document::Permit,
}

impl Monitor {
    pub(super) fn new(permit: document::Permit) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Vec<Target>>(1);
        let (outbox, inbox) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("ede-file-monitor".into())
            .spawn(move || {
                for targets in receiver {
                    let observations = targets
                        .into_iter()
                        .map(|target| {
                            let result =
                                document::observe(&target.path).map_err(|error| error.to_string());
                            Observation { target, result }
                        })
                        .collect();
                    if outbox.send(observations).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            inbox,
            worker: Some(worker),
            probing: false,
            due: Instant::now(),
            preparing: None,
            cursor: 0,
            _permit: permit,
        })
    }

    /// Join only on coordinator shutdown, after terminal restoration can proceed independently.
    pub(super) fn shutdown(mut self) {
        self.sender.take();
        if let Some(mut preparing) = self.preparing.take() {
            preparing.cancel.cancel();
            if let Some(worker) = preparing.worker.take() {
                let _ = worker.join();
            }
        }
        let worker = self.worker.take();
        drop(self);
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(preparing) = &self.preparing {
            preparing.cancel.cancel();
        }
    }
}

/// Fallible reads, grapheme mapping, and source validation all precede publication.
fn map_view(
    prepared: &mut document::PreparedRefresh,
    mut view: RefreshView,
    admitted: Arc<AtomicU64>,
    cancel: &Cancellation,
) -> Result<RefreshView, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !view.publication.is_multiple_of(2) {
        return Err(Error::Busy);
    }
    if prepared.changed() {
        let mut old = Text::new(prepared.before(), cancel);
        let caret = old.grapheme_coordinates(view.before.caret)?;
        let anchor = old.grapheme_coordinates(view.before.anchor)?;
        let snapshot = prepared.view_snapshot()?;
        let mut text = Text::new(&snapshot, cancel);
        view.position = Position {
            caret: text.at_grapheme(caret.0.min(snapshot.line_count() - 1), caret.1)?,
            anchor: text.at_grapheme(anchor.0.min(snapshot.line_count() - 1), anchor.1)?,
        };
        view.top = view.top.min(snapshot.line_count() - 1);
    }
    prepared.guard_admission(admitted, view.publication);
    prepared.validate_source(cancel)?;
    Ok(view)
}

impl Coordinator {
    /// Background readers own old immutable generations, so cancellation never waits on their I/O.
    pub(super) fn yield_refresh_for_edit(&mut self, tab: TabId) -> Result<(), Error> {
        if let Some(preparing) = self
            .monitor
            .as_ref()
            .and_then(|monitor| monitor.preparing.as_ref())
            .filter(|preparing| preparing.tab == tab)
        {
            preparing.cancel.cancel();
        }
        Ok(())
    }

    pub(super) fn retry_refresh(&mut self, tab: TabId) -> Result<(), Error> {
        let state = self.state_mut(tab).ok_or(Error::Closed)?;
        if state.screen.path.is_none() {
            return Ok(());
        }
        if state.screen.modified {
            state.screen.message =
                "Local edits prevent refresh; use Save As or close/reopen".into();
            return Ok(());
        }
        state.watch.retry = None;
        state.watch.failures = 0;
        if let Some(monitor) = &mut self.monitor {
            monitor.due = Instant::now();
        }
        Ok(())
    }

    fn refresh_eligible(&self, preparing: &Preparing) -> bool {
        self.state(preparing.tab).is_some_and(|state| {
            state.screen.document == Some(preparing.document)
                && !state.screen.modified
                && state.screen.path.as_deref() == Some(&preparing.path)
                && state
                    .info
                    .as_ref()
                    .is_some_and(|info| info.revision == preparing.revision)
                && state.pending_edit.is_none()
                && state.watch.confirmed
                && state.watch.observed.as_ref() == Some(&preparing.observation)
                && !self.view_cancel.receiving_for(preparing.tab)
        }) && !self.saves.iter().any(|save| save.tab == preparing.tab)
            && !self
                .saving
                .as_ref()
                .is_some_and(|save| save.intent.tab == preparing.tab)
    }

    fn refresh_view(&self, tab: TabId) -> Result<RefreshView, Error> {
        let state = self.state(tab).ok_or(Error::Closed)?;
        Ok(RefreshView {
            before: state.screen.position,
            position: state.screen.position,
            top: state.top,
            left: state.left,
            preferred: state.preferred,
            publication: state.input.value.load(Ordering::Acquire),
        })
    }

    fn refresh_failed(&mut self, tab: TabId, error: Error) {
        if matches!(error, Error::Cancelled | Error::StaleRevision | Error::Busy) {
            return;
        }
        if let Some(state) = self.state_mut(tab) {
            state.watch.failures = state.watch.failures.saturating_add(1);
            let seconds = (1u64 << state.watch.failures.saturating_sub(1).min(5)).min(30);
            state.watch.retry = Some(Instant::now() + Duration::from_secs(seconds));
            let warning = format!("File refresh failed: {error}");
            state.watch.warning = Some(warning.clone());
            state.screen.message = warning;
        }
    }

    /// Observe and prepare without letting filesystem work occupy the interactive coordinator.
    pub(super) fn poll_refreshes(&mut self) -> Result<bool, Error> {
        let Some(mut monitor) = self.monitor.take() else {
            return Ok(false);
        };
        let result = (|| {
            let mut changed = false;
            if let Ok(observations) = monitor.inbox.try_recv() {
                monitor.probing = false;
                for observation in observations {
                    if let Some(preparing) = monitor
                        .preparing
                        .as_ref()
                        .filter(|preparing| preparing.tab == observation.target.tab)
                        && !observation
                            .result
                            .as_ref()
                            .is_ok_and(|observed| observed == &preparing.observation)
                    {
                        preparing.cancel.cancel();
                    }
                    let Some(state) = self.state_mut(observation.target.tab) else {
                        continue;
                    };
                    if state.screen.document != Some(observation.target.document)
                        || state.screen.path.as_deref() != Some(&observation.target.path)
                    {
                        continue;
                    }
                    let watch = &mut state.watch;
                    match observation.result {
                        Ok(observed)
                            if state
                                .info
                                .as_ref()
                                .and_then(|info| info.file_observation.as_ref())
                                == Some(&observed) =>
                        {
                            watch.observed = None;
                            watch.stable = None;
                            watch.confirmed = false;
                            watch.retry = None;
                            watch.failures = 0;
                            watch.warning = None;
                        }
                        Ok(observed) => {
                            if watch.observed.as_ref() != Some(&observed) {
                                watch.observed = Some(observed);
                                watch.stable = Some(Instant::now() + Duration::from_millis(250));
                                watch.confirmed = false;
                                watch.retry = None;
                                watch.failures = 0;
                                monitor.due =
                                    monitor.due.min(Instant::now() + Duration::from_millis(250));
                            } else if watch.stable.is_some_and(|stable| Instant::now() >= stable) {
                                watch.confirmed = true;
                            }
                            if state.screen.modified || watch.failures == 0 {
                                watch.warning = Some(
                                    if state.screen.modified {
                                        "Changed on disk; local edits preserved"
                                    } else {
                                        "Changed on disk; refreshing"
                                    }
                                    .into(),
                                );
                            }
                        }
                        Err(error) => {
                            watch.warning = Some(format!("File refresh unavailable: {error}"));
                            watch.confirmed = false;
                        }
                    }
                    if let Some(warning) = &watch.warning {
                        state.screen.message = warning.clone();
                    }
                    changed = true;
                }
            }
            if let Some(preparing) = &monitor.preparing
                && let Some(progress) = preparing
                    .progress
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take())
                && let Some(state) = self.state_mut(preparing.tab)
            {
                state.screen.message = format!(
                    "Refresh {} {}/{} bytes",
                    if progress.importing {
                        "import"
                    } else {
                        "preparation"
                    },
                    progress.completed,
                    progress.total
                );
                changed = true;
            }
            if monitor.preparing.as_ref().is_some_and(|preparing| {
                preparing.ready.is_some()
                    || preparing
                        .worker
                        .as_ref()
                        .is_some_and(JoinHandle::is_finished)
            }) {
                let mut preparing = monitor.preparing.take().ok_or(Error::CorruptStorage)?;
                let stage = if let Some(ready) = preparing.ready.take() {
                    Ok(PreparedStage::Root(ready))
                } else {
                    preparing
                        .worker
                        .take()
                        .ok_or(Error::CorruptStorage)?
                        .join()
                        .unwrap_or(Err(Error::CorruptStorage))
                };
                if self.refresh_eligible(&preparing) {
                    match stage {
                        Ok(PreparedStage::Root(prepared)) => {
                            if self
                                .state(preparing.tab)
                                .ok_or(Error::Closed)?
                                .input
                                .pending
                                .load(Ordering::Acquire)
                                != 0
                            {
                                preparing.ready = Some(prepared);
                            } else {
                                let view = self.refresh_view(preparing.tab)?;
                                let admitted = self
                                    .state(preparing.tab)
                                    .ok_or(Error::Closed)?
                                    .input
                                    .value
                                    .clone();
                                let cancel = Cancellation::default()
                                    .for_epoch(admitted.clone(), view.publication);
                                let running = cancel.clone();
                                preparing.cancel = cancel;
                                preparing.worker = Some(
                                    thread::Builder::new()
                                        .name("ede-refresh-view".into())
                                        .spawn(move || {
                                            let mut prepared = prepared;
                                            let view =
                                                map_view(&mut prepared, view, admitted, &running);
                                            Ok(PreparedStage::Mapped { prepared, view })
                                        })?,
                                );
                            }
                            monitor.preparing = Some(preparing);
                        }
                        Ok(PreparedStage::Mapped { prepared, view }) => match view {
                            Ok(view)
                                if self.state(preparing.tab).is_some_and(|state| {
                                    state.input.pending.load(Ordering::Acquire) == 0
                                        && state.input.value.load(Ordering::Acquire)
                                            == view.publication
                                }) =>
                            {
                                if let Err(error) = self.commit_refresh(
                                    preparing.tab,
                                    preparing.document,
                                    prepared,
                                    view,
                                ) {
                                    self.refresh_failed(preparing.tab, error);
                                }
                            }
                            Ok(_) | Err(Error::Cancelled | Error::StaleRevision) => {
                                preparing.ready = Some(prepared);
                                monitor.preparing = Some(preparing);
                            }
                            Err(error) => self.refresh_failed(preparing.tab, error),
                        },
                        Err(error) => self.refresh_failed(preparing.tab, error),
                    }
                }
                changed = true;
            }
            if !monitor.probing && Instant::now() >= monitor.due {
                let targets = self
                    .tabs
                    .iter()
                    .filter_map(|tab| {
                        let state = self.state(tab.id)?;
                        if state.screen.loading {
                            return None;
                        }
                        Some(Target {
                            tab: tab.id,
                            document: state.screen.document?,
                            path: state.screen.path.clone()?,
                        })
                    })
                    .collect::<Vec<_>>();
                if !targets.is_empty() {
                    monitor
                        .sender
                        .as_ref()
                        .ok_or(Error::Closed)?
                        .try_send(targets)
                        .map_err(|_| Error::Busy)?;
                    monitor.probing = true;
                }
                monitor.due = Instant::now() + Duration::from_secs(1);
            }
            if monitor.preparing.is_none()
                && self.opening.is_none()
                && !self
                    .tabs
                    .iter()
                    .any(|tab| self.state(tab.id).is_some_and(|state| state.screen.loading))
            {
                for step in 0..self.tabs.len() {
                    let index = (monitor.cursor + step) % self.tabs.len();
                    let tab = self.tabs[index].id;
                    let Some(state) = self.state(tab) else {
                        continue;
                    };
                    if !state.watch.confirmed
                        || state.input.pending.load(Ordering::Acquire) != 0
                        || self.view_cancel.receiving_for(tab)
                        || state.screen.modified
                        || state.pending_edit.is_some()
                        || state
                            .watch
                            .retry
                            .is_some_and(|retry| Instant::now() < retry)
                        || self.saves.iter().any(|save| save.tab == tab)
                        || self
                            .saving
                            .as_ref()
                            .is_some_and(|save| save.intent.tab == tab)
                    {
                        continue;
                    }
                    let Some(document) = state.screen.document else {
                        continue;
                    };
                    let Some(path) = state.screen.path.clone() else {
                        continue;
                    };
                    let revision = state.info.as_ref().ok_or(Error::Busy)?.revision;
                    let observation = state.watch.observed.clone().ok_or(Error::Busy)?;
                    let task = match self.call_background(Request::PrepareRefresh(document)) {
                        Ok(Response::RefreshTask(task)) => {
                            (*task).expect_observation(observation.clone())
                        }
                        Err(
                            Error::Busy
                            | Error::Cancelled
                            | Error::StaleRevision
                            | Error::SaveConflict,
                        ) => continue,
                        Err(error) => {
                            self.refresh_failed(tab, error);
                            continue;
                        }
                        _ => return Err(Error::CorruptStorage),
                    };
                    let cancel = Cancellation::default();
                    let running = cancel.clone();
                    let progress = Arc::new(Mutex::new(None));
                    let updates = progress.clone();
                    let worker =
                        thread::Builder::new()
                            .name("ede-refresh".into())
                            .spawn(move || {
                                task.run_with_progress(&running, |progress| {
                                    if let Ok(mut slot) = updates.lock() {
                                        *slot = Some(progress);
                                    }
                                })
                                .map(PreparedStage::Root)
                            })?;
                    monitor.preparing = Some(Preparing {
                        tab,
                        document,
                        path,
                        revision,
                        observation,
                        cancel,
                        worker: Some(worker),
                        ready: None,
                        progress,
                    });
                    monitor.cursor = index + 1;
                    break;
                }
            }
            if self
                .tab
                .view_retry
                .is_some_and(|retry| Instant::now() >= retry)
                && self.tab.input.pending.load(Ordering::Acquire) == 0
            {
                self.render_refreshed();
                changed = true;
            }
            Ok(changed)
        })();
        self.monitor = Some(monitor);
        result
    }

    /// The owner returns exact prepared metadata; all coordinator updates are infallible.
    fn commit_refresh(
        &mut self,
        tab: TabId,
        document: DocumentId,
        prepared: document::PreparedRefresh,
        view: RefreshView,
    ) -> Result<(), Error> {
        let input = &self.state(tab).ok_or(Error::Closed)?.input;
        if input.pending.load(Ordering::Acquire) != 0
            || input.value.load(Ordering::Acquire) != view.publication
            || self.view_cancel.receiving_for(tab)
        {
            return Err(Error::StaleRevision);
        }
        let report = match self.call_background(Request::Refresh {
            document,
            prepared: Box::new(prepared),
        })? {
            Response::Refreshed(report) => report,
            _ => return Err(Error::CorruptStorage),
        };
        let state = self.state_mut(tab).ok_or(Error::Closed)?;
        state.apply_info(report.info);
        state.screen.position = view.position;
        state.top = view.top;
        state.left = view.left;
        state.preferred = view.preferred;
        state.watch = Watch::default();
        state.screen.view = None;
        state.screen.message = if report.undo_reset {
            "Refreshed from disk; previous contents exceeded the undo limit"
        } else if report.changed {
            "Refreshed from disk"
        } else {
            "Disk baseline updated"
        }
        .into();
        if report.changed {
            state.redo_views.clear();
            if report.undo_reset {
                state.undo_views.clear();
            } else {
                state.undo_views.push_back((view.before, view.position));
                for _ in 0..report.history_pruned.min(state.undo_views.len() as u64) {
                    state.undo_views.pop_front();
                }
            }
        }
        if self.tab.screen.tab == Some(tab) {
            self.cache = Checkpoints::default();
            self.render_refreshed();
        }
        Ok(())
    }

    /// A view failure after a successful handoff cannot leave stale revisions/history behind.
    fn render_refreshed(&mut self) {
        let foreground = std::mem::take(&mut self.cancel);
        let result = self.layout(false);
        self.cancel = foreground;
        if let Err(error) = result {
            self.tab.screen.view = None;
            self.tab.view_retry = Some(Instant::now() + Duration::from_secs(1));
            self.tab.screen.message = format!("Refreshed; view retry pending: {error}");
        }
    }

    #[cfg(test)]
    fn install_refresh(
        &mut self,
        tab: TabId,
        document: DocumentId,
        mut prepared: document::PreparedRefresh,
    ) -> Result<(), Error> {
        let view = self.refresh_view(tab)?;
        let mapped = map_view(
            &mut prepared,
            view,
            self.state(tab).ok_or(Error::Closed)?.input.value.clone(),
            &Cancellation::default(),
        )?;
        self.commit_refresh(tab, document, prepared, mapped)
    }
}

#[cfg(test)]
mod tests;
