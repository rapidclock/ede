//! Session-wide search coordination; all scanning and batch preparation is off-owner.
use super::*;
use crate::search::{Progress, Query, ScanOptions, validate_field};
use std::ops::Range;
pub(crate) mod results;
use results::{Index, PageRequest, Results, Rows, Writer};

/// Named search intent, independent of keyboard, mouse, and panel focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    First,
    Next,
    Previous,
    ReplaceNext,
    ReplaceAll,
    Decorate,
    List,
}

/// Bounded captured fields; all-open membership is captured when admitted.
#[derive(Clone, Debug)]
pub(crate) struct SearchRequest {
    pub replacement_scope: Option<ReplacementScope>,
    pub navigation: Option<u64>,
    pub query: Query,
    pub replacement: String,
    pub all: bool,
    pub operation: Operation,
    pub browse: bool,
    pub expected: Option<(u64, u64)>,
}

/// Replacement authority at queue admission, including streams that must be skipped.
#[derive(Clone, Debug)]
pub(crate) struct ReplacementScope {
    pub current: Option<TabId>,
    pub tabs: Vec<(TabId, bool)>,
}

/// Exact search range is separate from the grapheme-safe ordinary caret/selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Found {
    pub tab: TabId,
    pub document: DocumentId,
    pub revision: document::Revision,
    pub range: Range<u64>,
    extent: u64,
}

/// A bounded, accessible per-target report; partial counts never become final silently.
#[derive(Clone, Debug)]
pub(crate) struct Outcome {
    pub tab: TabId,
    pub count: u64,
    pub revision: Option<document::Revision>,
    pub message: String,
}

/// Independent logical lines for the Search Details dialog.
#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub headline: String,
    pub tabs: Vec<String>,
    pub replacements: Vec<String>,
    pub notices: Vec<String>,
}

/// Small prepared UI data; no snapshot, matcher, or document text crosses this boundary.
#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub busy: bool,
    pub replacing: bool,
    pub message: String,
    pub current: Option<Found>,
    /// Completed explicit Find invocation, correlated with UI admission.
    pub find_submission: Option<u64>,
    pub outcomes: Vec<Outcome>,
    pub progress: Progress,
    pub decorations: Vec<Range<u64>>,
    pub decorated: Option<(TabId, document::Revision, u64, u64, u64)>,
    pub partial_highlighting: bool,
    pub processed: u64,
    pub changed: usize,
    /// One completed ReplaceNext remains distinct from its follow-up match counts.
    pub replacement_ack: Option<Outcome>,
    replacement_outcomes: Vec<Outcome>,
    pub rows: Rows,
}
impl Status {
    pub(crate) fn total(&self) -> u64 {
        self.outcomes.iter().map(|o| o.count).sum()
    }
    pub(crate) fn summary(&self, tabs: &[TabSummary]) -> Summary {
        let label = |id: TabId| {
            tabs.iter().find(|tab| tab.id == id).map_or_else(
                || format!("Closed tab {}", id.0),
                |tab| tab.label.to_string(),
            )
        };
        let mut notices = Vec::new();
        if self.rows.feedback_incomplete {
            notices.push("Replacement feedback incomplete".into());
        }
        if let Some(error) = &self.rows.error {
            notices.push(error.clone());
        }
        Summary {
            headline: self.message.clone(),
            tabs: self
                .outcomes
                .iter()
                .map(|outcome| {
                    let matches = if outcome.count == 1 {
                        "match"
                    } else {
                        "matches"
                    };
                    format!(
                        "{} · {} {matches} · {}",
                        label(outcome.tab),
                        outcome.count,
                        outcome.message
                    )
                })
                .collect(),
            replacements: self
                .replacement_ack
                .iter()
                .chain(&self.replacement_outcomes)
                .map(|outcome| {
                    format!(
                        "{} · {} processed · {}",
                        label(outcome.tab),
                        outcome.count,
                        outcome.message
                    )
                })
                .collect(),
            notices,
        }
    }
}

/// Live work remains singly owned, with one coalesced future search intention.
#[derive(Default)]
pub(super) struct State {
    stream_anchor: Option<Found>,
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
    results: Results,
    result_scope: Option<(bool, Option<TabId>)>,
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
    submission: Option<u64>,
    worker: Option<Worker>,
    candidates: Vec<(usize, bool, Found)>,
    held_stream: Option<TabId>,
    wrap: bool,
    exact: Option<Found>,
    stream_resume: Option<Found>,
    read_only: Vec<TabId>,
    progress: Arc<Mutex<Progress>>,
    receipts_started: bool,
}
struct Worker {
    tab: TabId,
    handle: JoinHandle<Result<Finished, Error>>,
}
enum Finished {
    Scanned(ScanResult),
    Prepared(Box<document::PreparedEdit>, u64, Receipt),
    Mapped(Box<document::PreparedEdit>, u64, Mapping, Receipt),
}
type Receipt = (Option<Index>, bool);
struct ScanResult {
    document: DocumentId,
    revision: document::Revision,
    extent: u64,
    count: u64,
    first: Option<Range<u64>>,
    last: Option<Range<u64>>,
    penultimate: Option<Range<u64>>,
    eligible: Option<Range<u64>>,
    preceding_eligible: Option<Range<u64>>,
    decorations: Vec<Range<u64>>,
    partial: bool,
    layout: u64,
    top: u64,
    left: u64,
    index: Receipt,
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
        Action::Size { .. }
            | Action::FieldPaste(..)
            | Action::Save(_)
            | Action::SaveGuard(..)
            | Action::RetryRefresh(_)
            | Action::Command(Command::Copy)
            | Action::Clipboard { cut: false, .. }
            | Action::NativeCopy { cut: false, .. }
            | Action::RecoveryPage { .. }
            | Action::SearchPage(PageRequest { jump: None, .. })
    )
}
pub(super) fn clears_anchor(action: &Action) -> bool {
    matches!(
        action,
        Action::Insert(_)
            | Action::Paste(_)
            | Action::Clipboard { cut: true, .. }
            | Action::NativeCopy { cut: true, .. }
            | Action::NativePaste { .. }
            | Action::Hit { .. }
            | Action::GoTo { .. }
            | Action::ClearSelection
            | Action::ToggleFollow(_)
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
            | Action::Clipboard { cut: true, .. }
            | Action::NativeCopy { cut: true, .. }
            | Action::NativePaste { .. }
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
    pub(super) fn page(&mut self, request: PageRequest) {
        self.results.request(request, &self.status.rows);
    }

    pub(super) fn holds_stream(&self, tab: TabId) -> bool {
        self.run.as_ref().is_some_and(|run| {
            !run.cancel.is_cancelled()
                && matches!(
                    run.request.operation,
                    Operation::First | Operation::Next | Operation::Previous
                )
                && run.held_stream == Some(tab)
        })
    }

    pub(super) fn clear_stream_anchor(&mut self) {
        self.stream_anchor = None;
    }

    /// At most one stream can hold publication. If All Open had to release an
    /// earlier extent, only its former EOF boundary can change an accepted match.
    fn navigation_candidate(
        &self,
        owner: &mut Coordinator,
        run: &mut Run,
    ) -> Result<Option<(bool, Found)>, Error> {
        run.candidates.sort_by_key(|(rank, _, _)| *rank);
        for (_, wrapped, mut found) in run.candidates.drain(..) {
            let Some(state) = owner.state(found.tab) else {
                continue;
            };
            if state.screen.document != Some(found.document) {
                continue;
            }
            let Some(info) = &state.info else {
                continue;
            };
            if info.revision != found.revision {
                if state.live.is_none() || info.len < found.extent {
                    continue;
                }
                if found.range.end == found.extent && info.len > found.extent {
                    let snapshot = owner.background_snapshot(found.tab, found.document)?;
                    let next = snapshot.read_window(ByteOffset(found.extent), 4)?;
                    let following = next.text().chars().next();
                    if run.request.query.whole_word
                        && following.is_some_and(regex_syntax::is_word_character)
                    {
                        continue;
                    }
                    if following == Some('\n') && found.extent > 0 {
                        let at = snapshot.floor_scalar(ByteOffset(found.extent - 1))?;
                        if snapshot.read_window(at, 4)?.text().starts_with('\r') {
                            continue;
                        }
                    }
                }
                found.revision = owner
                    .state(found.tab)
                    .and_then(|state| state.info.as_ref())
                    .ok_or(Error::Closed)?
                    .revision;
            }
            return Ok(Some((wrapped, found)));
        }
        Ok(None)
    }

    pub(super) fn stream_advanced(&mut self, tab: TabId, revision: document::Revision) {
        if self
            .status
            .current
            .as_ref()
            .is_some_and(|found| found.tab == tab && found.revision != revision)
        {
            self.status.current = None;
        }
        if self
            .status
            .decorated
            .is_some_and(|(target, previous, ..)| target == tab && previous != revision)
        {
            self.status.decorations.clear();
            self.status.decorated = None;
        }
    }

    pub(super) fn stream_promoted(&mut self, tab: TabId) {
        if self
            .stream_anchor
            .as_ref()
            .is_some_and(|found| found.tab == tab)
        {
            self.clear_stream_anchor();
        }
        self.stream_advanced(tab, document::Revision(u64::MAX));
    }

    pub(super) fn busy(&self) -> bool {
        self.run.is_some() || self.pending.is_some() || self.maintaining.is_some()
    }
    pub(super) fn cancel(&mut self, invalidate: bool) {
        if invalidate {
            self.stream_anchor = None;
        }
        self.decorations_enabled = false;
        self.pending = None;
        if let Some(work) = &self.maintaining {
            work.cancel.cancel();
        }
        if let Some(run) = &self.run {
            run.cancel.cancel();
            if run.request.operation != Operation::Decorate {
                self.status.message = "Cancelling search".into();
            }
        }
        if invalidate {
            self.status.current = None;
            self.status.find_submission = None;
            self.status.decorations.clear();
            self.status.decorated = None;
            self.query = None;
            self.result_scope = None;
            self.results.live.clear();
            self.results.changes.clear();
            self.status.rows.error = None;
            self.status.rows.feedback_incomplete = false;
            self.results.reset_page(&mut self.status.rows);
            self.status.outcomes.clear();
            self.status.message = "Enter search text or press Find".into();
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
        self.results.shutdown();
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
        if request.operation == Operation::ReplaceNext
            && let Some((generation, row)) = request.expected
            && (self.status.rows.generation != generation || self.status.rows.current != Some(row))
        {
            return Err(Error::StaleRevision);
        }
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
        if self.query.as_ref() != Some(&request.query) {
            self.clear_stream_anchor();
        }
        let scope = request.replacement_scope.as_ref();
        let selected = scope.map_or(owner.tab.screen.tab, |scope| scope.current);
        let read_only = |id| {
            scope.is_some_and(|scope| scope.tabs.contains(&(id, true)))
                || owner
                    .state(id)
                    .is_some_and(|state| state.capability == crate::search::Capability::ReadOnly)
        };
        if !request.all
            && matches!(
                request.operation,
                Operation::ReplaceNext | Operation::ReplaceAll
            )
            && selected.is_some_and(read_only)
        {
            return Err(Error::ReadOnly);
        }
        let exact = (request.operation == Operation::ReplaceNext)
            .then_some(current.clone())
            .flatten()
            .filter(|found| (request.all || Some(found.tab) == selected) && !read_only(found.tab));
        let active = exact.as_ref().map(|f| f.tab).or(selected);
        let targets: Vec<_> = scope.map_or_else(
            || {
                owner
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .filter(|id| request.all || Some(*id) == active)
                    .collect()
            },
            |scope| {
                scope
                    .tabs
                    .iter()
                    .map(|(tab, _)| *tab)
                    .filter(|id| request.all || Some(*id) == active)
                    .collect()
            },
        );
        let read_only = targets
            .iter()
            .copied()
            .filter(|id| read_only(*id))
            .collect();
        let mut request = request;
        if request.operation == Operation::ReplaceNext && exact.is_none() {
            request.operation = Operation::Next;
        }
        let first = request.operation == Operation::First;
        let anchor = if first {
            0
        } else {
            targets
                .iter()
                .position(|id| Some(*id) == active)
                .unwrap_or(0)
        };
        let backwards = request.operation == Operation::Previous;
        let bound = if first {
            0
        } else {
            current
                .as_ref()
                .filter(|found| Some(found.tab) == active)
                .map_or(owner.tab.screen.position.caret, |found| {
                    if backwards {
                        found.range.start
                    } else {
                        found.range.end
                    }
                })
        };
        let cancel = Cancellation::default();
        let navigation = request
            .navigation
            .unwrap_or_else(|| owner.view_cancel.search_navigation.load(Ordering::Acquire));
        let stream_resume = self.stream_anchor.clone().filter(|found| {
            Some(found.tab) == active
                && matches!(request.operation, Operation::Next | Operation::Previous)
        });
        let run = Run {
            _permit: None,
            request,
            targets,
            anchor,
            bound,
            next: 0,
            cancel,
            navigation,
            submission: first.then_some(owner.active_sequence),
            worker: None,
            candidates: Vec::new(),
            held_stream: None,
            wrap,
            exact,
            stream_resume,
            read_only,
            progress: Arc::new(Mutex::new(Progress::default())),
            receipts_started: false,
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
            let scope = (
                run.request.all,
                (!run.request.all)
                    .then(|| run.targets.first().copied())
                    .flatten(),
            );
            let same_search =
                self.query.as_ref() == Some(&run.request.query) && self.result_scope == Some(scope);
            if !same_search {
                self.results.changes.clear();
            }
            self.result_scope = Some(scope);
            if !same_search || !replacing {
                self.results.live.clear();
            }
            let mut rows = std::mem::take(&mut self.status.rows);
            rows.error = None;
            rows.feedback_incomplete &= same_search;
            self.results.reset_page(&mut rows);
            self.status = Status {
                busy: !run.request.query.literal.is_empty(),
                replacing,
                message: "Searching".into(),
                rows,
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
        let listing = run.request.operation == Operation::List;
        let changed_scope = !run.request.all && self.result_scope.is_none() && self.query.is_some();
        let candidate = if successful && !replaced && !decorating && !listing && !changed_scope {
            self.navigation_candidate(owner, &mut run)?
        } else {
            None
        };
        self.status.message = if decorating {
            // Highlighting owns decorations. An edit may have invalidated the
            // list and changed its message while this pass was still reading.
            if !run.cancel.is_cancelled()
                && let Some(reason) = reason
            {
                owner.event(Event::Failure(reason));
            }
            std::mem::take(&mut self.status.message)
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
        } else if let Some((wrapped, found)) = candidate {
            let revealed = owner.reveal_search_result(&found, None, run.navigation, &run.cancel)?;
            if revealed {
                self.stream_anchor = owner.tab.live.is_some().then(|| found.clone());
                self.status.current = Some(found);
                if self.status.rows.count != 0 {
                    self.results.reset_page(&mut self.status.rows);
                }
            }
            format!(
                "{} matches{}{}",
                self.status.total(),
                if wrapped && revealed { "; Wrapped" } else { "" },
                if revealed {
                    "; exact match differs from ordinary selection"
                } else {
                    ""
                },
            )
        } else if listing && self.status.total() != 0 {
            format!("{} matches", self.status.total())
        } else if run.wrap || listing {
            "No matches".into()
        } else {
            "No further matches".into()
        };
        if !decorating && run.cancel.is_cancelled() {
            self.status.message = format!(
                "Cancelled; {} processed; {} changed documents",
                self.status.processed, self.status.changed
            );
        }
        if let Some(submission) = run.submission {
            self.status.find_submission = Some(submission);
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
                "{}{} processed; {} changed documents; {}",
                self.status.processed,
                if listing { " occurrences" } else { "" },
                self.status.changed,
                self.status.message
            );
        }
        if changed_scope && !replaced && !decorating {
            self.status.message = "Current tab changed; press Find".into();
        }
        let auto = replaced
            && successful
            && run.request.operation == Operation::ReplaceNext
            && !run.cancel.is_cancelled()
            && run.navigation == owner.view_cancel.search_navigation.load(Ordering::Acquire);
        let refresh_list = replaced
            && successful
            && run.request.browse
            && matches!(
                run.request.operation,
                Operation::ReplaceAll | Operation::ReplaceNext
            )
            && !auto
            && self.query.as_ref() == Some(&run.request.query)
            && self.result_scope.is_some()
            && self.status.changed != 0
            && self.deferred.is_none();
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
        } else if refresh_list {
            let processed = self.status.processed;
            let changed = self.status.changed;
            let outcomes = self.status.outcomes.clone();
            let mut request = run.request;
            request.operation = Operation::List;
            self.start(owner, request, false)?;
            self.status.processed = processed;
            self.status.changed = changed;
            self.status.replacement_outcomes = outcomes;
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
    /// Resolve the target and viewport before committing a navigation action. The
    /// terminal can revoke authority while storage reads or view preparation wait.
    fn reveal_search_result(
        &mut self,
        found: &Found,
        caret: Option<u64>,
        navigation: u64,
        cancel: &Cancellation,
    ) -> Result<bool, Error> {
        let authority = cancel.for_epoch(self.view_cancel.search_navigation.clone(), navigation);
        if authority.is_cancelled() || self.parked_action {
            return Ok(false);
        }
        let snapshot = self.background_snapshot(found.tab, found.document)?;
        if snapshot.revision() != found.revision || authority.is_cancelled() {
            return Ok(false);
        }
        let mut text = Text::new(&snapshot, &authority);
        let position = match caret.map_or_else(
            || {
                let ceil = text.ceil(found.range.start)?;
                if ceil == found.range.start {
                    Ok(ceil)
                } else {
                    text.previous(ceil)
                }
            },
            Ok,
        ) {
            Ok(caret) => Position {
                caret,
                anchor: caret,
            },
            Err(Error::Cancelled) if authority.is_cancelled() => return Ok(false),
            Err(error) => return Err(error),
        };
        let target = self.state(found.tab).ok_or(Error::Closed)?;
        let request = ViewRequest {
            position,
            top: target.top,
            left: target.left,
            width: self.width,
            height: self.height,
            reveal: true,
            generation: self.generation,
        };
        let same_tab = self.tab.screen.tab == Some(found.tab);
        let mut cache = if same_tab {
            std::mem::take(&mut self.cache)
        } else {
            Checkpoints::default()
        };
        // As in ordinary layout, retain at most the displayed view and one in flight.
        self.tab.screen.view = None;
        if let Ok(mut slot) = self.output.lock() {
            slot.take();
        }
        let result = text.view_revealing_match(
            request,
            &mut cache,
            (self.search_cover != 0)
                .then_some((self.search_cover, found.range.end.saturating_sub(1))),
        );
        drop(snapshot);
        match result {
            Ok(view) if !authority.is_cancelled() => {
                self.activate(found.tab);
                self.tab.pause_follow();
                self.tab.screen.position = position;
                self.tab.preferred = None;
                self.tab.top = view.top;
                self.tab.left = view.left;
                self.tab.screen.view = Some(Arc::new(view));
                self.tab.view_retry = None;
                self.cache = cache;
                Ok(true)
            }
            result => {
                let error = result.err();
                if same_tab {
                    self.cache = cache;
                }
                self.layout(false)?;
                match error {
                    Some(Error::Cancelled) | None => Ok(false),
                    Some(error) => Err(error),
                }
            }
        }
    }

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
            let decorating = state.operation == Some(Operation::Decorate);
            state.cancel(false);
            state.status.busy = false;
            state.status.replacing = false;
            self.view_cancel
                .search_replacing
                .store(false, Ordering::Release);
            if !decorating {
                state.status.message = format!(
                    "Stopped: {error}; {} processed, {} changed documents",
                    state.status.processed, state.status.changed
                );
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
        let different_target = self
            .result_scope
            .is_some_and(|(all, tab)| !all && tab != owner.tab.screen.tab);
        let results_changed = if different_target {
            self.result_scope = None;
            self.results.live.clear();
            self.results.changes.clear();
            self.status.outcomes.clear();
            self.status.current = None;
            self.status.rows.feedback_incomplete = false;
            self.status.message = "Current tab changed; press Find".into();
            true
        } else {
            self.results.prune(owner)
        };
        if results_changed {
            self.results.reset_page(&mut self.status.rows);
        }
        let (page_changed, jump) =
            self.results
                .poll(owner, &mut self.status, self.run.is_none())?;
        if let Some(jump) = jump
            && owner.reveal_search_result(
                &jump.found,
                Some(jump.caret),
                jump.navigation,
                &Cancellation::default(),
            )?
        {
            self.stream_anchor = owner.tab.live.is_some().then(|| jump.found.clone());
            self.status.current = Some(jump.found);
            self.status.rows.current = Some(jump.row);
            self.status.rows.ordinal = jump.ordinal;
        }
        if self.maintaining.is_some() {
            return self.poll_maintenance(owner);
        }
        // Refresh/undo/edit invalidate counts without ever applying old ranges.
        let mut invalidated = results_changed;
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
        if self.status.current.is_none() && self.status.rows.current.take().is_some() {
            self.status.rows.ordinal = None;
            invalidated = true;
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
                            replacement_scope: None,
                            navigation: None,
                            query,
                            replacement: String::new(),
                            all: false,
                            operation: Operation::Decorate,
                            browse: false,
                            expected: None,
                        },
                        false,
                    )?;
                    return Ok(true);
                }
            }
            return Ok(invalidated || page_changed);
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
                        let (index, failed) = scan.index;
                        if self.query.as_ref() == Some(&run.request.query)
                            && self.result_scope.is_some()
                        {
                            if let Some(index) = index {
                                self.results.live.push(index);
                                self.results.reset_page(&mut self.status.rows);
                            }
                            if failed {
                                self.status.rows.error = Some("Occurrence index incomplete".into());
                            }
                        }
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
                        {
                            let fallback = if previous && range.end == scan.extent {
                                if index == run.anchor && !wrapped {
                                    scan.preceding_eligible
                                } else {
                                    scan.penultimate
                                }
                            } else {
                                None
                            };
                            for range in std::iter::once(range).chain(fallback) {
                                run.candidates.push((
                                    rank,
                                    wrapped,
                                    Found {
                                        tab,
                                        document: scan.document,
                                        revision: scan.revision,
                                        range,
                                        extent: scan.extent,
                                    },
                                ));
                            }
                        }
                        run.held_stream = run
                            .candidates
                            .iter()
                            .min_by_key(|(rank, _, _)| *rank)
                            .map(|(_, _, candidate)| candidate.tab)
                            .filter(|tab| target_is_stream(owner, *tab));
                        if owner.tab.screen.tab == Some(tab) {
                            self.status.decorated =
                                Some((tab, scan.revision, scan.layout, scan.top, scan.left));
                            self.status.decorations = scan.decorations;
                            self.status.partial_highlighting = scan.partial;
                        }
                    }
                }
                Ok(Finished::Prepared(prepared, count, receipt)) => {
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
                                    receipt,
                                ))
                            })?;
                    run.worker = Some(Worker { tab, handle });
                    self.run = Some(run);
                    return Ok(true);
                }
                Ok(Finished::Mapped(prepared, count, mapping, receipt)) => {
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
                        let handle =
                            thread::spawn(move || Ok(Finished::Prepared(prepared, count, receipt)));
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
                    if report.changed
                        && run.request.browse
                        && self.query.as_ref() == Some(&run.request.query)
                        && self.result_scope.is_some()
                    {
                        if !run.receipts_started {
                            self.results.changes.clear();
                            self.status.rows.feedback_incomplete = false;
                            run.receipts_started = true;
                        }
                        self.results.live.retain(|index| index.tab != tab);
                        let (index, failed) = receipt;
                        if let Some(mut index) = index {
                            index.revision = report.info.revision;
                            self.results.changes.push(index);
                        }
                        self.status.rows.feedback_incomplete |= failed;
                        self.results.reset_page(&mut self.status.rows);
                    }
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
        ) && (run.read_only.contains(&tab)
            || target.capability == crate::search::Capability::ReadOnly)
        {
            self.status.outcomes.push(Outcome {
                tab,
                count: 0,
                revision: None,
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
                    let mut index = Writer::new(request.browse);
                    let record_change = |index: &mut Writer, range: TextRange| {
                        if request.browse {
                            match replacement_changes(task.snapshot(), &request, range, &cancel) {
                                Ok(true) => index.push(&(range.start().0..range.end().0)),
                                Ok(false) => {}
                                Err(_) => index.failed = true,
                            }
                        }
                    };
                    let count = if let Some(exact) = exact {
                        if exact.document != document || exact.revision != revision {
                            return Err(Error::StaleRevision);
                        }
                        let range = TextRange::new(exact.range.start, exact.range.end)?;
                        record_change(&mut index, range);
                        transaction.replace(range, request.replacement.as_bytes(), &cancel)?;
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
                                    record_change(&mut index, range);
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
                    let change = if request.replacement.is_empty() {
                        "Removed"
                    } else {
                        "Replaced"
                    };
                    let receipt = if index.has_records() {
                        match task.snapshot().duplicate_live() {
                            Ok(snapshot) => {
                                index.finish(tab, document, revision, Some(snapshot), Some(change))
                            }
                            Err(_) => {
                                index.failed = true;
                                None
                            }
                        }
                    } else {
                        None
                    };
                    Ok(Finished::Prepared(
                        Box::new(task.run(transaction, &cancel)?),
                        count,
                        (receipt, index.failed),
                    ))
                })?;
            run.worker = Some(Worker { tab, handle });
        } else {
            let change_start = self.results.single_change_start(tab);
            let snapshot = owner.background_snapshot(tab, document)?;
            let resume = run
                .stream_resume
                .as_ref()
                .filter(|found| found.tab == tab && found.document == document)
                .map(|found| found.range.clone());
            if target_is_stream(owner, tab) && request.operation != Operation::Decorate {
                run.held_stream = Some(tab);
                self.status.message = format!(
                    "Retained history · {} bytes · ingestion continues",
                    snapshot.len()
                );
            }
            let handle = thread::Builder::new()
                .name("ede-search-scan".into())
                .spawn(move || {
                    let mut index =
                        Writer::new(request.browse && request.operation != Operation::Decorate);
                    let mut before_change = 0;
                    let mut result = ScanResult {
                        document,
                        revision,
                        extent: snapshot.len(),
                        count: 0,
                        first: None,
                        last: None,
                        penultimate: None,
                        eligible: None,
                        preceding_eligible: None,
                        decorations: Vec::with_capacity(256),
                        partial: false,
                        layout,
                        top,
                        left,
                        index: (None, false),
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
                                if change_start.is_some_and(|start| range.start < start) {
                                    before_change += 1;
                                }
                                index.push(&range);
                                if result.first.is_none() {
                                    result.first = Some(range.clone());
                                }
                                result.penultimate = result.last.replace(range.clone());
                                if request.operation == Operation::Previous {
                                    if resume.as_ref().map_or(range.end <= bound, |prior| {
                                        range.start <= prior.start && range != *prior
                                    }) {
                                        result.preceding_eligible =
                                            result.eligible.replace(range.clone());
                                    }
                                } else if resume.as_ref().map_or(range.start >= bound, |prior| {
                                    range.start >= prior.start && range != *prior
                                }) && result.eligible.is_none()
                                {
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
                    let finished =
                        index
                            .finish(tab, document, revision, None, None)
                            .map(|mut index| {
                                index.before_change = change_start.map(|_| before_change);
                                index
                            });
                    result.index = (finished, index.failed);
                    Ok(Finished::Scanned(result))
                })?;
            run.worker = Some(Worker { tab, handle });
        }
        self.run = Some(run);
        Ok(true)
    }
}

/// Case-folded matches may use the same bytes as the replacement.
fn replacement_changes(
    snapshot: &Snapshot,
    request: &SearchRequest,
    range: TextRange,
    cancel: &Cancellation,
) -> Result<bool, Error> {
    let length = range.end().0 - range.start().0;
    if length != request.replacement.len() as u64 {
        return Ok(true);
    }
    if request.query.match_case {
        return Ok(request.query.literal != request.replacement);
    }
    snapshot
        .read_window_with_cancel(range.start(), (length as usize).max(4), cancel)
        .map(|window| {
            window.text().as_bytes().get(..length as usize) != Some(request.replacement.as_bytes())
        })
}

fn target_is_stream(owner: &Coordinator, tab: TabId) -> bool {
    owner.state(tab).is_some_and(|state| state.live.is_some())
}
