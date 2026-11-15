//! Session-wide search coordination; all scanning and batch preparation is off-owner.
use super::*;
use crate::search::{Progress, Query, ScanOptions, validate_field};
use std::ops::Range;

/// Named search intent, independent of keyboard, mouse, and panel focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    Next,
    Previous,
    ReplaceNext,
    ReplaceAll,
    Decorate,
}

/// Bounded captured fields; all-open membership is captured when admitted.
#[derive(Clone, Debug)]
pub(crate) struct SearchRequest {
    pub navigation: Option<u64>,
    pub query: Query,
    pub replacement: String,
    pub all: bool,
    pub operation: Operation,
}

/// Exact search range is separate from the grapheme-safe ordinary caret/selection.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub tab: TabId,
    pub document: DocumentId,
    pub revision: document::Revision,
    pub range: Range<u64>,
}

/// A bounded, accessible per-target report; partial counts never become final silently.
#[derive(Clone, Debug)]
pub(crate) struct Outcome {
    pub tab: TabId,
    pub count: u64,
    pub revision: Option<document::Revision>,
    pub message: String,
}

/// Small prepared UI data; no snapshot, matcher, or document text crosses this boundary.
#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub busy: bool,
    pub replacing: bool,
    pub message: String,
    pub current: Option<Found>,
    pub outcomes: Vec<Outcome>,
    pub progress: Progress,
    pub decorations: Vec<Range<u64>>,
    pub decorated: Option<(TabId, document::Revision, u64, u64, u64)>,
    pub partial_highlighting: bool,
    pub processed: u64,
    pub changed: usize,
    /// One completed ReplaceNext remains distinct from its follow-up match counts.
    pub replacement_ack: Option<Outcome>,
}
impl Status {
    pub(crate) fn total(&self) -> u64 {
        self.outcomes.iter().map(|o| o.count).sum()
    }
    pub(crate) fn summary(&self, tabs: &[TabSummary]) -> String {
        let mut text = self.message.clone();
        if let Some(ack) = &self.replacement_ack {
            let label = tabs.iter().find(|tab| tab.id == ack.tab).map_or_else(
                || format!("Closed tab {}", ack.tab.0),
                |tab| tab.label.to_string(),
            );
            text.push_str(&format!(
                "\nReplacement: {label}; {} processed; {}",
                ack.count, ack.message
            ));
        }
        for outcome in &self.outcomes {
            let label = tabs.iter().find(|tab| tab.id == outcome.tab).map_or_else(
                || format!("Closed tab {}", outcome.tab.0),
                |tab| tab.label.to_string(),
            );
            text.push_str(&format!(
                "\n{label}: {} matches; {}",
                outcome.count, outcome.message
            ));
        }
        text
    }
}

/// Live work remains singly owned, with one coalesced future search intention.
#[derive(Default)]
pub(super) struct State {
    metadata: Option<document::Permit>,
    pub(super) status: Status,
    query: Option<Query>,
    run: Option<Run>,
    pending: Option<Run>,
    deferred: Option<(Action, u64)>,
    decorations_enabled: bool,
    targets: Vec<TabId>,
    active_target: Option<TabId>,
    operation: Option<Operation>,
    maintenance: Vec<(TabId, Option<u64>)>,
    maintaining: Option<Maintenance>,
}
/// One private reclamation shares the search worker budget and yields to user work.
struct Maintenance {
    tab: TabId,
    cancel: Cancellation,
    handle: JoinHandle<Result<document::PreparedEdit, Error>>,
    _permit: document::Permit,
}
struct Run {
    _permit: Option<document::Permit>,
    request: SearchRequest,
    targets: Vec<TabId>,
    anchor: usize,
    bound: u64,
    next: usize,
    cancel: Cancellation,
    navigation: u64,
    worker: Option<Worker>,
    best: Option<(usize, bool, Found)>,
    wrap: bool,
    exact: Option<Found>,
    progress: Arc<Mutex<Progress>>,
    previous_message: String,
}
struct Worker {
    tab: TabId,
    handle: JoinHandle<Result<Finished, Error>>,
}
enum Finished {
    Scanned(ScanResult),
    Prepared(Box<document::PreparedEdit>, u64),
    Mapped(Box<document::PreparedEdit>, u64, Mapping),
}
struct ScanResult {
    document: DocumentId,
    revision: document::Revision,
    count: u64,
    first: Option<Range<u64>>,
    last: Option<Range<u64>>,
    eligible: Option<Range<u64>>,
    decorations: Vec<Range<u64>>,
    partial: bool,
    layout: u64,
    top: u64,
    left: u64,
}
#[derive(Clone, Copy, PartialEq)]
struct ViewState {
    position: Position,
    top: u64,
    left: u64,
    preferred: Option<u64>,
    generation: u64,
    width: u16,
    height: u16,
}
struct Mapping {
    before: ViewState,
    position: Position,
    top: u64,
    left: u64,
}

/// Navigation ownership is revoked at admission, before a delayed worker can steal focus.
pub(super) fn revokes_navigation(action: &Action) -> bool {
    !matches!(
        action,
        Action::Size(..)
            | Action::FieldPaste(..)
            | Action::Save(_)
            | Action::SaveGuard(..)
            | Action::RetryRefresh(_)
            | Action::Command(Command::Copy)
            | Action::RecoveryPage { .. }
    )
}
pub(super) fn clears_anchor(action: &Action) -> bool {
    matches!(
        action,
        Action::Insert(_)
            | Action::Paste(_)
            | Action::Hit { .. }
            | Action::GoTo { .. }
            | Action::ClearSelection
            | Action::Switch(_)
            | Action::CycleTab(_)
            | Action::Command(
                Command::Move(..)
                    | Command::SelectAll
                    | Command::Undo
                    | Command::Redo
                    | Command::Cut
                    | Command::Paste
                    | Command::Backspace
                    | Command::Delete
                    | Command::Tab
                    | Command::Newline
            )
    )
}
/// Stream history permits navigation/copy/search, but no user mutation path.
pub(super) fn mutates(action: &Action) -> bool {
    matches!(
        action,
        Action::Insert(_)
            | Action::Paste(_)
            | Action::Command(
                Command::Cut
                    | Command::Paste
                    | Command::Undo
                    | Command::Redo
                    | Command::Backspace
                    | Command::Delete
                    | Command::Tab
                    | Command::Newline
            )
    )
}

impl State {
    pub(super) fn busy(&self) -> bool {
        self.run.is_some() || self.pending.is_some() || self.maintaining.is_some()
    }
    pub(super) fn cancel(&mut self, invalidate: bool) {
        self.decorations_enabled = false;
        self.pending = None;
        if let Some(work) = &self.maintaining {
            work.cancel.cancel();
        }
        if let Some(run) = &self.run {
            run.cancel.cancel();
            self.status.message = "Cancelling search".into();
        }
        if invalidate {
            self.status.current = None;
            self.status.decorations.clear();
            self.status.decorated = None;
            self.query = None;
        }
    }
    /// Lifecycle decisions wait for every possible publication to be reconciled.
    pub(super) fn defer_lifecycle(&mut self, action: &Action, sequence: u64) -> bool {
        if self.busy()
            && self.status.replacing
            && matches!(
                action,
                Action::Guard(_) | Action::RemoveTab(_) | Action::FinishQuit
            )
        {
            self.cancel(false);
            self.deferred = Some((action.clone(), sequence));
            return true;
        }
        if matches!(action, Action::CancelGuard) {
            self.deferred = None;
        }
        false
    }
    pub(super) fn shutdown(&mut self) {
        self.cancel(true);
        if let Some(work) = self.maintaining.take() {
            let _ = work.handle.join();
        }
        if let Some(run) = self.run.take()
            && let Some(worker) = run.worker
        {
            let _ = worker.handle.join();
        }
    }
    fn start(
        &mut self,
        owner: &mut Coordinator,
        request: SearchRequest,
        wrap: bool,
    ) -> Result<(), Error> {
        validate_field(&request.query.literal)?;
        validate_field(&request.replacement)?;
        if self.metadata.is_none() {
            self.metadata = Some(owner.service.reserve_interaction(256 << 10)?);
        }
        if self.run.is_some() && self.status.replacing {
            return Err(Error::Busy);
        }
        let current = self
            .status
            .current
            .as_ref()
            .filter(|found| {
                self.query.as_ref() == Some(&request.query)
                    && owner
                        .state(found.tab)
                        .and_then(|s| s.info.as_ref())
                        .is_some_and(|i| i.revision == found.revision)
            })
            .cloned();
        let exact = (request.operation == Operation::ReplaceNext)
            .then_some(current.clone())
            .flatten();
        let mut request = request;
        if request.operation == Operation::ReplaceNext && exact.is_none() {
            request.operation = Operation::Next;
        }
        let active = exact.as_ref().map(|f| f.tab).or(owner.tab.screen.tab);
        let targets: Vec<_> = owner
            .tabs
            .iter()
            .filter(|t| request.all || Some(t.id) == active)
            .map(|t| t.id)
            .collect();
        let anchor = targets
            .iter()
            .position(|id| Some(*id) == active)
            .unwrap_or(0);
        let backwards = request.operation == Operation::Previous;
        let bound = current
            .as_ref()
            .filter(|found| Some(found.tab) == active)
            .map_or(owner.tab.screen.position.caret, |found| {
                if backwards {
                    found.range.start
                } else {
                    found.range.end
                }
            });
        let cancel = Cancellation::default();
        let navigation = request
            .navigation
            .unwrap_or_else(|| owner.view_cancel.search_navigation.load(Ordering::Acquire));
        let run = Run {
            _permit: None,
            request,
            targets,
            anchor,
            bound,
            next: 0,
            cancel,
            navigation,
            worker: None,
            best: None,
            wrap,
            exact,
            progress: Arc::new(Mutex::new(Progress::default())),
            previous_message: self.status.message.clone(),
        };
        if self.run.is_some() || self.maintaining.is_some() {
            self.cancel(false);
            self.pending = Some(run);
            Ok(())
        } else {
            self.admit(owner, run)
        }
    }
    /// Acquire resources only after prior work releases them. Scope and anchor
    /// were captured at dispatch and cannot drift while cancellation drains.
    fn admit(&mut self, owner: &mut Coordinator, mut run: Run) -> Result<(), Error> {
        let permit = if run.request.query.literal.is_empty() {
            None
        } else {
            Some(
                owner
                    .service
                    .reserve_interaction(crate::search::WORK_BYTES - (256 << 10))?,
            )
        };
        self.targets = run.targets.clone();
        self.operation = Some(run.request.operation);
        let replacing = matches!(
            run.request.operation,
            Operation::ReplaceNext | Operation::ReplaceAll
        );
        if run.request.operation == Operation::Decorate {
            self.status.busy = false;
            self.status.replacing = false;
        } else {
            self.status = Status {
                busy: !run.request.query.literal.is_empty(),
                replacing,
                message: "Searching".into(),
                ..Status::default()
            };
        }
        self.query = Some(run.request.query.clone());
        if run.request.query.literal.is_empty() {
            self.status.message = "Enter search text".into();
            return Ok(());
        }
        run._permit = permit;
        self.decorations_enabled = true;
        owner
            .view_cancel
            .search_replacing
            .store(replacing, Ordering::Release);
        *owner
            .view_cancel
            .search
            .lock()
            .map_err(|_| Error::CorruptStorage)? = run.cancel.clone();
        self.run = Some(run);
        Ok(())
    }
    fn finish(
        &mut self,
        owner: &mut Coordinator,
        mut run: Run,
        reason: Option<String>,
    ) -> Result<(), Error> {
        self.status.busy = false;
        self.status.replacing = false;
        owner
            .view_cancel
            .search_replacing
            .store(false, Ordering::Release);
        let successful = reason.is_none() && !run.cancel.is_cancelled();
        if !successful {
            self.decorations_enabled = false;
        }
        let decorating = run.request.operation == Operation::Decorate;
        for tab in &run.targets {
            if !decorating
                && !self
                    .status
                    .outcomes
                    .iter()
                    .any(|outcome| outcome.tab == *tab)
            {
                self.status.outcomes.push(Outcome {
                    tab: *tab,
                    count: 0,
                    revision: None,
                    message: "Unprocessed".into(),
                });
            }
        }
        let replaced = matches!(
            run.request.operation,
            Operation::ReplaceAll | Operation::ReplaceNext
        );
        self.status.message = if decorating && successful {
            run.previous_message.clone()
        } else if run.cancel.is_cancelled() {
            self.status.current = None;
            self.status.decorations.clear();
            self.status.decorated = None;
            format!(
                "Cancelled; {} processed; {} changed documents",
                self.status.processed, self.status.changed
            )
        } else if let Some(reason) = reason {
            format!(
                "Stopped: {reason}; {} processed, {} changed documents",
                self.status.processed, self.status.changed
            )
        } else if replaced {
            format!(
                "{} occurrences processed; {} changed documents",
                self.status.processed, self.status.changed
            )
        } else if let Some((_, wrapped, found)) = run.best.take() {
            if run.navigation == owner.view_cancel.search_navigation.load(Ordering::Acquire)
                && owner
                    .state(found.tab)
                    .and_then(|s| s.info.as_ref())
                    .is_some_and(|info| info.revision == found.revision)
            {
                owner.activate(found.tab);
                let snapshot = owner.snapshot()?;
                let mut text = Text::new(&snapshot, &run.cancel);
                let ceil = text.ceil(found.range.start)?;
                let caret = if ceil == found.range.start {
                    ceil
                } else {
                    text.previous(ceil)?
                };
                owner.tab.screen.position.move_to(caret, false);
                owner.tab.preferred = None;
                owner.layout(true)?;
                self.status.current = Some(found);
            }
            format!(
                "{} matches{}; exact match differs from ordinary selection",
                self.status.total(),
                if wrapped { "; Wrapped" } else { "" }
            )
        } else if run.wrap {
            "No matches".into()
        } else {
            "No further matches".into()
        };
        if run.cancel.is_cancelled() {
            self.status.message = format!(
                "Cancelled; {} processed; {} changed documents",
                self.status.processed, self.status.changed
            );
        }
        if !replaced
            && !decorating
            && !run.cancel.is_cancelled()
            && self
                .status
                .outcomes
                .iter()
                .any(|outcome| outcome.message != "Complete")
        {
            self.status.message = if self.status.current.is_none() {
                "Search incomplete/stale; inspect Search summary".into()
            } else {
                format!("{}; Incomplete/stale", self.status.message)
            };
        }
        if !replaced && !decorating && successful && self.status.processed != 0 {
            self.status.message = format!(
                "{} processed; {} changed documents; {}",
                self.status.processed, self.status.changed, self.status.message
            );
        }
        let auto = replaced
            && successful
            && run.request.operation == Operation::ReplaceNext
            && !run.cancel.is_cancelled()
            && run.navigation == owner.view_cancel.search_navigation.load(Ordering::Acquire);
        // The completed worker is joined and publication reconciled. Its full
        // reservation must be released before admitting a continuation/retry.
        drop(run._permit);
        if let Some((action, sequence)) = self.deferred.take() {
            let prior = owner.active_sequence;
            owner.active_sequence = sequence;
            let result = owner.action(action);
            owner.active_sequence = prior;
            result?;
        } else if auto {
            let processed = self.status.processed;
            let changed = self.status.changed;
            let acknowledged = run.exact.as_ref().and_then(|exact| {
                self.status
                    .outcomes
                    .iter()
                    .find(|outcome| outcome.tab == exact.tab)
                    .cloned()
            });
            run.request.operation = Operation::Next;
            let mut request = run.request;
            // The continuation is installed as an exact anchor, even for a no-op.
            self.status.current = run.exact;
            request.operation = Operation::Next;
            self.start(owner, request, false)?;
            self.status.processed = processed;
            self.status.changed = changed;
            self.status.replacement_ack = acknowledged;
        } else if let Some(run) = self.pending.take() {
            self.admit(owner, run)?;
        }
        self.start_maintenance(owner);
        Ok(())
    }

    /// Remember at most one reclamation request per open tab. Failed attempts
    /// wait for a new acknowledged action instead of spinning on an I/O error.
    pub(super) fn queue_maintenance(&mut self, tab: TabId) {
        if !self.maintenance.iter().any(|entry| entry.0 == tab) {
            self.maintenance.push((tab, None));
        }
    }

    fn start_maintenance(&mut self, owner: &mut Coordinator) {
        if self.busy() || owner.saving.is_some() || !owner.saves.is_empty() {
            return;
        }
        self.maintenance.retain(|(tab, _)| {
            owner
                .state(*tab)
                .and_then(|s| s.info.as_ref())
                .is_some_and(|i| i.compaction_pending)
        });
        let Some(entry) = self.maintenance.iter_mut().find(|(tab, attempted)| {
            *attempted != Some(owner.acknowledged)
                && owner
                    .state(*tab)
                    .is_some_and(|s| s.pending_edit.is_none() && !s.screen.loading)
        }) else {
            return;
        };
        let tab = entry.0;
        entry.1 = Some(owner.acknowledged);
        let result = (|| {
            let permit = owner
                .service
                .reserve_interaction(crate::search::WORK_BYTES - (256 << 10))?;
            let document = owner
                .state(tab)
                .and_then(|s| s.screen.document)
                .ok_or(Error::Closed)?;
            let task = match owner.call_background(Request::PrepareEdit(document))? {
                Response::EditTask(task) => task,
                _ => return Err(Error::CorruptStorage),
            };
            let cancel = Cancellation::default();
            *owner
                .view_cancel
                .search
                .lock()
                .map_err(|_| Error::CorruptStorage)? = cancel.clone();
            let token = cancel.clone();
            let handle = thread::Builder::new()
                .name("ede-search-maintain".into())
                .spawn(move || task.compact(&token))?;
            Ok(Maintenance {
                tab,
                cancel,
                handle,
                _permit: permit,
            })
        })();
        match result {
            Ok(work) => self.maintaining = Some(work),
            Err(error) => owner.event(Event::Failure(format!("Storage cleanup: {error}"))),
        }
    }

    /// Publication changes storage only. Text revision, selection and undo views
    /// remain valid, and save-baseline/lease guards are checked on the owner.
    fn poll_maintenance(&mut self, owner: &mut Coordinator) -> Result<bool, Error> {
        let Some(work) = self.maintaining.take() else {
            return Ok(false);
        };
        if !work.handle.is_finished() {
            self.maintaining = Some(work);
            return Ok(true);
        }
        let result = work
            .handle
            .join()
            .map_err(|_| Error::CorruptStorage)
            .and_then(|r| r)
            .and_then(|prepared| {
                work.cancel.check()?;
                let document = prepared.before().document();
                match owner.call_background(Request::FinishEdit {
                    document,
                    prepared: Box::new(prepared),
                })? {
                    Response::Edited(report) => Ok(report),
                    _ => Err(Error::CorruptStorage),
                }
            });
        match result {
            Ok(report) => {
                if let Some(target) = owner.state_mut(work.tab) {
                    target.apply_info(report.info);
                }
                self.maintenance.retain(|entry| entry.0 != work.tab);
            }
            Err(Error::Cancelled | Error::Busy | Error::StaleRevision | Error::Closed) => {}
            Err(error) => owner.event(Event::Failure(format!("Storage cleanup: {error}"))),
        }
        drop(work._permit);
        if let Some(run) = self.pending.take() {
            self.admit(owner, run)?;
        }
        Ok(true)
    }
}

impl Coordinator {
    pub(super) fn start_search(&mut self, request: SearchRequest) -> Result<(), Error> {
        let mut state = std::mem::take(&mut self.search);
        let result = state.start(self, request, true);
        self.search = state;
        result
    }
    fn search_view(&self, tab: TabId) -> Result<ViewState, Error> {
        let state = self.state(tab).ok_or(Error::Closed)?;
        Ok(ViewState {
            position: state.screen.position,
            top: state.top,
            left: state.left,
            preferred: state.preferred,
            generation: self.generation,
            width: self.width,
            height: self.height,
        })
    }
    pub(super) fn poll_search(&mut self) -> Result<bool, Error> {
        let mut state = std::mem::take(&mut self.search);
        let result = state.poll(self);
        if let Err(error) = &result {
            state.cancel(false);
            state.status.busy = false;
            state.status.replacing = false;
            state.status.message = format!(
                "Stopped: {error}; {} processed, {} changed documents",
                state.status.processed, state.status.changed
            );
            self.view_cancel
                .search_replacing
                .store(false, Ordering::Release);
            if state.operation != Some(Operation::Decorate) {
                for tab in &state.targets {
                    if !state
                        .status
                        .outcomes
                        .iter()
                        .any(|outcome| outcome.tab == *tab)
                    {
                        state.status.outcomes.push(Outcome {
                            tab: *tab,
                            count: 0,
                            revision: None,
                            message: if state.active_target == Some(*tab) {
                                error.to_string()
                            } else {
                                "Unprocessed".into()
                            },
                        });
                    }
                }
            }
            if let Some((action, sequence)) = state.deferred.take() {
                let previous = self.active_sequence;
                self.active_sequence = sequence;
                let deferred = self.action(action);
                self.active_sequence = previous;
                if let Err(error) = deferred {
                    self.event(Event::Failure(error.to_string()));
                }
            }
        }
        self.search = state;
        result
    }
}

impl State {
    fn poll(&mut self, owner: &mut Coordinator) -> Result<bool, Error> {
        if self.maintaining.is_some() {
            return self.poll_maintenance(owner);
        }
        // Refresh/undo/edit invalidate counts without ever applying old ranges.
        let mut invalidated = false;
        for outcome in &mut self.status.outcomes {
            if outcome.revision.is_some_and(|revision| {
                owner
                    .state(outcome.tab)
                    .and_then(|s| s.info.as_ref())
                    .is_none_or(|i| i.revision != revision)
            }) {
                invalidated |= outcome.message != "Stale; search again";
                outcome.message = "Stale; search again".into();
                outcome.count = 0;
            }
        }
        if invalidated {
            self.status.message = "Results stale/incomplete; search again".into();
        }
        if self.status.current.as_ref().is_some_and(|found| {
            owner
                .state(found.tab)
                .and_then(|s| s.info.as_ref())
                .is_none_or(|i| i.revision != found.revision)
        }) {
            self.status.current = None;
        }
        let Some(mut run) = self.run.take() else {
            self.start_maintenance(owner);
            if self.maintaining.is_some() {
                return Ok(true);
            }
            if self.decorations_enabled
                && let (Some(query), Some(tab), Some(info), Some(view)) = (
                    self.query.clone(),
                    owner.tab.screen.tab,
                    owner.tab.info.as_ref(),
                    owner.tab.screen.view.as_ref(),
                )
            {
                let key = (tab, info.revision, owner.generation, view.top, view.left);
                if self.status.decorated != Some(key) {
                    self.start(
                        owner,
                        SearchRequest {
                            navigation: None,
                            query,
                            replacement: String::new(),
                            all: false,
                            operation: Operation::Decorate,
                        },
                        false,
                    )?;
                    return Ok(true);
                }
            }
            return Ok(invalidated);
        };
        self.status.progress = *run.progress.lock().map_err(|_| Error::CorruptStorage)?;
        if let Some(worker) = run.worker.take() {
            if !worker.handle.is_finished() {
                run.worker = Some(worker);
                self.run = Some(run);
                return Ok(true);
            }
            let tab = worker.tab;
            let finished = worker
                .handle
                .join()
                .map_err(|_| Error::CorruptStorage)
                .and_then(|r| r);
            match finished {
                Err(error) => {
                    if run.request.operation != Operation::Decorate {
                        self.status.outcomes.push(Outcome {
                            tab,
                            count: 0,
                            revision: None,
                            message: error.to_string(),
                        });
                    }
                    self.finish(owner, run, Some(error.to_string()))?;
                    return Ok(true);
                }
                Ok(Finished::Scanned(scan)) => {
                    if run.cancel.is_cancelled() {
                        self.finish(owner, run, None)?;
                        return Ok(true);
                    }
                    let valid = owner
                        .state(tab)
                        .and_then(|s| s.info.as_ref())
                        .is_some_and(|i| i.revision == scan.revision);
                    if run.request.operation == Operation::Decorate {
                        if valid && owner.tab.screen.tab == Some(tab) {
                            self.status.decorated =
                                Some((tab, scan.revision, scan.layout, scan.top, scan.left));
                            self.status.decorations = scan.decorations;
                            self.status.partial_highlighting = scan.partial;
                        }
                        self.finish(owner, run, None)?;
                        return Ok(true);
                    }
                    self.status.outcomes.push(Outcome {
                        tab,
                        count: if valid { scan.count } else { 0 },
                        revision: Some(scan.revision),
                        message: if valid {
                            "Complete"
                        } else {
                            "Stale; search again"
                        }
                        .into(),
                    });
                    if valid {
                        let index = run.next - 1;
                        let previous = run.request.operation == Operation::Previous;
                        let n = run.targets.len();
                        let (rank, wrapped, range) = if index == run.anchor {
                            if scan.eligible.is_some() {
                                (0, false, scan.eligible)
                            } else {
                                (n, true, if previous { scan.last } else { scan.first })
                            }
                        } else if previous {
                            ((run.anchor + n - index) % n, index > run.anchor, scan.last)
                        } else {
                            ((index + n - run.anchor) % n, index < run.anchor, scan.first)
                        };
                        if let Some(range) = range
                            && (run.wrap || !wrapped)
                            && run.best.as_ref().is_none_or(|(best, _, _)| rank < *best)
                        {
                            run.best = Some((
                                rank,
                                wrapped,
                                Found {
                                    tab,
                                    document: scan.document,
                                    revision: scan.revision,
                                    range,
                                },
                            ));
                        }
                        if owner.tab.screen.tab == Some(tab) {
                            self.status.decorated =
                                Some((tab, scan.revision, scan.layout, scan.top, scan.left));
                            self.status.decorations = scan.decorations;
                            self.status.partial_highlighting = scan.partial;
                        }
                    }
                }
                Ok(Finished::Prepared(prepared, count)) => {
                    if run.cancel.is_cancelled() {
                        self.finish(owner, run, Some("Cancelled before publication".into()))?;
                        return Ok(true);
                    }
                    let before = owner.search_view(tab)?;
                    let cancel = run.cancel.clone();
                    let handle =
                        thread::Builder::new()
                            .name("ede-search-map".into())
                            .spawn(move || {
                                let mut offsets = [
                                    before.position.caret,
                                    before.position.anchor,
                                    prepared
                                        .before()
                                        .locate_line(
                                            before.top.min(prepared.before().line_count() - 1),
                                            &cancel,
                                        )?
                                        .0,
                                ];
                                prepared.map_offsets(&mut offsets, &cancel)?;
                                let snapshot = prepared.snapshot()?;
                                let mut text = Text::new(&snapshot, &cancel);
                                let position = Position {
                                    caret: text.ceil(offsets[0].min(snapshot.len()))?,
                                    anchor: text.ceil(offsets[1].min(snapshot.len()))?,
                                };
                                let top = snapshot
                                    .line_of_offset(ByteOffset(offsets[2].min(snapshot.len())))?;
                                let left = text.clamp_left(
                                    top,
                                    before.height,
                                    before.left,
                                    &mut Checkpoints::default(),
                                )?;
                                Ok(Finished::Mapped(
                                    prepared,
                                    count,
                                    Mapping {
                                        before,
                                        position,
                                        top,
                                        left,
                                    },
                                ))
                            })?;
                    run.worker = Some(Worker { tab, handle });
                    self.run = Some(run);
                    return Ok(true);
                }
                Ok(Finished::Mapped(prepared, count, mapping)) => {
                    if owner.state(tab).is_none_or(|target| {
                        target.capability == crate::search::Capability::ReadOnly
                    }) {
                        self.status.outcomes.push(Outcome {
                            tab,
                            count: 0,
                            revision: None,
                            message: "Skipped: read-only or closed".into(),
                        });
                        self.finish(owner, run, Some("Target became unavailable".into()))?;
                        return Ok(true);
                    }
                    if owner.search_view(tab)? != mapping.before {
                        let handle = thread::spawn(move || Ok(Finished::Prepared(prepared, count)));
                        run.worker = Some(Worker { tab, handle });
                        self.run = Some(run);
                        return Ok(true);
                    }
                    let document = prepared.before().document();
                    let expected = prepared.before().revision();
                    let result = owner.call_background(Request::FinishEdit { document, prepared });
                    let report = match result {
                        Ok(Response::Edited(report)) => report,
                        Ok(_) => return Err(Error::CorruptStorage),
                        Err(error) => {
                            self.status.outcomes.push(Outcome {
                                tab,
                                count: 0,
                                revision: Some(expected),
                                message: error.to_string(),
                            });
                            self.finish(owner, run, Some(error.to_string()))?;
                            return Ok(true);
                        }
                    };
                    if let Some(target) = owner.state_mut(tab) {
                        let before = target.screen.position;
                        target.apply_info(report.info.clone());
                        if report.changed {
                            target.screen.position = mapping.position;
                            target.top = mapping.top;
                            target.left = mapping.left;
                            target.preferred = mapping.before.preferred;
                            target.redo_views.clear();
                            target.undo_views.push_back((before, mapping.position));
                            for _ in 0..report.history_pruned.min(target.undo_views.len() as u64) {
                                target.undo_views.pop_front();
                            }
                            target.screen.view = None;
                        }
                    }
                    if report.info.compaction_pending {
                        self.queue_maintenance(tab);
                    }
                    self.status.processed = self
                        .status
                        .processed
                        .checked_add(count)
                        .ok_or(Error::ResourceLimit("replacement count overflow"))?;
                    self.status.changed += usize::from(report.changed);
                    self.status.outcomes.push(Outcome {
                        tab,
                        count,
                        revision: Some(report.info.revision),
                        message: if report.changed {
                            format!("Replaced; {} history entries pruned", report.history_pruned)
                        } else {
                            "Completed without text changes".into()
                        },
                    });
                    if let Some(exact) = &mut run.exact {
                        exact.revision = report.info.revision;
                        exact.range.end = exact.range.start + run.request.replacement.len() as u64;
                    }
                    if owner.tab.screen.tab == Some(tab) {
                        owner.cache = Checkpoints::default();
                        owner.layout(false)?;
                    }
                }
            }
        }
        if run.cancel.is_cancelled() || run.next == run.targets.len() {
            self.finish(owner, run, None)?;
            return Ok(true);
        }
        let tab = run.targets[run.next];
        self.active_target = Some(tab);
        let label = owner
            .summaries
            .iter()
            .find(|summary| summary.id == tab)
            .map_or_else(
                || format!("Tab {}", tab.0),
                |summary| summary.label.to_string(),
            );
        if run.request.operation != Operation::Decorate {
            self.status.message = format!(
                "{} {label}",
                if self.status.replacing {
                    "Replacing"
                } else {
                    "Searching"
                }
            );
        }
        run.next += 1;
        let eligible = owner
            .state(tab)
            .filter(|s| s.info.is_some() && !s.screen.loading && s.pending_edit.is_none());
        let Some(target) = eligible else {
            self.status.outcomes.push(Outcome {
                tab,
                count: 0,
                revision: None,
                message: "Skipped: loading, closed or unavailable".into(),
            });
            self.run = Some(run);
            return Ok(true);
        };
        let document = target.screen.document.ok_or(Error::Busy)?;
        if matches!(
            run.request.operation,
            Operation::ReplaceNext | Operation::ReplaceAll
        ) && target.capability == crate::search::Capability::ReadOnly
        {
            self.status.outcomes.push(Outcome {
                tab,
                count: 0,
                revision: target.info.as_ref().map(|i| i.revision),
                message: "Skipped: read-only retained history".into(),
            });
            self.run = Some(run);
            return Ok(true);
        }
        let revision = target.info.as_ref().ok_or(Error::Busy)?.revision;
        let visible = target
            .screen
            .view
            .as_ref()
            .map(|view| {
                let start = view
                    .rows
                    .first()
                    .and_then(|r| r.spans.first())
                    .map_or(0, |s| s.bytes.start);
                start..view.rows.last().map_or(start, |r| r.end)
            })
            .unwrap_or(0..0);
        let layout = owner.generation;
        let (top, left) = (target.top, target.left);
        let request = run.request.clone();
        let bound = run.bound;
        let cancel = run.cancel.clone();
        let progress = run.progress.clone();
        let replacing = matches!(
            request.operation,
            Operation::ReplaceAll | Operation::ReplaceNext
        );
        if request.operation == Operation::ReplaceNext
            && run.exact.as_ref().is_none_or(|exact| exact.tab != tab)
        {
            self.run = Some(run);
            return Ok(true);
        }
        if replacing {
            let task = match owner.call_background(Request::PrepareEdit(document))? {
                Response::EditTask(task) => task,
                _ => return Err(Error::CorruptStorage),
            };
            let mut transaction = match owner.call_background(Request::BeginTransaction {
                document,
                expected: revision,
            })? {
                Response::Transaction(t) => t,
                _ => return Err(Error::CorruptStorage),
            };
            let exact = run.exact.clone();
            let handle = thread::Builder::new()
                .name("ede-search-plan".into())
                .spawn(move || {
                    let count = if let Some(exact) = exact {
                        if exact.document != document || exact.revision != revision {
                            return Err(Error::StaleRevision);
                        }
                        transaction.replace(
                            TextRange::new(exact.range.start, exact.range.end)?,
                            request.replacement.as_bytes(),
                            &cancel,
                        )?;
                        1
                    } else {
                        request
                            .query
                            .compile()?
                            .scan_admitted(
                                task.snapshot(),
                                &cancel,
                                ScanOptions::default(),
                                |range| {
                                    transaction.replace(
                                        range,
                                        request.replacement.as_bytes(),
                                        &cancel,
                                    )?;
                                    Ok(true)
                                },
                                |p| {
                                    if let Ok(mut slot) = progress.lock() {
                                        *slot = p;
                                    }
                                },
                            )?
                            .matches
                    };
                    Ok(Finished::Prepared(
                        Box::new(task.run(transaction, &cancel)?),
                        count,
                    ))
                })?;
            run.worker = Some(Worker { tab, handle });
        } else {
            let snapshot = match owner.call_background(Request::BackgroundSnapshot(document))? {
                Response::Snapshot(s) => s,
                _ => return Err(Error::CorruptStorage),
            };
            let handle = thread::Builder::new()
                .name("ede-search-scan".into())
                .spawn(move || {
                    let mut result = ScanResult {
                        document,
                        revision,
                        count: 0,
                        first: None,
                        last: None,
                        eligible: None,
                        decorations: Vec::with_capacity(256),
                        partial: false,
                        layout,
                        top,
                        left,
                    };
                    request
                        .query
                        .compile()?
                        .scan_admitted(
                            &snapshot,
                            &cancel,
                            ScanOptions {
                                stop_after: (request.operation == Operation::Decorate)
                                    .then_some(visible.end),
                                ..ScanOptions::default()
                            },
                            |range| {
                                let range = range.start().0..range.end().0;
                                if result.first.is_none() {
                                    result.first = Some(range.clone());
                                }
                                result.last = Some(range.clone());
                                if request.operation == Operation::Previous {
                                    if range.end <= bound {
                                        result.eligible = Some(range.clone());
                                    }
                                } else if range.start >= bound && result.eligible.is_none() {
                                    result.eligible = Some(range.clone());
                                }
                                if range.start < visible.end && range.end > visible.start {
                                    if result.decorations.len() < 256 {
                                        result.decorations.push(range);
                                    } else {
                                        result.partial = true;
                                    }
                                }
                                Ok(true)
                            },
                            |p| {
                                if let Ok(mut slot) = progress.lock() {
                                    *slot = p;
                                }
                            },
                        )
                        .map(|p| result.count = p.matches)?;
                    Ok(Finished::Scanned(result))
                })?;
            run.worker = Some(Worker { tab, handle });
        }
        self.run = Some(run);
        Ok(true)
    }
}
