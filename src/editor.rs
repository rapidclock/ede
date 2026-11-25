//! Ordered interactive coordinator over StorageService, running away from the terminal.
//!
//! Storage commits remain authoritative. Caret/history changes publish only after
//! successful mutations. One bounded input queue and one coalescing screen slot
//! prevent slow storage or a slow terminal from growing resident state.
use crate::{
    command::{Command, Movement},
    document::{
        self, ByteOffset, Cancellation, DocumentId, DocumentInfo, Error, Job, JobId, OpenOptions,
        OpenProgress, Request, Response, SaveOutcome, SaveTarget, Snapshot, StorageService,
        TextRange,
    },
    spool::Spool,
    syntax,
    text::{Checkpoints, Position, Text, View, ViewRequest},
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
mod clipboard;
mod recovery;
mod refresh;
use refresh::{Monitor, Watch};
pub(crate) mod search;
mod session;
mod streams;
pub(crate) use streams::StreamStatus;
#[path = "editor/syntax.rs"]
mod syntax_state;
pub(crate) use recovery::RecoveryPage;
#[cfg(test)]
pub(crate) use recovery::RecoveryRow;
use recovery::{RetainedScratch, Retention};
pub(crate) use session::absolute;
pub(crate) use session::{LineEndingStatus, TabId, TabSummary};
use session::{OpenJob, RunningSave, SaveIntent, Tab, TabState, WaitingGuard};
/// Tab-local input revokes prepared refreshes while queued intentions retain their charge.
#[derive(Default)]
struct InputEpoch {
    value: Arc<AtomicU64>,
    pending: AtomicUsize,
    read_only: AtomicBool,
}
/// Shared tab admission state is small and never holds a lock during filesystem I/O.
#[derive(Default)]
struct ViewControl {
    syntax: Arc<syntax::ProcessControl>,
    active: Mutex<Cancellation>,
    layout: Arc<AtomicU64>,
    navigation: Arc<AtomicU64>,
    targets: Mutex<Vec<(TabId, Arc<InputEpoch>)>>,
    current: Mutex<Option<TabId>>,
    receiving: Mutex<Option<TabId>>,
    search: Mutex<Cancellation>,
    search_navigation: AtomicU64,
    search_replacing: AtomicBool,
    streamed: AtomicBool,
    export: Mutex<Option<Cancellation>>,
    clipboard: Mutex<Option<ClipboardTransfer>>,
    /// Serializes successful copies with the first nonblocking tty write.
    clipboard_generation: Arc<Mutex<u64>>,
}

/// One prepared terminal clipboard write, tied to the dispatch authorization.
pub(crate) struct ClipboardTransfer {
    pub epoch: u64,
    pub cut: bool,
    pub sequence: u64,
    pub frame: Vec<u8>,
    pub generation: u64,
    pub authority: Arc<Mutex<u64>>,
}

impl ClipboardTransfer {
    /// Admit the first byte only while this is still the newest successful copy.
    /// The caller must perform one nonblocking write and release before waiting.
    pub(crate) fn write_first(
        &self,
        write: impl FnOnce(&[u8]) -> std::io::Result<usize>,
    ) -> std::io::Result<usize> {
        let generation = self
            .authority
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *generation != self.generation {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        write(&self.frame)
    }
}

const SYSTEM_CLIPBOARD_LIMIT: u64 = 8 * 1024;
pub(crate) const COPIED_INTERNALLY: &str = "Copied internally";
pub(crate) const CUT_INTERNALLY: &str = "Cut internally";
impl ViewControl {
    /// Bind a queued replacement to the current tab and the capabilities visible at dispatch.
    fn replacement_scope(&self) -> search::ReplacementScope {
        let current = *self.current.lock().unwrap_or_else(|e| e.into_inner());
        let targets = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        search::ReplacementScope {
            current,
            tabs: targets
                .iter()
                .map(|(tab, epoch)| (*tab, epoch.read_only.load(Ordering::Acquire)))
                .collect(),
        }
    }

    fn admit(&self, action: &Action) -> Vec<Arc<InputEpoch>> {
        let current = *self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let all = matches!(
            action,
            Action::Size { .. }
                | Action::Switch(_)
                | Action::CycleTab(_)
                | Action::New
                | Action::Open(_)
                | Action::OpenRetained { .. }
                | Action::Guard(After::Quit)
                | Action::FinishQuit
        );
        let target = match action {
            Action::ForTab(tab, _)
            | Action::RetryRefresh(tab)
            | Action::RemoveTab(tab)
            | Action::CancelOpen(tab) => Some(*tab),
            Action::Guard(after) | Action::SaveGuard(after, _) | Action::Discard(after, _) => {
                after.target().or(current)
            }
            Action::RecoveryPage { .. }
            | Action::DiscardRetained { .. }
            | Action::SetLanguage(..)
            | Action::RetryHighlighting(..)
            | Action::InspectLineEnding(_)
            | Action::SyntaxTheme(..)
            | Action::ReloadHighlighting
            | Action::FieldPaste(..)
            | Action::CancelGuard => None,
            _ => current,
        };
        let targets = self
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let epochs: Vec<_> = targets
            .iter()
            .filter(|(tab, _)| all || Some(*tab) == target)
            .map(|(_, epoch)| epoch.clone())
            .collect();
        for epoch in &epochs {
            epoch.pending.fetch_add(1, Ordering::AcqRel);
            epoch.value.fetch_add(2, Ordering::AcqRel);
        }
        epochs
    }
    fn set_receiving(&self, tab: Option<TabId>) {
        let mut receiving = self
            .receiving
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *receiving == tab {
            return;
        }
        let targets = self
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let changed: Vec<_> = targets
            .iter()
            .filter(|(id, _)| Some(*id) == *receiving || Some(*id) == tab)
            .map(|(_, epoch)| epoch.clone())
            .collect();
        for epoch in &changed {
            epoch.value.fetch_add(1, Ordering::AcqRel);
        }
        *receiving = tab;
        for epoch in &changed {
            epoch.value.fetch_add(1, Ordering::AcqRel);
        }
    }
    fn receiving_for(&self, tab: TabId) -> bool {
        self.receiving
            .lock()
            .map_or(true, |receiving| *receiving == Some(tab))
    }
    /// Inspect or replace the current read token without changing its generation.
    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Cancellation>> {
        self.active.lock()
    }
}

/// Follow-up protected by the unsaved-content guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum After {
    Quit,
    QuitTab(TabId),
    Close(TabId),
}
/// Ordered intent submitted by the terminal shell.
#[derive(Clone, Debug)]
pub(crate) enum Action {
    Search(search::SearchRequest),
    SearchPage(search::results::PageRequest),
    CancelSearch,
    InvalidateSearch,
    InvalidateReplacement,
    RevokeSearch,
    Command(Command),
    Clipboard {
        cut: bool,
        system: Option<u64>,
    },
    NativeCopy {
        cut: bool,
        access: crate::clipboard::Access,
    },
    NativePaste {
        access: crate::clipboard::Access,
        observed: u64,
    },
    Insert(String),
    Paste(Arc<Spool>),
    FieldPaste(Arc<Spool>, u64),
    Open(PathBuf),
    Follow(PathBuf),
    ToggleFollow(TabId),
    RetryFinish(TabId),
    ExportRetained(TabId, PathBuf),
    New,
    Switch(TabId),
    CycleTab(bool),
    RemoveTab(TabId),
    CancelOpen(TabId),
    CancelActiveOpen(Option<u64>),
    CancelGuard,
    FinishQuit,
    RetryRefresh(TabId),
    InspectLineEnding(TabId),
    SetLanguage(TabId, syntax::Choice),
    RetryHighlighting(TabId),
    SyntaxTheme(Arc<str>),
    ReloadHighlighting,
    ApplySettings(crate::settings::Values, u64),
    RecoveryPage {
        generation: u64,
        cursor: Option<(u64, crate::recovery::Id)>,
        previous: bool,
    },
    OpenRetained {
        generation: u64,
        scratch: crate::recovery::Id,
    },
    DiscardRetained {
        generation: u64,
        scratch: crate::recovery::Id,
    },
    ForTab(TabId, Box<Action>),
    Save(Option<PathBuf>),
    SaveGuard(After, Option<PathBuf>),
    Guard(After),
    Discard(After, Option<JobId>),
    Size {
        width: u16,
        height: u16,
        generation: u64,
        search_cover: u16,
    },
    Scroll(i64, i64),
    Hit {
        caret: u64,
        extend: bool,
        generation: u64,
        document: DocumentId,
        revision: document::Revision,
        top: u64,
        left: u64,
        gesture: PointerGesture,
    },
    ClearSelection,
    GoTo {
        line: u64,
        column: u64,
    },
}
/// A drag can extend only a press that established a caret in a valid viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerGesture {
    Press(u64),
    Drag(u64),
}
impl Action {
    /// Input that can move the original caret or change which document owns it.
    pub(crate) fn invalidates_clipboard_target(&self) -> bool {
        match self {
            Self::ForTab(_, action) => action.invalidates_clipboard_target(),
            Self::Clipboard { .. } | Self::NativeCopy { .. } | Self::NativePaste { .. } => false,
            _ => {
                search::clears_anchor(self)
                    || matches!(
                        self,
                        Self::Search(_)
                            | Self::SearchPage(search::results::PageRequest { jump: Some(_), .. })
                            | Self::Guard(_)
                            | Self::RemoveTab(_)
                            | Self::New
                            | Self::Open(_)
                            | Self::Follow(_)
                            | Self::CancelActiveOpen(_)
                            | Self::CancelOpen(_)
                            | Self::OpenRetained { .. }
                    )
            }
        }
    }

    /// Resident payload accounting; disk-backed paste bytes are excluded.
    fn bytes(&self) -> usize {
        (match self {
            Self::Search(request) => {
                request.query.literal.capacity() + request.replacement.capacity()
            }
            Self::Insert(text) => text.capacity(),
            Self::Open(path) | Self::Save(Some(path)) | Self::SaveGuard(_, Some(path)) => {
                path.capacity()
            }
            Self::ForTab(_, action) => action.bytes(),
            _ => 0,
        }) + 128
            + crate::TAB_LIMIT * std::mem::size_of::<Arc<InputEpoch>>()
    }
}
/// Only combine collinear wheel deltas with the same direction: clamping is order-dependent.
fn scrolls_combine(v: i64, h: i64, next_v: i64, next_h: i64) -> bool {
    (h == 0 && next_h == 0 && v.signum() == next_v.signum())
        || (v == 0 && next_v == 0 && h.signum() == next_h.signum())
}
/// UI-facing asynchronous control event, kept separate from the replaceable screen slot.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Exported(String),
    StreamGuard(u64, After, String),
    Guard(u64, After, bool),
    QuitTabs(u64, Vec<TabId>),
    Proceed(u64, After),
    NeedSaveAs(u64, TabId),
    NeedGuardSaveAs(u64, After, TabId),
    GuardSaveStopped(u64),
    Saved(TabId, SaveResult, String),
    Failure(String),
    OpenFailed(String),
    Uncertain(u64, After, JobId),
    FieldPaste(u64, Result<String, String>),
    QuitReady(u64),
    PersistenceFailed(TabId, After, String),
    RecoveryPage(u64, Arc<RecoveryPage>),
}
/// Save outcomes that require different handling while a discard is pending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveResult {
    Saved,
    Cancelled,
    Failed,
    Uncertain,
}
/// Most recent document state plus prepared immutable viewport.
#[derive(Clone)]
pub(crate) struct Screen {
    pub stream: Option<StreamStatus>,
    pub exporting: bool,
    pub syntax_language: syntax::Choice,
    pub syntax_config: Arc<syntax::Config>,
    pub syntax_status: syntax::Status,
    pub syntax_projection: Option<Arc<syntax::Projection>>,
    pub syntax_palette: syntax::Palette,
    pub syntax_theme: Arc<str>,
    pub syntax_notice: Option<Arc<str>>,
    pub syntax_epoch: u64,
    pub settings_generation: u64,
    pub can_replace: bool,
    pub search: Arc<search::Status>,
    pub tab: Option<TabId>,
    pub tabs: Arc<[TabSummary]>,
    pub reserved_paths: Arc<[Arc<Path>]>,
    pub document: Option<DocumentId>,
    pub view: Option<Arc<View>>,
    pub preview: Option<String>,
    pub position: Position,
    pub path: Option<PathBuf>,
    pub modified: bool,
    pub loading: bool,
    pub message: String,
    pub acknowledged: u64,
    /// The Copy/Cut responsible for a retained clipboard status, independent of layout ACKs.
    pub clipboard_sequence: Option<u64>,
    pub pointer_press: Option<u64>,
}
impl Default for Screen {
    /// Pending startup is visibly distinct from a ready empty buffer.
    fn default() -> Self {
        Self {
            stream: None,
            exporting: false,
            syntax_language: syntax::Choice::Auto,
            syntax_config: Arc::default(),
            syntax_status: syntax::Status::Plain,
            syntax_projection: None,
            syntax_palette: syntax::Palette::default(),
            syntax_theme: Arc::from("Dark"),
            syntax_notice: None,
            syntax_epoch: 0,
            settings_generation: 0,
            can_replace: true,
            search: Arc::default(),
            tab: None,
            tabs: Arc::default(),
            reserved_paths: Arc::default(),
            document: None,
            view: None,
            preview: None,
            position: Position::default(),
            path: None,
            modified: false,
            loading: true,
            message: "Preparing document".into(),
            acknowledged: 0,
            clipboard_sequence: None,
            pointer_press: None,
        }
    }
}
/// Aggregate pending interaction admission, including the active envelope.
#[derive(Default)]
struct QueueBudget {
    bytes: AtomicUsize,
    entries: AtomicUsize,
    paste: AtomicBool,
    cancellation: AtomicU64,
    searches: AtomicUsize,
}
/// A committed transaction whose caret lookup must survive transient read failure/cancellation.
struct CommittedEdit {
    before: Position,
    endpoint: u64,
    history_pruned: u64,
}
/// Charged message; dropping after handling releases the aggregate queue byte reservation.
struct Envelope {
    action: Action,
    reject_read_only: bool,
    sequence: u64,
    charge: usize,
    paste: bool,
    search: bool,
    paste_cancel: Option<Cancellation>,
    cancellation: u64,
    layout_epoch: u64,
    navigation_epoch: u64,
    paste_slot: Arc<Mutex<Option<Cancellation>>>,
    used: Arc<QueueBudget>,
    epochs: Vec<Arc<InputEpoch>>,
}
impl Drop for Envelope {
    fn drop(&mut self) {
        for epoch in &self.epochs {
            epoch.pending.fetch_sub(1, Ordering::AcqRel);
        }
        self.used.bytes.fetch_sub(self.charge, Ordering::AcqRel);
        self.used.entries.fetch_sub(1, Ordering::AcqRel);
        if self.search {
            self.used.searches.fetch_sub(1, Ordering::AcqRel);
        }
        if self.paste {
            if let Ok(mut slot) = self.paste_slot.lock() {
                *slot = None;
            }
            self.used.paste.store(false, Ordering::Release);
        }
    }
}
/// Terminal-facing nonblocking bridge. Drop cancels and detaches filesystem work.
pub(crate) struct Editor {
    sender: Option<SyncSender<Envelope>>,
    pub events: Receiver<Event>,
    screen: Arc<Mutex<Option<Screen>>>,
    stop: Arc<AtomicBool>,
    activity: Arc<Mutex<Cancellation>>,
    view_cancel: Arc<ViewControl>,
    used: Arc<QueueBudget>,
    paste_slot: Arc<Mutex<Option<Cancellation>>>,
    sequence: u64,
    worker: Option<JoinHandle<()>>,
}
impl Editor {
    /// Admit only the latest completed clipboard write after the UI sees its acknowledgement.
    pub(crate) fn take_clipboard(&self, acknowledged: u64) -> Option<ClipboardTransfer> {
        let mut slot = self.view_cancel.clipboard.try_lock().ok()?;
        if slot.as_ref()?.sequence > acknowledged {
            return None;
        }
        slot.take()
    }

    /// Isolate parser lifecycle from terminal job control and restoration.
    pub(crate) fn pause_highlighting(&self) -> u64 {
        self.view_cancel.syntax.pause()
    }

    /// Restart only after the terminal has restored its interactive modes.
    pub(crate) fn resume_highlighting(&self) {
        self.view_cancel.syntax.resume();
    }

    /// Keep a cleanup owner outside the coordinator and event loop.
    pub(crate) fn highlighting_process(&self) -> Arc<syntax::ProcessControl> {
        self.view_cancel.syntax.clone()
    }
    /// Start storage and coordinator workers with an optional initial path.
    pub fn new(
        launch_directory: PathBuf,
        startup: crate::stream::Startup,
        recovery: crate::recovery::Store,
        syntax_program: Option<PathBuf>,
        syntax_config_directory: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        Self::with_options(
            launch_directory,
            startup,
            recovery,
            syntax_program,
            syntax_config_directory,
            OpenOptions::default(),
        )
    }

    /// Supply storage policy and fault boundaries while retaining the normal worker loop.
    pub(crate) fn with_options(
        launch_directory: PathBuf,
        startup: crate::stream::Startup,
        recovery: crate::recovery::Store,
        syntax_program: Option<PathBuf>,
        syntax_config_directory: Option<PathBuf>,
        options: OpenOptions,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(256);
        let (event_sender, events) = mpsc::sync_channel(16);
        let screen = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(Mutex::new(Cancellation::default()));
        let view_cancel = Arc::new(ViewControl {
            syntax: Arc::new(syntax::ProcessControl::new(syntax_program)),
            ..ViewControl::default()
        });
        let view_cancellation = view_cancel.clone();
        let output = screen.clone();
        let stopping = stop.clone();
        let active = activity.clone();
        let worker = thread::Builder::new()
            .name("ede-editor".into())
            .spawn(
                move || match StorageService::with_stop_flag(options, stopping.clone()) {
                    Ok(service) => {
                        let mut coordinator = Coordinator::new(
                            service,
                            receiver,
                            event_sender,
                            output,
                            stopping,
                            active,
                            view_cancellation,
                        )
                        .with_retention(recovery);
                        coordinator.syntax = syntax_state::State::new(syntax_config_directory);
                        coordinator.run(launch_directory, startup);
                    }
                    Err(error) => {
                        if let Ok(mut slot) = output.lock() {
                            *slot = Some(Screen {
                                loading: false,
                                message: error.to_string(),
                                ..Screen::default()
                            });
                        }
                    }
                },
            )?;
        Ok(Self {
            sender: Some(sender),
            events,
            screen,
            stop,
            activity,
            view_cancel,
            used: Arc::new(QueueBudget::default()),
            paste_slot: Arc::new(Mutex::new(None)),
            sequence: 0,
            worker: Some(worker),
        })
    }
    /// Admit an ordered operation or visibly reject it without changing the document.
    pub fn submit(&mut self, mut action: Action) -> bool {
        let charge = action.bytes();
        if self
            .used
            .entries
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 256).then_some(n + 1)
            })
            .is_err()
        {
            return false;
        }
        if self
            .used
            .bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(charge).filter(|n| *n <= 1 << 20)
            })
            .is_err()
        {
            self.used.entries.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        let inner = match &action {
            Action::ForTab(_, inner) => inner,
            action => action,
        };
        if (matches!(inner, Action::CancelSearch | Action::InvalidateSearch)
            || matches!(inner, Action::InvalidateReplacement)
                && self.view_cancel.search_replacing.load(Ordering::Acquire))
            && let Ok(cancel) = self.view_cancel.search.lock()
        {
            cancel.cancel();
        }
        if search::revokes_navigation(inner) {
            self.view_cancel
                .search_navigation
                .fetch_add(1, Ordering::AcqRel);
        }
        let paste = matches!(
            inner,
            Action::Paste(_)
                | Action::FieldPaste(..)
                | Action::Command(Command::Paste)
                | Action::NativePaste { .. }
        );
        if paste
            && self
                .used
                .paste
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            self.used.entries.fetch_sub(1, Ordering::AcqRel);
            self.used.bytes.fetch_sub(charge, Ordering::AcqRel);
            return false;
        }
        let paste_cancel = paste.then(Cancellation::default);
        if paste && let Ok(mut slot) = self.paste_slot.lock() {
            *slot = paste_cancel.clone();
        }
        let sequence = self.sequence + 1;
        if matches!(
            inner,
            Action::Clipboard { .. }
                | Action::NativeCopy { .. }
                | Action::Command(Command::Copy | Command::Cut)
        ) {
            let mut generation = self
                .view_cancel
                .clipboard_generation
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *generation = sequence;
        }
        let search = matches!(inner, Action::Search(_));
        if search
            && self
                .used
                .searches
                .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < 16).then_some(count + 1)
                })
                .is_err()
        {
            self.used.entries.fetch_sub(1, Ordering::AcqRel);
            self.used.bytes.fetch_sub(charge, Ordering::AcqRel);
            return false;
        }
        // Entry admission guarantees channel capacity. Advance epochs before
        // enqueueing so even a job that has not created its read token sees it.
        let layout_epoch = if matches!(
            inner,
            Action::Size { .. }
                | Action::Scroll(..)
                | Action::Guard(_)
                | Action::Switch(_)
                | Action::CycleTab(_)
                | Action::RemoveTab(_)
        ) {
            self.view_cancel.layout.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.view_cancel.layout.load(Ordering::Acquire)
        };
        let navigation_epoch = if matches!(inner, Action::Guard(_)) {
            self.view_cancel.navigation.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.view_cancel.navigation.load(Ordering::Acquire)
        };
        let reject_read_only =
            matches!(inner, Action::Save(_) | Action::SaveGuard(..)) || search::mutates(inner);
        let epochs = self.view_cancel.admit(&action);
        let reject_read_only = reject_read_only
            && epochs
                .iter()
                .any(|epoch| epoch.read_only.load(Ordering::Acquire));
        let dispatched = match &mut action {
            Action::ForTab(_, inner) => &mut **inner,
            action => action,
        };
        if let Action::Search(request) = dispatched {
            request.navigation = Some(self.view_cancel.search_navigation.load(Ordering::Acquire));
            if matches!(
                request.operation,
                search::Operation::ReplaceNext | search::Operation::ReplaceAll
            ) {
                request.replacement_scope = Some(self.view_cancel.replacement_scope());
            }
        }
        if let Action::SearchPage(request) = dispatched
            && request.jump.is_some()
        {
            request.navigation = Some(self.view_cancel.search_navigation.load(Ordering::Acquire));
        }
        let envelope = Envelope {
            action,
            reject_read_only,
            sequence,
            charge,
            paste,
            search,
            paste_cancel,
            cancellation: self.used.cancellation.load(Ordering::Acquire),
            layout_epoch,
            navigation_epoch,
            paste_slot: self.paste_slot.clone(),
            used: self.used.clone(),
            epochs,
        };
        let Some(sender) = &self.sender else {
            return false;
        };
        match sender.try_send(envelope) {
            Ok(()) => {
                self.sequence = sequence;
                true
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
    /// Protect the receiving document until its completed paste is admitted or rejected.
    pub fn receiving(&self, tab: Option<TabId>) {
        self.view_cancel.set_receiving(tab);
    }
    /// Identity of the last admitted intention, used to reject obsolete dialog responses.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Modal ownership can change without a document action or queue admission.
    pub fn revoke_search_navigation(&self) {
        self.view_cancel
            .search_navigation
            .fetch_add(1, Ordering::AcqRel);
    }
    /// Cancel the current long operation without requiring admission to its queue.
    pub fn cancel(&self) {
        if let Ok(cancel) = self.view_cancel.search.lock() {
            cancel.cancel();
        }
        if let Ok(export) = self.view_cancel.export.lock()
            && let Some(cancel) = &*export
        {
            cancel.cancel();
        }
        self.used.cancellation.fetch_add(1, Ordering::AcqRel);
        self.cancel_paste();
        if let Ok(cancel) = self.activity.lock() {
            cancel.cancel();
        }
        if let Ok(cancel) = self.view_cancel.lock() {
            cancel.cancel();
        }
    }
    /// Sessions with independent source I/O may abandon blocked teardown after tty cleanup.
    pub(crate) fn streamed(&self) -> bool {
        self.view_cancel.streamed.load(Ordering::Acquire)
    }
    /// Cancel an admitted paste without affecting unrelated queued work.
    pub fn cancel_paste(&self) {
        if let Ok(slot) = self.paste_slot.lock()
            && let Some(cancel) = slot.as_ref()
        {
            cancel.cancel();
        }
    }
    /// Take the replaceable latest screen without waiting for storage.
    pub fn screen(&self) -> Option<Screen> {
        self.screen.lock().ok()?.take()
    }
    /// Unexpected worker termination must become a restored-terminal failure, not an eternal busy state.
    pub fn failed(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && !self.stop.load(Ordering::Acquire)
    }
    /// Whether the displayed state predates an admitted interaction.
    pub fn pending(&self, acknowledged: u64) -> bool {
        acknowledged < self.sequence
    }
    /// Stop new work and hand orderly joining to the caller after terminal restoration.
    pub fn shutdown(mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        self.cancel();
        self.sender.take();
        self.worker.take()
    }
}
impl Drop for Editor {
    /// Panic/error paths request cancellation without waiting in raw terminal mode.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel();
    }
}
/// Coordinator owns view/history metadata and serializes operations against acknowledged epochs.
struct Coordinator {
    syntax: syntax_state::State,
    search: search::State,
    service: StorageService,
    inbox: Receiver<Envelope>,
    events: SyncSender<Event>,
    output: Arc<Mutex<Option<Screen>>>,
    stop: Arc<AtomicBool>,
    activity: Arc<Mutex<Cancellation>>,
    view_cancel: Arc<ViewControl>,
    cancel: Cancellation,
    tab: TabState,
    tabs: Vec<Tab>,
    next_tab: u64,
    next_untitled: u64,
    launch_directory: PathBuf,
    opening: Option<OpenJob>,
    saving: Option<RunningSave>,
    saves: VecDeque<SaveIntent>,
    completed_open: Option<document::Completion>,
    completed_save: Option<document::Completion>,
    waiting_guard: Option<WaitingGuard>,
    acknowledged: u64,
    active_open_admission: Option<(TabId, u64)>,
    summaries: Arc<[TabSummary]>,
    parked_action: bool,
    retention: Option<Retention>,
    monitor: Option<Monitor>,
    clipboard: Option<Arc<Spool>>,
    cut_clipboard: Option<Arc<Spool>>,
    last_native_write: Option<(u64, bool)>,
    cache: Checkpoints,
    width: u16,
    height: u16,
    search_cover: u16,
    generation: u64,
    counter: u64,
    active_sequence: u64,
    layout_epoch: u64,
    navigation_epoch: u64,
    reader_slots: Arc<AtomicUsize>,
    stream_guard: Option<After>,
    quit_ready: bool,
    settings_generation: u64,
    default_line_ending: document::Newline,
    export: Option<streams::Export>,
}
/// Text or staged bytes to insert in one replacement.
enum Replacement<'a> {
    Text(String),
    Newline,
    Paste(&'a Spool),
}

impl Coordinator {
    /// Initialize empty interaction metadata; storage owns document contents.
    fn new(
        service: StorageService,
        inbox: Receiver<Envelope>,
        events: SyncSender<Event>,
        output: Arc<Mutex<Option<Screen>>>,
        stop: Arc<AtomicBool>,
        activity: Arc<Mutex<Cancellation>>,
        view_cancel: Arc<ViewControl>,
    ) -> Self {
        Self {
            syntax: syntax_state::State::default(),
            search: search::State::default(),
            service,
            inbox,
            events,
            output,
            stop,
            activity,
            view_cancel,
            cancel: Cancellation::default(),
            tab: TabState::default(),
            tabs: Vec::new(),
            next_tab: 0,
            next_untitled: 0,
            launch_directory: PathBuf::new(),
            opening: None,
            saving: None,
            saves: VecDeque::new(),
            completed_open: None,
            completed_save: None,
            waiting_guard: None,
            acknowledged: 0,
            active_open_admission: None,
            summaries: Arc::default(),
            parked_action: false,
            retention: None,
            monitor: None,
            clipboard: None,
            cut_clipboard: None,
            last_native_write: None,
            cache: Checkpoints::default(),
            width: 80,
            height: 22,
            search_cover: 0,
            generation: 0,
            counter: 0,
            active_sequence: 0,
            layout_epoch: 0,
            navigation_epoch: 0,
            reader_slots: Arc::default(),
            stream_guard: None,
            quit_ready: false,
            settings_generation: 0,
            default_line_ending: document::Newline::default(),
            export: None,
        }
    }
    /// Process lifecycle commands and asynchronous results in admission order.
    fn run(mut self, launch_directory: PathBuf, startup: crate::stream::Startup) {
        self.launch_directory = launch_directory.clone();
        if let Some(source) = startup.source
            && let Err(error) = self.add_stream(source, false)
        {
            self.event(Event::OpenFailed(error.to_string()));
        }
        self.start(launch_directory, startup.paths);
        self.syntax.reload();
        self.publish();
        let mut pending = None;
        while !self.stop.load(Ordering::Acquire) {
            if !self.poll_background() {
                break;
            }
            match pending
                .take()
                .map(Ok)
                .unwrap_or_else(|| self.inbox.recv_timeout(Duration::from_millis(5)))
            {
                Ok(mut envelope) => {
                    // Coalesce only adjacent disposable view requests. Text, mutations,
                    // navigation, and lifecycle intentions retain their exact ordering.
                    while let Ok(mut next) = self.inbox.try_recv() {
                        match (&envelope.action, &mut next.action) {
                            (Action::Size { .. }, Action::Size { .. }) => {}
                            (Action::Scroll(v, h), Action::Scroll(nv, nh))
                                if scrolls_combine(*v, *h, *nv, *nh)
                                    && (*v == 0 || self.tab.left == 0) =>
                            {
                                *nv = nv.saturating_add(*v);
                                *nh = nh.saturating_add(*h);
                            }
                            (
                                Action::Hit {
                                    extend: true,
                                    generation: a,
                                    document: ad,
                                    revision: ar,
                                    top: at,
                                    left: al,
                                    gesture: PointerGesture::Drag(ag),
                                    ..
                                },
                                Action::Hit {
                                    extend: true,
                                    generation: b,
                                    document: bd,
                                    revision: br,
                                    top: bt,
                                    left: bl,
                                    gesture: PointerGesture::Drag(bg),
                                    ..
                                },
                            ) if a == b
                                && ad == bd
                                && ar == br
                                && at == bt
                                && al == bl
                                && ag == bg => {}
                            _ => {
                                pending = Some(next);
                                break;
                            }
                        }
                        envelope = next;
                    }
                    self.begin_envelope(&envelope);
                    self.active_sequence = envelope.sequence;
                    // Move the admitted payload; cloning would retain an extra
                    // uncharged String while the active envelope still owns it.
                    let action = std::mem::replace(&mut envelope.action, Action::ClearSelection);
                    let result = if envelope.reject_read_only {
                        Err(Error::ReadOnly)
                    } else {
                        self.action(action)
                    };
                    let closed = matches!(result, Err(Error::Closed));
                    self.acknowledged = envelope.sequence;
                    if let Err(error) = result {
                        self.tab.screen.message = if self.tab.pending_edit.is_some() {
                            format!("Edit committed; caret lookup pending: {error}")
                        } else {
                            error.to_string()
                        };
                        if !matches!(error, Error::Cancelled | Error::ReadOnly) {
                            self.event(Event::Failure(self.tab.screen.message.clone()));
                        }
                    }
                    let storage_available = self.poll_background();
                    self.publish();
                    if closed || !storage_available {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        self.stop_streams();
        let shutdown = self.service.shutdown();
        self.syntax.shutdown(&self.view_cancel.syntax);
        self.search.shutdown();
        if let Some(monitor) = self.monitor.take() {
            monitor.shutdown();
        }
        if let Some(retention) = self.retention.take()
            && let Some(worker) = retention.service.shutdown()
        {
            let _ = worker.join();
        }
        let _ = shutdown.wait();
    }
    /// Install a new cancellation token for exactly one foreground operation.
    fn begin(&mut self) {
        self.cancel = Cancellation::default();
        if let Ok(mut active) = self.activity.lock() {
            *active = self.cancel.clone();
        }
    }
    /// Cancellation also reaches a long operation admitted before its worker started it.
    fn begin_envelope(&mut self, envelope: &Envelope) {
        self.begin();
        self.layout_epoch = envelope.layout_epoch;
        self.navigation_epoch = envelope.navigation_epoch;
        if let Some(cancel) = &envelope.paste_cancel {
            self.cancel = cancel.clone();
            if let Ok(mut active) = self.activity.lock() {
                *active = cancel.clone();
            }
        }
        if (envelope.paste || matches!(envelope.action, Action::Open(_) | Action::New))
            && envelope.cancellation != envelope.used.cancellation.load(Ordering::Acquire)
        {
            self.cancel.cancel();
        }
    }
    /// Coalesce screen updates; two bounded layouts at most are retained across the bridge.
    fn publish(&mut self) {
        self.syntax.screen(&mut self.tab.screen);
        self.tab.screen.settings_generation = self.settings_generation;
        self.tab.screen.can_replace = self.tab.capability == crate::search::Capability::Editable;
        self.tab.screen.exporting = self.export.is_some();
        self.tab.screen.search = Arc::new(self.search.status.clone());
        self.summarize();
        if let Ok(mut slot) = self.output.lock() {
            *slot = Some(self.tab.screen.clone());
        }
    }
    /// Send rare control events with bounded backpressure and shutdown cancellation.
    fn event(&self, mut event: Event) {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            match self.events.try_send(event) {
                Ok(()) => return,
                Err(TrySendError::Full(value)) => {
                    event = value;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return,
            }
        }
    }
    /// Submit a storage request, waiting on this worker and observing cancellation/progress.
    fn call_background(&mut self, request: Request) -> Result<Response, Error> {
        let foreground = std::mem::take(&mut self.cancel);
        let result = self.call(request);
        self.cancel = foreground;
        result
    }
    /// Submit a storage request, waiting on this worker and observing cancellation/progress.
    fn call(&mut self, request: Request) -> Result<Response, Error> {
        self.call_authorized(request, None, false)
    }

    fn call_authorized(
        &mut self,
        request: Request,
        authority: Option<Cancellation>,
        detect_line_ending: bool,
    ) -> Result<Response, Error> {
        let opening = matches!(
            request,
            Request::Open(_) | Request::Restore { .. } | Request::New
        );
        let cancellable = !matches!(request, Request::Info(_) | Request::Snapshot(_));
        self.counter += 1;
        let job = self
            .service
            .submit_authorized(self.counter, request, authority, detect_line_ending)
            .map_err(|error| error.error)?;
        loop {
            if self.stop.load(Ordering::Acquire) || (cancellable && self.cancel.is_cancelled()) {
                job.cancel();
            }
            while let Some(progress) = job.take_progress() {
                self.tab.screen.message = match progress {
                    OpenProgress::Preview { text, raw_bytes } => {
                        self.tab.screen.preview = Some(text);
                        format!("Loading {raw_bytes} bytes (read-only preview)")
                    }
                    OpenProgress::Preparing { completed, total } => {
                        format!("Preparing {completed}/{total} bytes")
                    }
                };
                self.publish();
            }
            if let Some(completion) = self.service.try_recv()? {
                if completion.job == job.id
                    && completion.document == job.document
                    && completion.request == self.counter
                {
                    if opening && self.cancel.is_cancelled() {
                        self.service.close(completion.document);
                        return Err(Error::Cancelled);
                    }
                    return completion.result;
                }
                self.route_completion(completion);
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    /// Current ready identity or an explicit loading error.
    fn document(&self) -> Result<DocumentId, Error> {
        self.tab.screen.document.ok_or(Error::Busy)
    }
    /// Read authoritative dirty/path/history metadata after a mutation or save.
    fn read_info(&mut self, id: DocumentId) -> Result<DocumentInfo, Error> {
        match self.call(Request::Info(id))? {
            Response::Info(info) => Ok(info),
            _ => Err(Error::CorruptStorage),
        }
    }

    fn refresh(&mut self) -> Result<(), Error> {
        let info = self.read_info(self.document()?)?;
        self.tab.apply_info(info);
        Ok(())
    }

    /// Capture a bounded immutable lease for background navigation/layout/staging.
    fn snapshot_of(&mut self, document: DocumentId) -> Result<Snapshot, Error> {
        match self.call(Request::Snapshot(document))? {
            Response::Snapshot(snapshot) => Ok(snapshot),
            _ => Err(Error::CorruptStorage),
        }
    }

    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        if let Some(snapshot) = self
            .tab
            .live
            .as_ref()
            .and_then(|live| live.snapshot.as_ref())
        {
            return snapshot.duplicate_live();
        }
        self.snapshot_of(self.document()?)
    }
    /// Render a fresh revision/geometry-tagged view; no snapshot is retained afterward.
    fn layout(&mut self, reveal: bool) -> Result<(), Error> {
        self.prepare_layout(reveal, None)
    }

    /// Leave the selected occurrence above the floating search window.
    fn layout_search(&mut self, last_byte: u64) -> Result<(), Error> {
        self.prepare_layout(true, (self.search_cover != 0).then_some(last_byte))
    }

    fn prepare_layout(&mut self, reveal: bool, match_end: Option<u64>) -> Result<(), Error> {
        self.tab.screen.view = None;
        if self.parked_action {
            return Ok(());
        }
        // Drop an unconsumed replacement before building another: the UI's current
        // Arc and this job then account for at most two prepared layout allocations.
        if let Ok(mut slot) = self.output.lock() {
            slot.take();
        }
        let cancel = self
            .cancel
            .for_epoch(self.view_cancel.layout.clone(), self.layout_epoch);
        if let Ok(mut active) = self.view_cancel.lock() {
            *active = cancel.clone();
        }
        let snapshot = self.snapshot()?;
        let following = self
            .tab
            .screen
            .stream
            .as_ref()
            .is_some_and(|status| status.following);
        if following {
            self.tab.top = snapshot.line_count().saturating_sub(u64::from(self.height));
            self.tab.left = 0;
        }
        let request = ViewRequest {
            position: self.tab.screen.position,
            top: self.tab.top,
            left: self.tab.left,
            width: self.width,
            height: self.height,
            reveal: reveal || following,
            generation: self.generation,
        };
        let mut text = Text::new(&snapshot, &cancel);
        let result = match match_end {
            Some(last) => {
                text.view_revealing_match(request, &mut self.cache, Some((self.search_cover, last)))
            }
            None => text.view(request, &mut self.cache),
        };
        let view = match result {
            Ok(view) => view,
            Err(Error::Cancelled) if !self.cancel.is_cancelled() => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.tab.top = view.top;
        self.tab.left = view.left;
        self.tab.screen.view = Some(Arc::new(view));
        self.tab.view_retry = None;
        Ok(())
    }
    /// Publish background changes and surface failures without blocking terminal input.
    fn poll_background(&mut self) -> bool {
        let result = (|| {
            let changed = self.settle_completions()?
                | self.receive()?
                | self.poll_streams()?
                | self.poll_export()?
                | self.poll_opens()?
                | self.poll_retention()?
                | self.poll_refreshes()?
                | self.poll_search()?;
            let mut syntax = std::mem::take(&mut self.syntax);
            let syntax_changed = syntax.poll(self);
            self.syntax = syntax;
            Ok(changed | syntax_changed)
        })();
        match result {
            Ok(true) => self.publish(),
            Ok(false) => {}
            Err(Error::Closed) => return false,
            Err(error) => {
                self.event(Event::Failure(error.to_string()));
                self.publish();
            }
        }
        true
    }

    /// Apply one atomic replacement, publishing view history only after successful commit.
    fn replace(
        &mut self,
        replacement: Replacement<'_>,
        range: std::ops::Range<u64>,
    ) -> Result<(), Error> {
        self.replace_authorized(replacement, range, None)
    }

    fn replace_authorized(
        &mut self,
        replacement: Replacement<'_>,
        range: std::ops::Range<u64>,
        authority: Option<(clipboard::Target, Cancellation)>,
    ) -> Result<(), Error> {
        if self.tab.capability == crate::search::Capability::ReadOnly {
            return Err(Error::ReadOnly);
        }
        if let Some(tab) = self.tab.screen.tab {
            self.yield_refresh_for_edit(tab)?;
        }
        if self
            .tab
            .info
            .as_ref()
            .is_some_and(|info| info.compaction_pending)
            && let Some(tab) = self.tab.screen.tab
        {
            self.search.cancel(false);
            self.yield_checkpoint(tab)?;
        }
        let info = self.tab.info.as_ref().ok_or(Error::Busy)?;
        let expected = info.revision;
        let before = self.tab.screen.position;
        let document = self.document()?;
        let authority = if let Some((target, authority)) = authority {
            if self.clipboard_target() != Some(target) {
                return Err(Error::Cancelled);
            }
            authority.check()?;
            Some(authority)
        } else {
            None
        };
        let edit_range = TextRange::new(range.start, range.end)?;
        let (request, inserted) = match replacement {
            Replacement::Text(text) => {
                let inserted = text.len() as u64;
                (
                    Request::Replace {
                        document,
                        expected,
                        range: edit_range,
                        text,
                    },
                    inserted,
                )
            }
            Replacement::Newline | Replacement::Paste(_) => {
                let mut transaction = match self.call_authorized(
                    Request::BeginTransaction { document, expected },
                    authority.clone(),
                    true,
                )? {
                    Response::Transaction(transaction) => transaction,
                    _ => return Err(Error::CorruptStorage),
                };
                let cancel = authority.as_ref().unwrap_or(&self.cancel);
                let newline = transaction
                    .first_line_ending
                    .unwrap_or(self.default_line_ending);
                let inserted = match replacement {
                    Replacement::Paste(spool) => transaction.replace_counted(
                        edit_range,
                        spool.paste_reader(newline),
                        cancel,
                    )?,
                    Replacement::Newline => transaction.replace_counted(
                        edit_range,
                        newline.as_str().as_bytes(),
                        cancel,
                    )?,
                    Replacement::Text(_) => unreachable!(),
                };
                (
                    Request::Apply {
                        document,
                        transaction,
                    },
                    inserted,
                )
            }
        };
        let response = self.call_authorized(request, authority, false)?;
        if let Response::Changed {
            revision,
            history_pruned,
        } = response
        {
            // The deletion has committed even if a later layout/read fails.
            if let Some(clipboard) = self.cut_clipboard.take() {
                self.clipboard = Some(clipboard);
            }
            if revision != expected {
                self.cache.edited(expected, revision, range.start);
                self.tab.pending_edit = Some(CommittedEdit {
                    before,
                    endpoint: range.start + inserted,
                    history_pruned,
                });
                // An old cell map/selection must not describe the committed revision.
                self.tab.screen.view = None;
                self.begin();
                self.reconcile_edit()?;
            } else {
                self.refresh()?;
                if inserted != 0 {
                    // Storage kept the same bytes and history. The selection still
                    // ends at its original, valid grapheme boundary after replacement.
                    self.tab.screen.position.move_to(range.end, false);
                }
            }
            self.tab.screen.message = if history_pruned > 0 {
                format!("History pruned: {history_pruned} undo entries")
            } else {
                "Ready".into()
            };
            self.tab.preferred = None;
            self.layout(true)?;
        }
        Ok(())
    }
    /// Resolve committed caret/history exactly once before another position-dependent action.
    /// The pending record survives a read failure; retrying never repeats the transaction.
    fn reconcile_edit(&mut self) -> Result<(), Error> {
        let Some(edit) = &self.tab.pending_edit else {
            return Ok(());
        };
        let endpoint = edit.endpoint;
        self.refresh()?;
        let snapshot = self.snapshot()?;
        if let Ok(mut active) = self.view_cancel.lock() {
            *active = self.cancel.clone();
        }
        let caret = Text::new(&snapshot, &self.cancel).ceil(endpoint)?;
        let edit = self.tab.pending_edit.take().expect("committed edit");
        self.tab.screen.position.move_to(caret, false);
        self.tab.redo_views.clear();
        self.tab
            .undo_views
            .push_back((edit.before, self.tab.screen.position));
        for _ in 0..edit.history_pruned.min(self.tab.undo_views.len() as u64) {
            self.tab.undo_views.pop_front();
        }
        self.tab.preferred = None;
        Ok(())
    }
    /// Navigate using the same chunked cell mapping as rendering and pointer hits.
    fn movement(&mut self, movement: Movement, extend: bool) -> Result<(), Error> {
        let cancel = self
            .cancel
            .for_epoch(self.view_cancel.navigation.clone(), self.navigation_epoch);
        if let Ok(mut active) = self.view_cancel.lock() {
            *active = cancel.clone();
        }
        let snapshot = self.snapshot()?;
        let mut text = Text::new(&snapshot, &cancel);
        self.cache.reset(snapshot.revision());
        let position = self.tab.screen.position;
        let range = position.selection();
        let line = snapshot.line_of_offset(ByteOffset(position.caret))?;
        let vertical = matches!(
            movement,
            Movement::Up | Movement::Down | Movement::PageUp | Movement::PageDown
        );
        let preferred = if vertical {
            match self.tab.preferred {
                Some(column) => column,
                None => text.coordinates(position.caret, &mut self.cache)?.1,
            }
        } else {
            0
        };
        let page = self.height.saturating_sub(1).max(1) as u64;
        let caret = match movement {
            Movement::Left if !extend && !range.is_empty() => range.start,
            Movement::Right if !extend && !range.is_empty() => range.end,
            Movement::Left => text.previous(position.caret)?,
            Movement::Right => text.next(position.caret)?,
            Movement::Home => text.line_bounds(line)?.0,
            Movement::End => text.line_bounds(line)?.1,
            Movement::Start => 0,
            Movement::Finish => snapshot.len(),
            Movement::Up | Movement::PageUp => text.at_column(
                line.saturating_sub(if movement == Movement::Up { 1 } else { page }),
                preferred,
                &mut self.cache,
            )?,
            Movement::Down | Movement::PageDown => text.at_column(
                line.saturating_add(if movement == Movement::Down { 1 } else { page })
                    .min(snapshot.line_count() - 1),
                preferred,
                &mut self.cache,
            )?,
        };
        self.tab.screen.position.move_to(caret, extend);
        if self.tab.screen.position != position || movement == Movement::Finish {
            self.tab.pause_follow();
        }
        self.tab.preferred = vertical.then_some(preferred);
        if movement == Movement::PageUp {
            self.tab.top = self.tab.top.saturating_sub(page);
        }
        if movement == Movement::PageDown {
            self.tab.top = self
                .tab
                .top
                .saturating_add(page)
                .min(snapshot.line_count() - 1);
        }
        drop(snapshot);
        self.layout(true)
    }
    /// Handle one ordered user intention, leaving state intact on a failed edit.
    fn action(&mut self, action: Action) -> Result<(), Error> {
        if let Action::ApplySettings(values, generation) = action {
            self.settings_generation = generation;
            self.default_line_ending = values.default_line_ending();
            if let Some(retention) = &mut self.retention {
                retention.set_interval(Instant::now(), values.checkpoint());
            }
            if self.syntax.set_limits(values.syntax()) {
                self.tab.screen.syntax_projection = None;
                self.tab.screen.syntax_status = syntax::Status::Preparing;
            }
            return Ok(());
        }
        if let Some(export) = &mut self.export {
            match &action {
                Action::Guard(after) | Action::Discard(after, _)
                    if after.target().is_none_or(|tab| tab == export.tab) =>
                {
                    export.defer(action, self.active_sequence);
                    return Ok(());
                }
                Action::CancelGuard => export.cancel_deferred(),
                _ => {}
            }
        }
        if self.tab.capability == crate::search::Capability::ReadOnly && search::mutates(&action) {
            return Err(Error::ReadOnly);
        }
        if self.search.defer_lifecycle(&action, self.active_sequence) {
            return Ok(());
        }
        if search::clears_anchor(&action) {
            self.search.status.current = None;
            self.search.clear_stream_anchor();
        }
        if matches!(
            action,
            Action::Insert(_)
                | Action::Paste(_)
                | Action::Clipboard { .. }
                | Action::Command(_)
                | Action::NativeCopy { cut: true, .. }
                | Action::NativePaste { .. }
        ) && self
            .tab
            .scratch
            .as_ref()
            .is_some_and(|scratch| scratch.retiring)
        {
            self.cancel_retained_close()?;
        }
        // Lifecycle guards and saving can still protect/serialize work when its
        // caret cannot currently be read. No new position-dependent edit can run.
        if !matches!(
            action,
            Action::Guard(_)
                | Action::Discard(..)
                | Action::Save(_)
                | Action::SaveGuard(..)
                | Action::New
                | Action::Open(_)
                | Action::FieldPaste(..)
                | Action::ForTab(..)
                | Action::Switch(_)
                | Action::CycleTab(_)
                | Action::RemoveTab(_)
                | Action::CancelOpen(_)
                | Action::CancelActiveOpen(_)
                | Action::CancelGuard
                | Action::FinishQuit
                | Action::RetryRefresh(_)
                | Action::InspectLineEnding(_)
                | Action::SetLanguage(..)
                | Action::RetryHighlighting(..)
                | Action::SyntaxTheme(..)
                | Action::ReloadHighlighting
                | Action::RecoveryPage { .. }
                | Action::OpenRetained { .. }
                | Action::DiscardRetained { .. }
                | Action::Size { .. }
                | Action::SearchPage(..)
        ) {
            self.reconcile_edit()?;
        }
        match action {
            Action::Search(request) => self.start_search(request)?,
            Action::SearchPage(request) => self.search.page(request),
            Action::CancelSearch | Action::InvalidateSearch => self
                .search
                .cancel(matches!(action, Action::InvalidateSearch)),
            Action::InvalidateReplacement => {
                if self.search.status.replacing {
                    self.search.cancel(false);
                }
            }
            Action::RevokeSearch => {}
            Action::RetryRefresh(tab) => self.retry_refresh(tab)?,
            Action::InspectLineEnding(tab) => self.inspect_line_ending(tab),
            Action::SetLanguage(tab, choice) => {
                let config = self.syntax.config();
                if let Some(state) = self.state_mut(tab) {
                    state.screen.syntax_language = choice;
                    state.resolve_syntax(&config);
                }
            }
            Action::RetryHighlighting(tab) => {
                if let Some(state) = self.state_mut(tab) {
                    state.syntax_attempt = state.syntax_attempt.wrapping_add(1);
                    state.syntax_failure = None;
                }
            }
            Action::SyntaxTheme(name) => self.syntax.theme(name),
            Action::ReloadHighlighting => self.syntax.reload(),
            Action::ApplySettings(..) => unreachable!("settings are handled before document work"),
            Action::RecoveryPage {
                generation,
                cursor,
                previous,
            } => {
                if let Some(retention) = &mut self.retention {
                    retention.request_page(generation, cursor, previous);
                }
            }
            Action::OpenRetained {
                generation,
                scratch,
            } => self.open_retained(generation, scratch)?,
            Action::DiscardRetained {
                generation,
                scratch,
            } => self.discard_retained(generation, scratch)?,
            Action::FieldPaste(paste, generation) => {
                // Field length is independently bounded; document paste has no size cutoff.
                let result = (|| -> Result<String, Error> {
                    use std::io::Read;
                    if self.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    if paste.len > 4096 {
                        return Err(Error::ResourceLimit("path field length (4096 bytes)"));
                    }
                    let mut text = String::new();
                    paste.reader().read_to_string(&mut text)?;
                    if self.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    if text.contains('\0') {
                        return Err(Error::InvalidRange);
                    }
                    Ok(text)
                })();
                self.event(Event::FieldPaste(
                    generation,
                    result.map_err(|error| error.to_string()),
                ));
            }
            Action::New | Action::Open(_) if self.cancel.is_cancelled() => {
                return Err(Error::Cancelled);
            }
            Action::New => self.add_new()?,
            Action::Open(path) => {
                self.add_named(&path, true)?;
                if self.tab.screen.loading
                    && let Some(tab) = self.tab.screen.tab
                {
                    self.active_open_admission = Some((tab, self.active_sequence));
                }
            }
            Action::Follow(path) => self.add_stream(crate::stream::Source::Follow(path), true)?,
            Action::RetryFinish(tab) => self.retry_stream_finish(tab)?,
            Action::ExportRetained(tab, path) => self.export_retained(tab, path)?,
            Action::ToggleFollow(tab) => {
                if let Some(state) = self.state_mut(tab)
                    && let Some(status) = &mut state.screen.stream
                {
                    status.following = !status.following;
                    if status.following {
                        status.unseen = false;
                        state
                            .screen
                            .position
                            .move_to(state.info.as_ref().map_or(0, |info| info.len), false);
                    }
                    state.describe_stream();
                    if self.tab.screen.tab == Some(tab) && self.tab.screen.document.is_some() {
                        self.layout(false)?;
                    }
                }
            }
            Action::Switch(tab) => self.switch_tab(tab)?,
            Action::CycleTab(backwards) => self.cycle_tab(backwards)?,
            Action::RemoveTab(tab) => self.remove_tab(tab)?,
            Action::CancelOpen(tab) => self.cancel_open(tab)?,
            Action::CancelActiveOpen(switch) => self.cancel_active_open(switch)?,
            Action::CancelGuard => {
                self.quit_ready = false;
                self.stream_guard = None;
                self.waiting_guard = None;
                self.cancel_retained_close()?;
            }
            Action::FinishQuit => {
                if self.retention.is_some() {
                    self.cancel_retained_close()?;
                    self.begin_retained_quit(self.active_sequence);
                } else {
                    self.quit_ready = true;
                    self.event(Event::QuitReady(self.active_sequence));
                }
            }
            Action::Save(path) => {
                if let Some(tab) = self.tab.screen.tab {
                    self.queue_save(tab, path)?;
                }
            }
            Action::Guard(after) => self.guard(after)?,
            Action::SaveGuard(after, path) => self.save_for_guard(after, path)?,
            Action::Discard(after, acknowledged) => self.discard(after, acknowledged),
            Action::ForTab(tab, action) => self.tab_action(tab, *action)?,
            Action::Insert(text) => self.replace(
                Replacement::Text(text),
                self.tab.screen.position.selection(),
            )?,
            Action::Paste(spool) => self.replace(
                Replacement::Paste(&spool),
                self.tab.screen.position.selection(),
            )?,
            Action::Size {
                width,
                height,
                generation,
                search_cover,
            } => {
                self.width = width;
                self.height = height;
                self.generation = generation;
                self.search_cover = search_cover;
                if self.tab.screen.document.is_some() {
                    let found = self.search.status.current.as_ref().filter(|found| {
                        search_cover != 0
                            && self.tab.screen.tab == Some(found.tab)
                            && self
                                .tab
                                .info
                                .as_ref()
                                .is_some_and(|info| info.revision == found.revision)
                    });
                    if let Some(found) = found {
                        self.layout_search(found.range.end.saturating_sub(1))?;
                    } else {
                        self.layout(false)?;
                    }
                }
            }
            Action::Scroll(vertical, horizontal) => {
                let last_line = self
                    .tab
                    .info
                    .as_ref()
                    .ok_or(Error::Busy)?
                    .lines
                    .saturating_sub(1);
                // Ordered bounds are independent of cancelled/superseded paint jobs.
                // Clamp the old offset too: an edit may have shortened the document.
                let top = self
                    .tab
                    .top
                    .min(last_line)
                    .saturating_add_signed(vertical)
                    .min(last_line);
                let left = if self.tab.left != 0 || horizontal != 0 {
                    let snapshot = self.snapshot()?;
                    let cancel = self
                        .cancel
                        .for_epoch(self.view_cancel.navigation.clone(), self.navigation_epoch);
                    let mut text = Text::new(&snapshot, &cancel);
                    let old = text.clamp_left(top, self.height, self.tab.left, &mut self.cache)?;
                    text.clamp_left(
                        top,
                        self.height,
                        old.saturating_add_signed(horizontal),
                        &mut self.cache,
                    )?
                } else {
                    0
                };
                if top < self.tab.top || left < self.tab.left {
                    self.tab.pause_follow();
                }
                self.tab.top = top;
                self.tab.left = left;
                self.layout(false)?;
            }
            Action::GoTo { line, column } => {
                let cancel = self
                    .cancel
                    .for_epoch(self.view_cancel.navigation.clone(), self.navigation_epoch);
                if let Ok(mut active) = self.view_cancel.lock() {
                    *active = cancel.clone();
                }
                let snapshot = self.snapshot()?;
                if line >= snapshot.line_count() {
                    return Err(Error::InvalidRange);
                }
                let caret =
                    Text::new(&snapshot, &cancel).at_column(line, column, &mut self.cache)?;
                if caret != self.tab.screen.position.caret {
                    self.tab.pause_follow();
                }
                self.tab.screen.position.move_to(caret, false);
                self.tab.preferred = None;
                self.layout(true)?;
            }
            Action::ClearSelection => {
                if self.tab.screen.document.is_some() {
                    self.tab.screen.position.anchor = self.tab.screen.position.caret;
                    self.layout(false)?;
                }
            }
            Action::Hit {
                caret,
                extend,
                generation,
                document,
                revision,
                top,
                left,
                gesture,
            } => {
                if matches!(gesture, PointerGesture::Drag(id) if self.tab.screen.pointer_press != Some(id))
                {
                    return Ok(());
                }
                if self.tab.screen.document == Some(document)
                    && generation == self.generation
                    && top == self.tab.top
                    && left == self.tab.left
                    && self
                        .tab
                        .info
                        .as_ref()
                        .is_some_and(|info| info.revision == revision)
                {
                    if let PointerGesture::Press(id) = gesture {
                        self.tab.screen.pointer_press = Some(id);
                    }
                    self.tab.pause_follow();
                    self.tab.screen.position.move_to(caret, extend);
                    self.tab.preferred = None;
                    self.layout(false)?;
                } else {
                    self.tab.screen.message = "Selection ignored: view changed; retry".into();
                }
            }
            Action::Clipboard { cut, system } => self.copy(cut, system)?,
            Action::NativeCopy { cut, access } => self.native_copy(cut, access),
            Action::NativePaste { access, observed } => self.native_paste(access, observed),
            Action::Command(command) => match command {
                Command::Move(movement, extend) => self.movement(movement, extend)?,
                Command::SelectAll => {
                    let snapshot = self.snapshot()?;
                    self.tab.pause_follow();
                    self.tab.screen.position = Position {
                        anchor: 0,
                        caret: snapshot.len(),
                    };
                    self.tab.preferred = None;
                    self.layout(true)?;
                }
                Command::Copy => self.copy(false, None)?,
                Command::Cut => self.copy(true, None)?,
                Command::Paste => {
                    if let Some(paste) = self.clipboard.clone() {
                        self.replace(
                            Replacement::Paste(&paste),
                            self.tab.screen.position.selection(),
                        )?;
                    } else {
                        self.tab.screen.message = "Nothing copied in ede".into();
                    }
                }
                Command::Newline | Command::Tab => {
                    let replacement = if command == Command::Tab {
                        Replacement::Text("\t".into())
                    } else {
                        Replacement::Newline
                    };
                    self.replace(replacement, self.tab.screen.position.selection())?;
                }
                Command::Backspace | Command::Delete => {
                    let mut range = self.tab.screen.position.selection();
                    if range.is_empty() {
                        let snapshot = self.snapshot()?;
                        let mut text = Text::new(&snapshot, &self.cancel);
                        if command == Command::Backspace {
                            range.start = text.previous(range.start)?;
                        } else {
                            range.end = text.next(range.end)?;
                        }
                    }
                    self.replace(Replacement::Text(String::new()), range)?;
                }
                Command::Undo | Command::Redo => {
                    let expected = self.tab.info.as_ref().ok_or(Error::Busy)?.revision;
                    let document = self.document()?;
                    let response = self.call(if command == Command::Undo {
                        Request::Undo { document, expected }
                    } else {
                        Request::Redo { document, expected }
                    })?;
                    if matches!(response, Response::Changed {revision, ..} if revision != expected)
                    {
                        self.tab.screen.view = None;
                        if command == Command::Undo {
                            if let Some(entry) = self.tab.undo_views.pop_back() {
                                self.tab.screen.position = entry.0;
                                self.tab.redo_views.push(entry);
                            }
                        } else if let Some(entry) = self.tab.redo_views.pop() {
                            self.tab.screen.position = entry.1;
                            self.tab.undo_views.push_back(entry);
                        }
                    }
                    self.refresh()?;
                    self.tab.preferred = None;
                    self.layout(true)?;
                }
                _ => {}
            },
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    /// Worker-side fixture uses the real service without a terminal or shared user files.
    pub(super) fn coordinator(options: OpenOptions) -> Coordinator {
        crate::provide_descriptor_headroom();
        let (_, inbox) = mpsc::sync_channel(256);
        let (events, _) = mpsc::sync_channel(16);
        let mut worker = Coordinator::new(
            StorageService::new(options).unwrap(),
            inbox,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.add_new().unwrap();
        worker
    }
    /// Read small test contents with the same bounded snapshot interface as production.
    pub(super) fn contents(worker: &mut Coordinator) -> String {
        let snapshot = worker.snapshot().unwrap();
        let mut result = String::new();
        let mut offset = 0;
        while offset < snapshot.len() {
            let window = snapshot.read_window(ByteOffset(offset), 64 << 10).unwrap();
            result.push_str(window.text());
            offset += window.text().len() as u64;
        }
        result
    }

    /// A new internal copy cannot commit between authorization and the first tty byte.
    #[test]
    fn clipboard_first_write_serializes_authority_until_callback_returns() {
        let authority = Arc::new(Mutex::new(1));
        let transfer = ClipboardTransfer {
            epoch: 0,
            cut: false,
            sequence: 1,
            frame: b"first frame".to_vec(),
            generation: 1,
            authority: authority.clone(),
        };
        let (entered, reached) = mpsc::sync_channel(1);
        let (release, held) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            transfer.write_first(|bytes| {
                entered.send(()).unwrap();
                held.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(bytes.len())
            })
        });
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        let protected = matches!(
            authority.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        release.send(()).unwrap();
        assert_eq!(writer.join().unwrap().unwrap(), b"first frame".len());
        assert!(
            protected,
            "a newer copy could commit before the first byte was written"
        );
        *authority.lock().unwrap() = 2;
    }
    /// Pump an independent save to its acknowledged completion, with a finite deadline.
    fn saved(worker: &mut Coordinator) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.saving.is_some() || worker.completed_save.is_some() {
            worker.receive().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// A close's Save decision must survive acknowledgement of an older revision.
    #[test]
    fn guarded_save_waits_for_newer_queued_revision() {
        use crate::{
            app::{AppState, Intent, Modal},
            input::Input,
            ui::LayoutSnapshot,
        };
        use ratatui::layout::Rect;
        let gate = Arc::new(SaveGate {
            fail: false,
            ..SaveGate::new(document::IoPoint::Publish, 1)
        });
        let mut worker = coordinator(OpenOptions {
            faults: Some(gate.clone()),
            ..OpenOptions::default()
        });
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("named");
        std::fs::write(&file, "initial").unwrap();
        worker.open_and_switch(&file).unwrap();
        let id = worker.tab.screen.tab.unwrap();
        let after = After::Close(id);
        let (sender, events) = mpsc::sync_channel(16);
        worker.events = sender;
        worker.action(Action::Insert("one ".into())).unwrap();
        worker.action(Action::Save(None)).unwrap();
        gate.wait_until_blocked();
        worker.action(Action::Insert("two ".into())).unwrap();
        worker.active_sequence = 5;
        worker.action(Action::Guard(after)).unwrap();
        let mut app = AppState::new();
        app.update(worker.tab.screen.clone(), false);
        app.expect_guard(5);
        app.event(events.try_recv().unwrap());
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        app.input(Input::Command(Command::Tab), &layout);
        let Some(Intent::Editor(save)) = app.input(Input::Command(Command::Newline), &layout)
        else {
            panic!("expected guarded save");
        };
        worker.active_sequence = 6;
        app.expect_guard_for(6, after);
        worker.action(save).unwrap();
        gate.release();
        saved(&mut worker);
        let mut removed = None;
        for event in events.try_iter() {
            match app.event(event) {
                Some(Intent::Editor(Action::RemoveTab(id))) => removed = Some(id),
                Some(Intent::Editor(Action::Guard(_))) => {
                    panic!("older save consumed the close decision")
                }
                None => {}
                Some(intent) => panic!("unexpected {intent:?}"),
            }
        }
        assert_eq!(removed, Some(id));
        assert!(!matches!(app.modal, Some(Modal::Confirm(..))));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "one two initial");
    }
    /// Selection replacement, grapheme deletion and view history restore exact caret/anchor pairs.
    #[test]
    fn editing_selection_undo_and_redo() {
        let mut worker = coordinator(OpenOptions::default());
        worker
            .action(Action::Insert("a界e\u{301}\r\nlast".into()))
            .unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Right, false)))
            .unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Right, true)))
            .unwrap();
        let selection = worker.tab.screen.position;
        assert_eq!(selection.selection(), 1..4);
        worker.action(Action::Insert("X".into())).unwrap();
        assert_eq!(contents(&mut worker), "aXe\u{301}\r\nlast");
        let after = worker.tab.screen.position;
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(worker.tab.screen.position, selection);
        worker.action(Action::Command(Command::Redo)).unwrap();
        assert_eq!(worker.tab.screen.position, after);
        worker
            .action(Action::Command(Command::Move(Movement::End, false)))
            .unwrap();
        worker.action(Action::Command(Command::Delete)).unwrap();
        assert_eq!(contents(&mut worker), "aXe\u{301}last");
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(contents(&mut worker), "aXe\u{301}\r\nlast");
    }
    /// No-ops and failed text keep redo and clipboard; history view stacks follow storage pruning.
    #[test]
    fn clipboard_failure_noops_and_pruned_history() {
        let mut options = OpenOptions::default();
        options.limits.undo_transactions = 2;
        let mut worker = coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Command(Command::Copy)).unwrap();
        worker.action(Action::Insert("two".into())).unwrap();
        worker.action(Action::Insert("three".into())).unwrap();
        assert_eq!(worker.tab.undo_views.len(), 2);
        worker.action(Action::Command(Command::Undo)).unwrap();
        let before = contents(&mut worker);
        let position = worker.tab.screen.position;
        assert!(worker.action(Action::Insert("\0".into())).is_err());
        assert_eq!(worker.tab.screen.position, position);
        assert_eq!(contents(&mut worker), before);
        worker.action(Action::Insert(String::new())).unwrap();
        worker.action(Action::Command(Command::Redo)).unwrap();
        assert_eq!(contents(&mut worker), "twothree");
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Command(Command::Cut)).unwrap();
        assert_eq!(contents(&mut worker), "");
        worker.action(Action::Command(Command::Paste)).unwrap();
        assert_eq!(contents(&mut worker), "twothree");
    }

    /// A matching replacement updates only the view and keeps the existing redo transaction.
    #[test]
    fn identical_replacement_preserves_redo() {
        let mut worker = coordinator(OpenOptions::default());
        worker.action(Action::Insert("a".into())).unwrap();
        worker.action(Action::Insert("b".into())).unwrap();
        worker.action(Action::Command(Command::Undo)).unwrap();
        let revision = worker.tab.info.as_ref().unwrap().revision;
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Insert("a".into())).unwrap();
        assert_eq!(
            worker.tab.screen.position,
            Position {
                caret: 1,
                anchor: 1
            }
        );
        assert_eq!(worker.tab.info.as_ref().unwrap().revision, revision);
        worker.action(Action::Command(Command::Redo)).unwrap();
        assert_eq!(contents(&mut worker), "ab");
    }

    /// A rejected press cannot lend its stale anchor to motion on a fresh viewport.
    #[test]
    fn drag_requires_an_accepted_press() {
        let mut worker = coordinator(OpenOptions::default());
        worker
            .action(Action::Insert("row0\nrow1\nrow2\nrow3\nrow4".into()))
            .unwrap();
        worker.action(Action::GoTo { line: 1, column: 0 }).unwrap();
        let document = worker.document().unwrap();
        let revision = worker.tab.info.as_ref().unwrap().revision;
        let hit = |caret, top, gesture| Action::Hit {
            caret,
            extend: matches!(gesture, PointerGesture::Drag(_)),
            generation: 0,
            document,
            revision,
            top,
            left: 0,
            gesture,
        };
        worker.action(Action::Scroll(3, 0)).unwrap();
        worker.action(hit(0, 0, PointerGesture::Press(1))).unwrap();
        worker.action(hit(21, 3, PointerGesture::Drag(1))).unwrap();
        assert_eq!(
            worker.tab.screen.position,
            Position {
                caret: 5,
                anchor: 5
            }
        );
        worker.action(hit(15, 3, PointerGesture::Press(2))).unwrap();
        worker.action(hit(21, 3, PointerGesture::Drag(2))).unwrap();
        assert_eq!(worker.tab.screen.position.selection(), 15..21);
    }
    /// Internal paste uses the current fallback for a new document and keeps the copied bytes.
    #[test]
    fn internal_paste_normalizes_to_the_destination() {
        use std::io::Read;

        let mut worker = coordinator(OpenOptions::default());
        let copied = "one\r\ntwo\nthree\rfour";
        worker.action(Action::Insert(copied.into())).unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Command(Command::Copy)).unwrap();
        worker.add_new().unwrap();
        let mut settings = crate::settings::Values::default();
        settings.default_line_ending = Some(document::Newline::CrLf);
        worker.action(Action::ApplySettings(settings, 1)).unwrap();
        worker.action(Action::Command(Command::Paste)).unwrap();
        assert_eq!(contents(&mut worker), "one\r\ntwo\r\nthree\r\nfour");
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(contents(&mut worker), "");
        worker
            .action(Action::ApplySettings(crate::settings::Values::default(), 2))
            .unwrap();
        worker.action(Action::Command(Command::Paste)).unwrap();
        assert_eq!(contents(&mut worker), "one\ntwo\nthree\nfour");
        let mut retained = String::new();
        worker
            .clipboard
            .as_ref()
            .unwrap()
            .reader()
            .read_to_string(&mut retained)
            .unwrap();
        assert_eq!(retained, copied);
    }

    /// Internal and external disk-spooled pastes exceed 1 MiB, commit atomically, and undo once.
    #[test]
    fn large_paste_and_invalid_payload_are_atomic() {
        let mut worker = coordinator(OpenOptions::default());
        let mut paste = Spool::new().unwrap();
        let block = "界\r\n".repeat(16_385);
        for _ in 0..16 {
            paste.append(block.as_bytes()).unwrap();
        }
        let length = block.replace("\r\n", "\n").len() as u64 * 16;
        assert!(length > 1 << 20);
        worker.action(Action::Paste(Arc::new(paste))).unwrap();
        assert_eq!(worker.snapshot().unwrap().len(), length);
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(worker.snapshot().unwrap().len(), 0);
        worker.action(Action::Command(Command::Redo)).unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Command(Command::Copy)).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Finish, false)))
            .unwrap();
        worker.action(Action::Command(Command::Paste)).unwrap();
        assert_eq!(worker.snapshot().unwrap().len(), length * 2);
        let position = worker.tab.screen.position;
        let mut invalid = Spool::new().unwrap();
        invalid.append(b"valid\xffinvalid").unwrap();
        assert!(worker.action(Action::Paste(Arc::new(invalid))).is_err());
        assert_eq!(worker.tab.screen.position, position);
        assert_eq!(worker.snapshot().unwrap().len(), length * 2);
    }
    /// Failed replacements retain the current document; save/undo tracks the acknowledged content.
    #[test]
    fn save_reopen_and_failed_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document");
        let mut worker = coordinator(OpenOptions::default());
        worker.action(Action::Insert("hello".into())).unwrap();
        worker.action(Action::Save(Some(path.clone()))).unwrap();
        saved(&mut worker);
        assert!(!worker.tab.screen.modified);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        worker.action(Action::Insert("!".into())).unwrap();
        assert!(worker.tab.screen.modified);
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert!(!worker.tab.screen.modified);
        let old = worker.tab.screen.document;
        assert!(
            worker
                .open_and_switch(&directory.path().join("missing"))
                .is_err()
        );
        assert_eq!(worker.tab.screen.document, old);
        assert_eq!(contents(&mut worker), "hello");
        assert!(!worker.tab.screen.loading);
    }
    /// A pre-publication retry failure cannot acknowledge a prior uncertain destination.
    #[test]
    fn failed_retry_preserves_uncertain_discard_guard() {
        #[derive(Debug, Default)]
        struct Fault(AtomicBool);
        impl document::FaultInjector for Fault {
            fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
                if point == document::IoPoint::DirectorySync && self.0.swap(false, Ordering::AcqRel)
                {
                    Err(std::io::Error::other("directory sync failed"))
                } else {
                    Ok(())
                }
            }
        }
        let fault = Arc::new(Fault::default());
        let mut worker = coordinator(OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("destination");
        worker.action(Action::Insert("keep me".into())).unwrap();
        fault.0.store(true, Ordering::Release);
        worker.action(Action::Save(Some(path.clone()))).unwrap();
        saved(&mut worker);
        assert!(worker.tab.uncertain.is_some());
        worker.action(Action::Save(Some(path))).unwrap();
        saved(&mut worker);
        assert!(worker.tab.uncertain.is_some());
        assert!(worker.tab.screen.modified);
        let (sender, events) = mpsc::sync_channel(16);
        worker.events = sender;
        worker.active_sequence = 77;
        worker.action(Action::Discard(After::Quit, None)).unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Uncertain(77, After::Quit, job)) if Some(job) == worker.tab.uncertain
        ));
        worker
            .action(Action::Discard(After::Quit, worker.tab.uncertain))
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Proceed(77, After::Quit))
        ));
    }
    /// Block publication or directory sync until the test has issued cancellation.
    #[derive(Debug)]
    struct SaveGate {
        point: document::IoPoint,
        state: Mutex<(usize, bool)>,
        wake: std::sync::Condvar,
        block_on: usize,
        fail: bool,
    }

    impl SaveGate {
        fn new(point: document::IoPoint, block_on: usize) -> Self {
            Self {
                point,
                state: Mutex::new((0, false)),
                wake: std::sync::Condvar::new(),
                block_on,
                fail: true,
            }
        }

        fn wait_until_blocked(&self) {
            let (state, _) = self
                .wake
                .wait_timeout_while(
                    self.state.lock().unwrap(),
                    Duration::from_secs(5),
                    |state| state.0 < self.block_on,
                )
                .unwrap();
            assert_eq!(state.0, self.block_on);
        }

        fn release(&self) {
            self.state.lock().unwrap().1 = true;
            self.wake.notify_all();
        }

        fn release_after_cancellation(
            self: &Arc<Self>,
            cancellation: Cancellation,
        ) -> JoinHandle<()> {
            let gate = self.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancellation.is_cancelled() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
                gate.release();
                assert!(
                    cancellation.is_cancelled(),
                    "save cancellation was never requested"
                );
            })
        }
    }

    impl document::FaultInjector for SaveGate {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point != self.point {
                return Ok(());
            }
            let mut state = self.state.lock().unwrap();
            state.0 += 1;
            self.wake.notify_all();
            let (_state, wait) = self
                .wake
                .wait_timeout_while(state, Duration::from_secs(10), |state| {
                    state.0 == self.block_on && !state.1
                })
                .unwrap();
            assert!(!wait.timed_out(), "test never released the blocked save");
            if self.fail {
                Err(std::io::Error::other("injected save failure"))
            } else {
                Ok(())
            }
        }
    }

    /// The retry publishes after an older warning was shown and needs its own acknowledgement.
    #[test]
    fn retry_uncertainty_requires_a_fresh_discard_acknowledgement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("destination");
        std::fs::write(&path, b"first").unwrap();
        let gate = Arc::new(SaveGate::new(document::IoPoint::DirectorySync, 2));
        let mut worker = coordinator(OpenOptions {
            faults: Some(gate.clone()),
            ..OpenOptions::default()
        });
        let (sender, events) = mpsc::sync_channel(16);
        worker.events = sender;
        worker.open_and_switch(&path).unwrap();
        worker.action(Action::Insert("edit-".into())).unwrap();
        let first = directory.path().join("first-destination");
        worker.action(Action::Save(Some(first))).unwrap();
        saved(&mut worker);
        let first_job = worker.tab.uncertain.unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Saved(_, SaveResult::Uncertain, _))
        ));

        worker.action(Action::Save(None)).unwrap();
        gate.wait_until_blocked();
        assert_eq!(std::fs::read(&path).unwrap(), b"edit-first");
        let retry = &worker.saving.as_ref().unwrap().job;
        let retry_job = retry.id;
        let release = gate.release_after_cancellation(retry.cancellation());
        worker
            .action(Action::Discard(After::Quit, Some(first_job)))
            .unwrap();
        release.join().unwrap();
        saved(&mut worker);
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Saved(_, SaveResult::Uncertain, _))
        ));
        assert!(
            matches!(events.try_recv(), Ok(Event::Uncertain(_, After::Quit, job)) if job == retry_job)
        );
        worker
            .action(Action::Discard(After::Quit, Some(retry_job)))
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Proceed(_, After::Quit))
        ));
    }

    /// A real save error can arrive during Discard or just before the worker takes it.
    #[test]
    fn discard_preserves_a_new_precommit_failure() {
        use crate::{
            app::{AppState, Modal},
            ui::LayoutSnapshot,
        };
        use ratatui::layout::Rect;

        for failure_already_polled in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("destination");
            std::fs::write(&path, b"before").unwrap();
            let gate = Arc::new(SaveGate::new(document::IoPoint::Publish, 1));
            let mut worker = coordinator(OpenOptions {
                faults: Some(gate.clone()),
                ..OpenOptions::default()
            });
            let (sender, events) = mpsc::sync_channel(16);
            worker.events = sender;
            worker.open_and_switch(&path).unwrap();
            worker.action(Action::Insert("edit-".into())).unwrap();
            worker.action(Action::Save(None)).unwrap();
            gate.wait_until_blocked();

            let mut app = AppState::new();
            app.update(worker.tab.screen.clone(), false);
            worker.active_sequence = 5;
            app.expect_guard(5);
            worker
                .action(Action::Guard(After::Close(worker.tab.screen.tab.unwrap())))
                .unwrap();
            app.event(events.try_recv().unwrap());
            let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 0);
            let _ = app.input(crate::input::Input::Command(Command::Tab), &layout);
            let _ = app.input(crate::input::Input::Command(Command::Tab), &layout);
            let Some(crate::app::Intent::Editor(discard)) =
                app.input(crate::input::Input::Command(Command::Newline), &layout)
            else {
                panic!("expected Discard");
            };
            assert!(matches!(discard, Action::Discard(After::Close(_), None)));
            worker.active_sequence = 6;
            app.expect_guard(6);
            if failure_already_polled {
                gate.release();
                saved(&mut worker);
                worker.action(discard).unwrap();
            } else {
                let release = gate
                    .release_after_cancellation(worker.saving.as_ref().unwrap().job.cancellation());
                worker.action(discard).unwrap();
                release.join().unwrap();
                saved(&mut worker);
            }

            let failure = events.try_recv().unwrap();
            assert!(matches!(failure, Event::Saved(_, SaveResult::Failed, _)));
            assert!(matches!(
                app.event(failure),
                Some(crate::app::Intent::Editor(Action::CancelGuard))
            ));
            worker.action(Action::CancelGuard).unwrap();
            for event in events.try_iter() {
                assert!(
                    app.event(event).is_none(),
                    "failure must remain visible before quitting"
                );
            }
            assert!(
                matches!(app.modal, Some(Modal::SaveError(message)) if message.contains("injected save failure"))
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"before");
            assert_eq!(contents(&mut worker), "edit-before");
        }
    }

    /// An active save sees runtime stop immediately, even before the coordinator can shut down storage.
    #[test]
    fn stopping_editor_cancels_save_before_waiting_for_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("destination");
        std::fs::write(&path, b"before").unwrap();
        let gate = Arc::new(SaveGate {
            fail: false,
            ..SaveGate::new(document::IoPoint::Publish, 1)
        });
        let stop = Arc::new(AtomicBool::new(false));
        let service = StorageService::with_stop_flag(
            OpenOptions {
                faults: Some(gate.clone()),
                ..OpenOptions::default()
            },
            stop.clone(),
        )
        .unwrap();
        let (_, inbox) = mpsc::sync_channel(1);
        let (sender, _events) = mpsc::sync_channel(1);
        let mut worker = Coordinator::new(
            service,
            inbox,
            sender,
            Arc::new(Mutex::new(None)),
            stop.clone(),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.open_and_switch(&path).unwrap();
        worker.action(Action::Insert("edit-".into())).unwrap();
        worker.action(Action::Save(None)).unwrap();
        gate.wait_until_blocked();
        stop.store(true, Ordering::Release);
        let cancelled_before_release = worker
            .saving
            .as_ref()
            .unwrap()
            .job
            .cancellation()
            .is_cancelled();
        gate.release();
        assert!(cancelled_before_release);
        let deadline = Instant::now() + Duration::from_secs(5);
        let completion = loop {
            if let Some(completion) = worker.service.try_recv().unwrap() {
                break completion;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        assert!(matches!(
            completion.result,
            Ok(Response::Saved(document::SaveReport {
                outcome: SaveOutcome::NotCommitted,
                error: Some(Error::Cancelled),
                ..
            }))
        ));
        worker.service.shutdown().wait().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"before");
    }

    /// Even failure to create a fallback buffer leaves a safe keyboard quit route.
    #[test]
    fn guard_can_quit_without_a_ready_document() {
        let mut worker = coordinator(OpenOptions::default());
        worker
            .service
            .close(worker.tab.screen.document.take().unwrap());
        worker.tab.info = None;
        let (sender, events) = mpsc::sync_channel(16);
        worker.events = sender;
        worker.active_sequence = 3;
        worker.action(Action::Guard(After::Quit)).unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Guard(3, After::Quit, false))
        ));
    }
    /// Reversing at EOF must move up even when preceding layouts were skipped.
    #[test]
    fn vertical_scroll_clamps_before_superseded_layout() {
        let mut worker = coordinator(OpenOptions::default());
        worker.action(Action::Insert("a\nb\nc".into())).unwrap();
        worker.action(Action::Scroll(100, 0)).unwrap();
        assert_eq!(worker.tab.top, 2);
        worker.view_cancel.layout.fetch_add(1, Ordering::AcqRel);
        worker.action(Action::Scroll(3, 0)).unwrap();
        worker.action(Action::Scroll(-3, 0)).unwrap();
        assert_eq!(worker.tab.top, 0);
        assert!(!scrolls_combine(3, 0, -3, 0));
        assert!(!scrolls_combine(0, 4, 0, -4));
        assert!(scrolls_combine(3, 0, 3, 0));
        assert!(scrolls_combine(0, -4, 0, -4));
        // The same ordering must hold horizontally even when no paint job ran.
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Insert("abcde".into())).unwrap();
        worker.action(Action::Scroll(0, 4)).unwrap();
        worker.action(Action::Scroll(0, 4)).unwrap();
        worker.action(Action::Scroll(0, -4)).unwrap();
        assert_eq!(worker.tab.left, 1);
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker
            .action(Action::Insert("a\nb\nc\nd\ne".into()))
            .unwrap();
        worker.action(Action::Scroll(100, 0)).unwrap();
        assert_eq!(worker.tab.top, 4);
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Insert("a\nb\nc".into())).unwrap();
        worker.action(Action::Scroll(-3, 0)).unwrap();
        assert_eq!(worker.tab.top, 0);
    }
    /// A read failure after commit blocks later edits until caret/history reconciliation succeeds.
    #[test]
    fn committed_edit_recovers_history_after_read_failure() {
        #[derive(Debug, Default)]
        struct Fault(AtomicBool);
        impl document::FaultInjector for Fault {
            fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
                if point == document::IoPoint::Read && self.0.load(Ordering::Acquire) {
                    Err(std::io::Error::other("caret read failed"))
                } else {
                    Ok(())
                }
            }
        }
        let fault = Arc::new(Fault::default());
        let mut worker = coordinator(OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        });
        worker.action(Action::Insert("ab".into())).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        let before = worker.tab.screen.position;
        // Inject at the acknowledged-commit boundary, independently of storage's
        // own pre-commit read checks. The following caret lookup must be retriable.
        let response = worker
            .call(Request::Replace {
                document: worker.document().unwrap(),
                expected: worker.tab.info.as_ref().unwrap().revision,
                range: TextRange::new(0, 0).unwrap(),
                text: "X".into(),
            })
            .unwrap();
        assert!(matches!(response, Response::Changed { .. }));
        worker.tab.screen.view = None;
        worker.tab.pending_edit = Some(CommittedEdit {
            before,
            endpoint: 1,
            history_pruned: 0,
        });
        fault.0.store(true, Ordering::Release);
        assert!(
            worker
                .action(Action::Insert("must not commit".into()))
                .is_err()
        );
        assert!(worker.tab.pending_edit.is_some());
        assert_eq!(worker.tab.undo_views.len(), 1);
        let mut settings = crate::settings::Values::default();
        settings.set(crate::settings::Scalar::Timeout, Some(20));
        worker.action(Action::ApplySettings(settings, 42)).unwrap();
        assert_eq!(worker.settings_generation, 42);
        assert!(
            worker.tab.pending_edit.is_some(),
            "settings must not reconcile the caret"
        );
        fault.0.store(false, Ordering::Release);
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(contents(&mut worker), "ab");
        assert_eq!(worker.tab.screen.position, before);
        assert!(worker.tab.pending_edit.is_none());
        worker.action(Action::Command(Command::Redo)).unwrap();
        assert_eq!(contents(&mut worker), "Xab");
        assert_eq!(worker.tab.screen.position.caret, 1);
        assert_eq!(worker.tab.undo_views.len(), 2);
    }
    /// Document-start movement does not scan a long offscreen prefix just to compute its old column.
    #[test]
    fn document_start_does_not_scan_the_old_long_line() {
        #[derive(Debug, Default)]
        struct Reads {
            count: AtomicUsize,
            limited: AtomicBool,
        }
        impl document::FaultInjector for Reads {
            fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
                if point == document::IoPoint::Read
                    && self.limited.load(Ordering::Acquire)
                    && self.count.fetch_add(1, Ordering::AcqRel) >= 4
                {
                    return Err(std::io::Error::other("unexpected long prefix scan"));
                }
                Ok(())
            }
        }
        let fault = Arc::new(Reads::default());
        let mut worker = coordinator(OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        });
        worker.action(Action::Insert("x".repeat(2 << 20))).unwrap();
        worker.cache = Checkpoints::default();
        fault.limited.store(true, Ordering::Release);
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        assert_eq!(worker.tab.screen.position.caret, 0);
        assert!(fault.count.load(Ordering::Acquire) <= 4);
    }

    /// Compare mixed Unicode editing/navigation/history against an independent contiguous model.
    #[test]
    fn mixed_edit_sequences_match_contiguous_model() {
        use unicode_segmentation::UnicodeSegmentation;
        let mut worker = coordinator(OpenOptions::default());
        let mut text = String::new();
        let mut position = Position::default();
        let mut undo = Vec::new();
        let mut redo = Vec::new();
        let pieces = [
            "a", "界", "\r", "\n", "e", "\u{301}", "🇺", "🇸", "\u{200d}", "👩", "\t",
        ];
        let mut seed = 0x537a_93b8_u64;
        for step in 0..300 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte as u64)
                .chain(std::iter::once(text.len() as u64))
                .collect();
            let kind = (seed >> 32) % 16;
            let action = match kind {
                0..=7 => Action::Insert(pieces[(seed as usize) % pieces.len()].into()),
                8 => Action::Command(Command::Backspace),
                9 => Action::Command(Command::Delete),
                10..=12 => Action::Command(Command::Move(
                    if kind == 10 {
                        Movement::Left
                    } else {
                        Movement::Right
                    },
                    seed & 1 != 0,
                )),
                13 => Action::Command(Command::SelectAll),
                14 => Action::Command(Command::Undo),
                _ => Action::Command(Command::Redo),
            };
            match &action {
                Action::Insert(_) | Action::Command(Command::Backspace | Command::Delete) => {
                    let before = (text.clone(), position);
                    let mut range = position.selection();
                    let inserted = if let Action::Insert(value) = &action {
                        value.as_str()
                    } else {
                        if range.is_empty() {
                            if matches!(action, Action::Command(Command::Backspace)) {
                                range.start = boundaries
                                    .iter()
                                    .copied()
                                    .rfind(|b| *b < range.start)
                                    .unwrap_or(0);
                            } else {
                                range.end = boundaries
                                    .iter()
                                    .copied()
                                    .find(|b| *b > range.end)
                                    .unwrap_or(text.len() as u64);
                            }
                        }
                        ""
                    };
                    text.replace_range(range.start as usize..range.end as usize, inserted);
                    if text != before.0 {
                        let endpoint = range.start + inserted.len() as u64;
                        let caret = text
                            .grapheme_indices(true)
                            .map(|(b, _)| b as u64)
                            .chain(std::iter::once(text.len() as u64))
                            .find(|b| *b >= endpoint)
                            .unwrap();
                        position.move_to(caret, false);
                        undo.push((before, (text.clone(), position)));
                        redo.clear();
                    }
                }
                Action::Command(Command::Move(direction, extend)) => {
                    let range = position.selection();
                    let caret = if !extend && !range.is_empty() {
                        if *direction == Movement::Left {
                            range.start
                        } else {
                            range.end
                        }
                    } else if *direction == Movement::Left {
                        boundaries
                            .iter()
                            .copied()
                            .rfind(|b| *b < position.caret)
                            .unwrap_or(0)
                    } else {
                        boundaries
                            .iter()
                            .copied()
                            .find(|b| *b > position.caret)
                            .unwrap_or(text.len() as u64)
                    };
                    position.move_to(caret, *extend);
                }
                Action::Command(Command::SelectAll) => {
                    position = Position {
                        anchor: 0,
                        caret: text.len() as u64,
                    }
                }
                Action::Command(Command::Undo) => {
                    if let Some(entry) = undo.pop() {
                        (text, position) = entry.0.clone();
                        redo.push(entry);
                    }
                }
                Action::Command(Command::Redo) => {
                    if let Some(entry) = redo.pop() {
                        (text, position) = entry.1.clone();
                        undo.push(entry);
                    }
                }
                _ => unreachable!(),
            }
            worker.action(action).unwrap();
            assert_eq!(contents(&mut worker), text, "step {step}");
            assert_eq!(worker.tab.screen.position, position, "step {step}");
        }
    }
    /// Measure managed resources and UI incorporation separately from terminal transport.
    #[test]
    #[ignore = "resource benchmark: run explicitly with EDE_RESOURCE_MIB"]
    fn interactive_resource_and_incorporation_measurements() {
        use ratatui::{Terminal, backend::TestBackend, layout::Rect};
        use std::{fs::File, io::Write, os::unix::fs::MetadataExt};
        /// Sum filesystem-reported allocated blocks, including indexes/history and bitmap files.
        fn blocks(path: &std::path::Path) -> u64 {
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let metadata = entry.metadata().unwrap();
                    if metadata.is_dir() {
                        blocks(&entry.path())
                    } else {
                        metadata.blocks() * 512
                    }
                })
                .sum()
        }
        let mib: u64 = std::env::var("EDE_RESOURCE_MIB")
            .unwrap_or_else(|_| "64".into())
            .parse()
            .unwrap();
        let selected = std::env::var("EDE_BENCH_CORPUS").ok();
        for corpus in ["ordinary", "dense", "long", "unicode"] {
            if selected.as_deref().is_some_and(|name| name != corpus) {
                continue;
            }
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input");
            let working = directory.path().join("working");
            std::fs::create_dir(&working).unwrap();
            let pattern = match corpus {
                "ordinary" => {
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n"
                        .to_owned()
                }
                "dense" => "x\n".into(),
                "long" => "x".into(),
                _ => "aé".repeat(120) + "\n",
            };
            let block = pattern.repeat((64 << 10) / pattern.len());
            let mut file = File::create(&path).unwrap();
            let mut remaining = mib * (1 << 20);
            while remaining > 0 {
                let count = block.floor_char_boundary(remaining.min(block.len() as u64) as usize);
                if count == 0 {
                    break;
                }
                file.write_all(&block.as_bytes()[..count]).unwrap();
                remaining -= count as u64;
            }
            file.sync_all().unwrap();
            let mut worker = coordinator(OpenOptions {
                working_directory: Some(working.clone()),
                ..OpenOptions::default()
            });
            worker.add_new().unwrap();
            worker.open_and_switch(&path).unwrap();
            worker.summarize();
            let (width, height) = if corpus == "unicode" {
                (240, 82)
            } else {
                (80, 24)
            };
            worker
                .action(Action::Size {
                    width,
                    height: height - 2,
                    generation: 7,
                    search_cover: 0,
                })
                .unwrap();
            let mut app = crate::app::AppState::new();
            let layout =
                crate::ui::LayoutSnapshot::for_app(Rect::new(0, 0, width, height), &app, 7);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut incorporation = Vec::new();
            let mut routing = Vec::new();
            let mut frames = Vec::new();
            app.update(worker.tab.screen.clone(), false);
            for _ in 0..40 {
                let input = crate::input::Input::Text("Z".into());
                let now = Instant::now();
                let Some(crate::app::Intent::Editor(action)) = app.input(input, &layout) else {
                    panic!("expected ordered edit");
                };
                routing.push(now.elapsed().as_secs_f64() * 1000.0);
                worker.begin();
                worker.action(action).unwrap();
                let screen = worker.tab.screen.clone();
                let now = Instant::now();
                app.update(screen, false);
                app.sync_layout(&layout);
                incorporation.push(now.elapsed().as_secs_f64() * 1000.0);
                let now = Instant::now();
                terminal
                    .draw(|frame| crate::ui::render(frame, &app, &layout))
                    .unwrap();
                frames.push(now.elapsed().as_secs_f64() * 1000.0);
            }
            let Response::Stats { revision, usage } = worker
                .call(Request::Stats(worker.document().unwrap()))
                .unwrap()
            else {
                panic!("expected usage");
            };
            assert_eq!(revision, worker.tab.info.as_ref().unwrap().revision);
            assert!(usage.cache_bytes <= (96 << 20));
            assert!(usage.cache_high_water_bytes <= (96 << 20));
            assert!(usage.buffer_high_water_bytes <= (32 << 20));
            assert!(crate::text::layout_high_water() <= (6 << 20));
            incorporation.sort_by(f64::total_cmp);
            routing.sort_by(f64::total_cmp);
            frames.sort_by(f64::total_cmp);
            eprintln!(
                "managed corpus={corpus} MiB={mib} cache_bytes={} cache_high_water={} buffer_high_water={} layout_charge_high_water={} working_text_bytes={} working_metadata_bytes={} working_allocated_blocks_bytes={} samples=40 reducer_max_ms={:.3} incorporation_p95_ms={:.3} incorporation_max_ms={:.3} frame_cpu_p95_ms={:.3} frame_cpu_max_ms={:.3}",
                usage.cache_bytes,
                usage.cache_high_water_bytes,
                usage.buffer_high_water_bytes,
                crate::text::layout_high_water(),
                usage.text_bytes,
                usage.metadata_bytes,
                blocks(&working),
                routing[39],
                incorporation[38],
                incorporation[39],
                frames[38],
                frames[39]
            );
            assert!(incorporation[39] < 2.0, "UI incorporation exceeded 2 ms");
        }
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    use std::io::Read;
    /// A detached bridge lets tests hold its input queue full without timing races.
    pub(super) fn bridge() -> (Editor, Receiver<Envelope>) {
        let (sender, inbox) = mpsc::sync_channel(256);
        let (_, events) = mpsc::sync_channel(16);
        (
            Editor {
                sender: Some(sender),
                events,
                screen: Arc::new(Mutex::new(None)),
                stop: Arc::new(AtomicBool::new(false)),
                activity: Arc::new(Mutex::new(Cancellation::default())),
                view_cancel: Arc::new(ViewControl::default()),
                used: Arc::new(QueueBudget::default()),
                paste_slot: Arc::new(Mutex::new(None)),
                sequence: 0,
                worker: None,
            },
            inbox,
        )
    }
    /// All admitted interactions remain owned on overload; cancellation reaches a queued paste.
    #[test]
    fn queue_overload_and_queued_paste_cancellation() {
        let (mut editor, inbox) = bridge();
        for _ in 0..256 {
            assert!(editor.submit(Action::Insert("x".into())));
        }
        assert!(!editor.submit(Action::Insert("lost".into())));
        assert_eq!(editor.sequence, 256);
        for _ in 0..256 {
            drop(inbox.recv().unwrap());
        }
        assert_eq!(editor.used.entries.load(Ordering::Acquire), 0);
        assert_eq!(editor.used.bytes.load(Ordering::Acquire), 0);
        let mut full = String::with_capacity((1 << 20) - Action::Insert(String::new()).bytes());
        full.push('x');
        assert!(editor.submit(Action::Insert(full)));
        assert_eq!(editor.used.bytes.load(Ordering::Acquire), 1 << 20);
        assert!(!editor.submit(Action::ClearSelection));
        drop(inbox.recv().unwrap());
        let mut spool = Spool::new().unwrap();
        spool.append(b"cancel this").unwrap();
        assert!(editor.submit(Action::Paste(Arc::new(spool))));
        editor.cancel();
        let envelope = inbox.recv().unwrap();
        assert!(envelope.paste_cancel.as_ref().unwrap().is_cancelled());
        let (_, receiver) = mpsc::sync_channel(256);
        let (events, _) = mpsc::sync_channel(16);
        let mut worker = Coordinator::new(
            StorageService::new(OpenOptions::default()).unwrap(),
            receiver,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.add_new().unwrap();
        worker.cancel = envelope.paste_cancel.clone().unwrap();
        assert!(matches!(
            worker.action(envelope.action.clone()),
            Err(Error::Cancelled)
        ));
        assert_eq!(worker.snapshot().unwrap().len(), 0);
        assert!(worker.tab.undo_views.is_empty());
    }
    /// Cancel reaches Open before execution; a later admitted replacement remains usable.
    #[test]
    fn cancellation_reaches_a_queued_open() {
        let (mut editor, inbox) = bridge();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new");
        std::fs::write(&path, "replacement").unwrap();
        assert!(editor.submit(Action::Open(path)));
        editor.cancel();
        assert!(editor.submit(Action::New));
        let (events, _) = mpsc::sync_channel(16);
        let (_, receiver) = mpsc::sync_channel(256);
        let mut worker = Coordinator::new(
            StorageService::new(OpenOptions::default()).unwrap(),
            receiver,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.add_new().unwrap();
        let original = worker.tab.screen.document;
        let open = inbox.recv().unwrap();
        worker.begin_envelope(&open);
        assert!(matches!(
            worker.action(open.action.clone()),
            Err(Error::Cancelled)
        ));
        assert_eq!(worker.tab.screen.document, original);
        let new = inbox.recv().unwrap();
        worker.begin_envelope(&new);
        worker.action(new.action.clone()).unwrap();
        assert_ne!(worker.tab.screen.document, original);
    }
    /// A later resize preserves ordered navigation, while a guard cancels a not-yet-started seek.
    #[test]
    fn view_generations_cancel_future_reads_without_losing_navigation() {
        let (mut editor, inbox) = bridge();
        let (events, _) = mpsc::sync_channel(16);
        let (_, receiver) = mpsc::sync_channel(256);
        let mut worker = Coordinator::new(
            StorageService::new(OpenOptions::default()).unwrap(),
            receiver,
            events,
            Arc::new(Mutex::new(None)),
            editor.stop.clone(),
            editor.activity.clone(),
            editor.view_cancel.clone(),
        );
        worker.begin();
        worker.add_new().unwrap();
        worker.action(Action::Insert("abc".into())).unwrap();
        assert!(editor.submit(Action::GoTo { line: 0, column: 1 }));
        let seek = inbox.recv().unwrap();
        assert!(editor.submit(Action::Size {
            width: 40,
            height: 6,
            generation: 2,
            search_cover: 0,
        }));
        worker.begin_envelope(&seek);
        worker.action(seek.action.clone()).unwrap();
        assert_eq!(worker.tab.screen.position.caret, 1);
        assert!(worker.tab.screen.view.is_none());
        let size = inbox.recv().unwrap();
        worker.begin_envelope(&size);
        worker.action(size.action.clone()).unwrap();
        assert_eq!(worker.tab.screen.view.as_ref().unwrap().generation, 2);
        assert!(editor.submit(Action::GoTo { line: 0, column: 3 }));
        let seek = inbox.recv().unwrap();
        assert!(editor.submit(Action::Guard(After::Quit)));
        worker.begin_envelope(&seek);
        assert!(matches!(
            worker.action(seek.action.clone()),
            Err(Error::Cancelled)
        ));
        assert_eq!(worker.tab.screen.position.caret, 1);
        // Explicit cancellation delivered before a read token is constructed also persists.
        worker.begin();
        editor.cancel();
        assert!(matches!(
            worker.movement(Movement::Right, false),
            Err(Error::Cancelled)
        ));
    }
    /// Stale cell maps cannot move a caret after edits, and vertical movement retains a preferred cell.
    #[test]
    fn stale_hits_and_preferred_columns() {
        let (_, receiver) = mpsc::sync_channel(256);
        let (events, _) = mpsc::sync_channel(16);
        let mut worker = Coordinator::new(
            StorageService::new(OpenOptions::default()).unwrap(),
            receiver,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.add_new().unwrap();
        worker
            .action(Action::Insert("ab\t界xx\nx\nab\t界xx".into()))
            .unwrap();
        let old = worker.tab.info.as_ref().unwrap().revision;
        let document = worker.document().unwrap();
        worker.action(Action::Insert("!".into())).unwrap();
        let position = worker.tab.screen.position;
        worker
            .action(Action::Hit {
                caret: 0,
                extend: false,
                generation: 0,
                document,
                revision: old,
                top: worker.tab.top,
                left: worker.tab.left,
                gesture: PointerGesture::Press(1),
            })
            .unwrap();
        assert_eq!(worker.tab.screen.position, position);
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        for _ in 0..4 {
            worker
                .action(Action::Command(Command::Move(Movement::Right, false)))
                .unwrap();
        }
        worker
            .action(Action::Command(Command::Move(Movement::Down, false)))
            .unwrap();
        assert_eq!(worker.tab.screen.view.as_ref().unwrap().column, 1);
        worker
            .action(Action::Command(Command::Move(Movement::Down, false)))
            .unwrap();
        assert_eq!(worker.tab.screen.view.as_ref().unwrap().column, 6);
        worker
            .action(Action::Command(Command::Move(Movement::Home, false)))
            .unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Up, false)))
            .unwrap();
        assert_eq!(worker.tab.screen.view.as_ref().unwrap().column, 0);
    }
    /// A failed cut must retain both document text and the previous clipboard, including selection.
    #[test]
    fn failed_cut_preserves_previous_clipboard() {
        #[derive(Debug, Default)]
        struct Fault(AtomicBool);
        impl document::FaultInjector for Fault {
            fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
                if point == document::IoPoint::Write && self.0.swap(false, Ordering::AcqRel) {
                    Err(std::io::Error::other("cut write failure"))
                } else {
                    Ok(())
                }
            }
        }
        let fault = Arc::new(Fault::default());
        let options = OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        };
        let (_, receiver) = mpsc::sync_channel(256);
        let (events, _) = mpsc::sync_channel(16);
        let mut worker = Coordinator::new(
            StorageService::new(options).unwrap(),
            receiver,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        worker.begin();
        worker.add_new().unwrap();
        worker
            .action(Action::Insert("old clipboard".into()))
            .unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        worker.action(Action::Command(Command::Copy)).unwrap();
        worker
            .action(Action::Insert("new selection".into()))
            .unwrap();
        worker.action(Action::Command(Command::SelectAll)).unwrap();
        let position = worker.tab.screen.position;
        fault.0.store(true, Ordering::Release);
        assert!(worker.action(Action::Command(Command::Cut)).is_err());
        assert_eq!(worker.tab.screen.position, position);
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "new selection"
        );
        let mut clipboard = String::new();
        worker
            .clipboard
            .as_ref()
            .unwrap()
            .reader()
            .read_to_string(&mut clipboard)
            .unwrap();
        assert_eq!(clipboard, "old clipboard");
    }
}

/// Hold a full queue until the test explicitly releases one entry.
#[cfg(test)]
pub(crate) fn saturated_input_queue() -> (Editor, impl FnMut()) {
    let (mut editor, inbox) = ordering_tests::bridge();
    for _ in 0..256 {
        assert!(editor.submit(Action::ClearSelection));
    }
    (editor, move || drop(inbox.try_recv().unwrap()))
}

#[cfg(test)]
mod search_tests {
    //! Coordinator acceptance tests run through the same actions as the terminal.
    use super::*;
    use crate::search::Query;
    use std::time::{Duration, Instant};

    /// Pump only live background handles until the bounded search finishes.
    fn settle(worker: &mut Coordinator) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.search.busy() {
            assert!(Instant::now() < deadline, "search did not settle");
            assert!(worker.poll_background());
            thread::yield_now();
        }
    }

    /// Gate only the named search thread; foreground reads never wait on this hook.
    #[derive(Debug)]
    struct SearchGate {
        armed: AtomicBool,
        name: &'static str,
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    #[derive(Debug)]
    struct CleanupGate {
        armed: AtomicBool,
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }
    impl document::FaultInjector for CleanupGate {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::Cleanup && self.armed.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    /// Use the real bridge and event loop so a blocked coordinator cannot falsely pass.
    #[test]
    fn retired_generation_cleanup_never_blocks_foreground_editing() {
        let (entered, reached) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let options = OpenOptions {
            faults: Some(Arc::new(CleanupGate {
                armed: AtomicBool::new(true),
                entered,
                release: Mutex::new(gate),
            })),
            ..OpenOptions::default()
        };
        let mut worker = super::tests::coordinator(options);
        let (sender, inbox) = mpsc::sync_channel(256);
        let (events, receiver) = mpsc::sync_channel(16);
        worker.inbox = inbox;
        worker.events = events;
        let mut editor = Editor {
            sender: Some(sender),
            events: receiver,
            screen: worker.output.clone(),
            stop: worker.stop.clone(),
            activity: worker.activity.clone(),
            view_cancel: worker.view_cancel.clone(),
            used: Arc::new(QueueBudget::default()),
            paste_slot: Arc::new(Mutex::new(None)),
            sequence: 0,
            worker: None,
        };
        editor.worker = Some(thread::spawn(move || {
            worker.run(PathBuf::new(), crate::stream::Startup::default())
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        while editor.screen().is_none_or(|screen| screen.loading) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(editor.submit(Action::Insert("one".into())));
        assert!(editor.submit(Action::Search(request(search::Operation::ReplaceAll))));
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(editor.submit(Action::Insert("!".into())));
        let sequence = editor.sequence();
        let deadline = Instant::now() + Duration::from_secs(5);
        let responsive = loop {
            if let Some(screen) = editor.screen()
                && screen.acknowledged >= sequence
                && screen.view.is_some()
            {
                break screen.view.as_ref().is_some_and(|view| {
                    view.rows
                        .iter()
                        .flat_map(|row| &row.spans)
                        .any(|span| span.text.contains('!'))
                });
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::yield_now();
        };
        release.send(()).unwrap();
        editor.shutdown().unwrap().join().unwrap();
        assert!(
            responsive,
            "cleanup held the foreground coordinator until filesystem release"
        );
    }
    impl document::FaultInjector for SearchGate {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::Read
                && thread::current().name() == Some(self.name)
                && self.armed.swap(false, Ordering::AcqRel)
            {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    fn gated(name: &'static str) -> (Coordinator, Receiver<()>, SyncSender<()>) {
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let options = OpenOptions {
            faults: Some(Arc::new(SearchGate {
                armed: AtomicBool::new(true),
                name,
                entered,
                release: Mutex::new(release_rx),
            })),
            ..OpenOptions::default()
        };
        (super::tests::coordinator(options), entered_rx, release)
    }

    fn request(operation: search::Operation) -> search::SearchRequest {
        search::SearchRequest {
            replacement_scope: None,
            navigation: None,
            query: Query {
                literal: "one".into(),
                ..Query::default()
            },
            replacement: "123456789".into(),
            all: false,
            operation,
            browse: false,
            expected: None,
        }
    }

    /// A later replacement may not edit the previous match while a chosen row is unresolved.
    #[test]
    fn pending_result_selection_cannot_replace_previous_match() {
        let (mut worker, reached, release) = gated("ede-search-page");
        worker.add_new().unwrap();
        worker.action(Action::Insert("one one".into())).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        let mut find = request(search::Operation::Next);
        find.browse = true;
        worker.action(Action::Search(find)).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.current.as_ref().unwrap().range, 0..3);
        let generation = worker.search.status.rows.generation;
        let navigation = worker
            .view_cancel
            .search_navigation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        worker
            .action(Action::SearchPage(search::results::PageRequest {
                generation,
                start: 0,
                jump: Some(1),
                navigation: Some(navigation),
            }))
            .unwrap();
        worker.poll_background();
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        worker
            .view_cancel
            .search_navigation
            .fetch_add(1, Ordering::AcqRel);
        let mut replace = request(search::Operation::ReplaceNext);
        replace.browse = true;
        replace.expected = Some((generation, 1));
        let result = worker.action(Action::Search(replace));
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "one one");
        assert!(matches!(result, Err(Error::StaleRevision)));
    }

    /// A folded no-op stays live while a changed occurrence also gets a receipt.
    #[test]
    fn replacement_feedback_records_only_actual_changes_and_clears_on_undo() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker.action(Action::Insert("one ONE one".into())).unwrap();
        let mut replace = request(search::Operation::ReplaceAll);
        replace.browse = true;
        replace.query.match_case = false;
        replace.replacement = "one".into();
        worker.action(Action::Search(replace)).unwrap();
        settle(&mut worker);
        let rows = &worker.search.status.rows;
        assert_eq!((rows.count, rows.live), (4, 3));
        worker
            .action(Action::SearchPage(search::results::PageRequest {
                generation: rows.generation,
                start: 0,
                jump: None,
                navigation: None,
            }))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.search.status.rows.page.is_empty() {
            assert!(Instant::now() < deadline, "replacement page did not arrive");
            worker.poll_background();
            thread::yield_now();
        }
        let mut changed = worker
            .search
            .status
            .rows
            .page
            .iter()
            .filter(|row| row.change.is_some());
        let receipt = changed.next().unwrap();
        assert_eq!(
            (receipt.column, receipt.matched.as_str(), receipt.change),
            (5, "ONE", Some("Replaced"))
        );
        assert!(changed.next().is_none());
        worker.action(Action::Command(Command::Undo)).unwrap();
        worker.poll_background();
        assert_eq!(super::tests::contents(&mut worker), "one ONE one");
        assert_eq!(worker.search.status.rows.count, 0);
    }

    /// Cancelling automatic navigation retains the replacement already acknowledged.
    #[test]
    fn replace_next_followup_cancellation_retains_committed_counts() {
        let (entered, reached) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let fault = Arc::new(SearchGate {
            armed: AtomicBool::new(false),
            name: "ede-search-scan",
            entered,
            release: Mutex::new(gate),
        });
        let mut worker = super::tests::coordinator(OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        });
        worker.action(Action::Insert("one".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        settle(&mut worker);
        fault.armed.store(true, Ordering::Release);
        worker
            .action(Action::Search(request(search::Operation::ReplaceNext)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            worker.poll_background();
            if reached.try_recv().is_ok() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        worker.action(Action::CancelSearch).unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "123456789");
        assert_eq!(worker.search.status.processed, 1);
        assert_eq!(worker.search.status.changed, 1);
        assert!(
            worker
                .search
                .status
                .message
                .contains("1 processed; 1 changed")
        );
        assert_eq!(
            worker.search.status.replacement_ack.as_ref().unwrap().count,
            1
        );
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.processed, 0);
        assert!(worker.search.status.replacement_ack.is_none());
    }

    /// Run the actual coordinator loop with its nonblocking terminal-side bridge.
    fn live_editor(options: OpenOptions) -> Editor {
        let mut worker = super::tests::coordinator(options);
        let (sender, inbox) = mpsc::sync_channel(256);
        let (events, receiver) = mpsc::sync_channel(16);
        worker.inbox = inbox;
        worker.events = events;
        let mut editor = Editor {
            sender: Some(sender),
            events: receiver,
            screen: worker.output.clone(),
            stop: worker.stop.clone(),
            activity: worker.activity.clone(),
            view_cancel: worker.view_cancel.clone(),
            used: Arc::new(QueueBudget::default()),
            paste_slot: Arc::new(Mutex::new(None)),
            sequence: 0,
            worker: None,
        };
        editor.worker = Some(thread::spawn(move || {
            worker.run(PathBuf::new(), crate::stream::Startup::default())
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        while editor.screen().is_none_or(|s| s.loading) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        editor
    }

    #[derive(Debug)]
    struct CommitGate {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }
    impl document::FaultInjector for CommitGate {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::EditCommitted {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    /// Cancel and Close admitted after commit must reconcile its count and dirty text.
    #[test]
    fn cancellation_and_close_after_commit_count_once_and_preserve_newer_input() {
        let (entered, reached) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let mut editor = live_editor(OpenOptions {
            faults: Some(Arc::new(CommitGate {
                entered,
                release: Mutex::new(gate),
            })),
            ..OpenOptions::default()
        });
        assert!(editor.submit(Action::Insert("one".into())));
        assert!(editor.submit(Action::Search(request(search::Operation::ReplaceAll))));
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(editor.submit(Action::CancelSearch));
        assert!(editor.submit(Action::Command(Command::Move(Movement::End, false))));
        assert!(editor.submit(Action::Insert("!".into())));
        let tab = editor.screen().unwrap().tab.unwrap();
        assert!(editor.submit(Action::Guard(After::Close(tab))));
        let sequence = editor.sequence();
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let screen = loop {
            if let Some(screen) = editor.screen()
                && screen.acknowledged >= sequence
            {
                break screen;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert_eq!(screen.search.processed, 1);
        assert_eq!(screen.search.changed, 1);
        assert!(
            matches!(editor.events.recv_timeout(Duration::from_secs(5)), Ok(Event::Guard(_, After::Close(id), true)) if id == tab)
        );
        assert!(editor.submit(Action::CancelGuard));
        assert!(editor.submit(Action::Command(Command::Move(Movement::End, false))));
        let sequence = editor.sequence();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(screen) = editor.screen()
                && screen.acknowledged >= sequence
                && let Some(view) = &screen.view
            {
                let text: String = view.rows[0].spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(text, "123456789!");
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        editor.shutdown().unwrap().join().unwrap();
    }

    /// Repeated private batches reclaim obsolete storage without another ordinary edit.
    #[test]
    fn repeated_replacements_reclaim_storage_and_preserve_undo() {
        let mut options = OpenOptions::default();
        options.limits.undo_transactions = 1;
        options.limits.undo_bytes = 128 << 10;
        let mut worker = super::tests::coordinator(options);
        worker.add_new().unwrap();
        worker.action(Action::Insert("A".repeat(64 << 10))).unwrap();
        for index in 0..40 {
            let (before, after) = if index % 2 == 0 {
                ("A", "B")
            } else {
                ("B", "A")
            };
            let mut request = request(search::Operation::ReplaceAll);
            request.query.literal = before.repeat(4096);
            request.replacement = after.repeat(4096);
            worker.action(Action::Search(request)).unwrap();
            settle(&mut worker);
            assert_eq!(worker.search.status.processed, 16);
        }
        let document = worker.document().unwrap();
        let Response::Stats { usage, .. } = worker.call(Request::Stats(document)).unwrap() else {
            panic!("expected storage usage");
        };
        assert!(
            usage.text_bytes < 2 << 20,
            "obsolete batches retained: {usage:?}"
        );
        assert!(usage.buffer_high_water_bytes <= 32 << 20);
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 4)
                .unwrap()
                .text(),
            "BBBB"
        );
    }

    /// A blocked private cleanup leaves foreground edits and newer search dispatch usable.
    #[test]
    fn editing_and_new_search_cancel_gated_maintenance() {
        let (mut worker, entered, release) = gated("ede-search-maintain");
        worker.add_new().unwrap();
        worker.action(Action::Insert("one".repeat(1024))).unwrap();
        // Force reclamation admission; production queues it only when storage reports garbage.
        let tab = worker.tab.screen.tab.unwrap();
        worker.tab.info.as_mut().unwrap().compaction_pending = true;
        worker.search.queue_maintenance(tab);
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.action(Action::Insert("!".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.total(), 1024);
        assert_eq!(worker.snapshot().unwrap().len(), 3073);
        assert!(worker.search.status.current.is_some());
    }

    /// A newly installed disk revision revokes both navigation and staged old replacements.
    #[test]
    fn external_refresh_revokes_gated_search_and_replacement() {
        for (operation, name) in [
            (search::Operation::Next, "ede-search-scan"),
            (search::Operation::ReplaceAll, "ede-search-plan"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("named");
            std::fs::write(&path, "one").unwrap();
            let (mut worker, entered, release) = gated(name);
            worker.open_and_switch(&path).unwrap();
            let tab = worker.tab.screen.tab.unwrap();
            let document = worker.document().unwrap();
            worker.action(Action::Search(request(operation))).unwrap();
            worker.poll_background();
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            std::fs::write(&path, "external").unwrap();
            let Response::RefreshTask(task) =
                worker.call(Request::PrepareRefresh(document)).unwrap()
            else {
                panic!("expected refresh task");
            };
            let prepared = task.run(&Cancellation::default()).unwrap();
            worker.install_refresh(tab, document, prepared).unwrap();
            release.send(()).unwrap();
            settle(&mut worker);
            assert_eq!(super::tests::contents(&mut worker), "external");
            assert_eq!(worker.search.status.processed, 0);
            assert!(worker.search.status.current.is_none());
            worker.action(Action::Command(Command::Undo)).unwrap();
            assert_eq!(super::tests::contents(&mut worker), "one");
        }
    }

    /// Maximum session membership retains bounded reports and survives repeated cancellation.
    #[test]
    fn sixty_four_tabs_and_repeated_cancellation_stay_within_shared_budget() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        for index in 0..64 {
            if index != 0 {
                worker.add_new().unwrap();
            }
            worker.action(Action::Insert("one".into())).unwrap();
        }
        let mut request = request(search::Operation::ReplaceAll);
        request.all = true;
        request.browse = true;
        worker.action(Action::Search(request.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.processed, 64);
        assert_eq!(worker.search.status.changed, 64);
        assert_eq!(worker.search.status.outcomes.len(), 64);
        assert_eq!(worker.search.status.rows.count, 64);
        assert!(!worker.search.status.rows.feedback_incomplete);
        request.operation = search::Operation::Next;
        request.query.literal = request.replacement.clone();
        for _ in 0..20 {
            worker.action(Action::Search(request.clone())).unwrap();
            worker.poll_background();
            worker.action(Action::CancelSearch).unwrap();
            settle(&mut worker);
            assert!(!worker.search.status.busy);
            assert!(worker.search.status.outcomes.len() <= 64);
        }
        worker.action(Action::Search(request)).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.total(), 64);
        assert_eq!(worker.search.status.rows.live, 64);
        let document = worker.document().unwrap();
        let Response::Stats { usage, .. } = worker.call(Request::Stats(document)).unwrap() else {
            panic!("expected storage usage");
        };
        assert!(usage.buffer_high_water_bytes <= 32 << 20, "{usage:?}");
        assert!(usage.cache_high_water_bytes <= 96 << 20, "{usage:?}");
    }

    #[test]
    fn edit_during_gated_planning_survives_stale_publication() {
        let (mut worker, entered, release) = gated("ede-search-plan");
        worker.add_new().unwrap();
        worker.action(Action::Insert("one one".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        worker.action(Action::Insert("new ".into())).unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "new one one");
        assert_eq!(worker.search.status.processed, 0);
        assert!(worker.search.status.message.contains("revision changed"));
    }

    #[test]
    fn replacement_maps_latest_view_after_gated_planning() {
        let (mut worker, entered, release) = gated("ede-search-plan");
        worker.add_new().unwrap();
        worker
            .action(Action::Insert("one\nabc\none".into()))
            .unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Finish, false)))
            .unwrap();
        worker.action(Action::Scroll(1, 0)).unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(
            super::tests::contents(&mut worker),
            "123456789\nabc\n123456789"
        );
        assert_eq!(worker.tab.screen.position.caret, 23);
        assert_eq!(worker.tab.top, 1);
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(worker.tab.screen.position.caret, 11);
    }

    #[test]
    fn cancellation_after_scan_cannot_install_navigation() {
        let (mut worker, entered, release) = gated("ede-search-scan");
        worker.add_new().unwrap();
        worker.action(Action::Insert("one".into())).unwrap();
        let first = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        let second = worker.tab.screen.tab.unwrap();
        let mut search = request(search::Operation::Next);
        search.all = true;
        worker.action(Action::Search(search)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if entered.try_recv().is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "scan never reached the gate");
            worker.poll_background();
            thread::yield_now();
        }
        worker.action(Action::CancelSearch).unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(worker.tab.screen.tab, Some(second));
        assert_ne!(first, second);
        assert!(worker.search.status.current.is_none());
    }

    #[test]
    fn no_op_continuation_releases_previous_search_reservation() {
        let mut options = OpenOptions::default();
        options.limits.buffer_bytes = 12 << 20;
        let mut worker = super::tests::coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        let mut search = request(search::Operation::Next);
        search.replacement = "one".into();
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        search.operation = search::Operation::ReplaceNext;
        worker.action(Action::Search(search)).unwrap();
        settle(&mut worker);
        assert!(worker.search.status.current.is_none());
        assert!(worker.search.status.message.ends_with("No further matches"));
        assert_eq!(worker.search.status.processed, 1);
        assert_eq!(worker.search.status.changed, 0);
        assert_eq!(super::tests::contents(&mut worker), "one");
    }

    #[test]
    fn read_only_retained_history_is_searchable_and_skipped_by_replacement() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker.action(Action::Insert("one".into())).unwrap();
        worker.tab.capability = crate::search::Capability::ReadOnly;
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.total(), 1);
        assert!(matches!(
            worker.action(Action::Search(request(search::Operation::ReplaceAll))),
            Err(Error::ReadOnly)
        ));
        let mut replace = request(search::Operation::ReplaceAll);
        replace.all = true;
        worker.action(Action::Search(replace)).unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "one");
        assert!(
            worker
                .search
                .status
                .outcomes
                .iter()
                .any(|o| o.message.contains("read-only"))
        );
        worker.tab.capability = crate::search::Capability::Editable;
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "123456789");
    }

    #[test]
    fn all_open_navigation_wraps_and_automatic_replacement_continuation_does_not() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker.action(Action::Insert("one A".into())).unwrap();
        let a = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        worker.action(Action::Insert("one B".into())).unwrap();
        let b = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        worker.action(Action::Insert("one C".into())).unwrap();
        let c = worker.tab.screen.tab.unwrap();
        worker.switch_tab(b).unwrap();
        let mut search = request(search::Operation::Next);
        search.all = true;
        search.replacement = "one".into();
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.current.as_ref().unwrap().tab, c);
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.current.as_ref().unwrap().tab, a);
        assert!(worker.search.status.message.contains("Wrapped"));
        search.operation = search::Operation::Previous;
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.current.as_ref().unwrap().tab, c);
        search.operation = search::Operation::ReplaceNext;
        worker.action(Action::Search(search)).unwrap();
        settle(&mut worker);
        assert!(worker.search.status.current.is_none());
        assert!(worker.search.status.message.ends_with("No further matches"));
        assert_eq!(worker.tab.screen.tab, Some(c));
        assert_eq!(
            worker.search.status.replacement_ack.as_ref().unwrap().tab,
            c
        );
    }

    #[test]
    fn coalesced_current_search_keeps_dispatch_target_after_switch() {
        let (mut worker, entered, release) = gated("ede-search-scan");
        worker.action(Action::Insert("one two".into())).unwrap();
        let a = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        worker.action(Action::Insert("two two".into())).unwrap();
        let b = worker.tab.screen.tab.unwrap();
        worker.switch_tab(a).unwrap();
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut newer = request(search::Operation::Next);
        newer.query.literal = "two".into();
        worker.action(Action::Search(newer)).unwrap();
        worker
            .view_cancel
            .search_navigation
            .fetch_add(1, Ordering::AcqRel);
        worker.action(Action::Switch(b)).unwrap();
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.total(), 1);
        assert_eq!(worker.search.status.outcomes[0].tab, a);
        assert_eq!(worker.tab.screen.tab, Some(b));
    }

    /// Fail only the second per-document preparation, independently of I/O call count.
    #[derive(Debug, Default)]
    struct FailSecondPlan(Mutex<Vec<thread::ThreadId>>);
    impl document::FaultInjector for FailSecondPlan {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::Write
                && thread::current().name() == Some("ede-search-plan")
            {
                let id = thread::current().id();
                let mut threads = self.0.lock().unwrap();
                if !threads.contains(&id) {
                    threads.push(id);
                }
                if threads.iter().position(|thread| *thread == id) == Some(1) {
                    return Err(std::io::Error::other("injected second-plan failure"));
                }
            }
            Ok(())
        }
    }

    #[test]
    fn all_open_failure_counts_only_completed_documents_and_retains_per_document_undo() {
        let options = OpenOptions {
            faults: Some(Arc::new(FailSecondPlan::default())),
            ..OpenOptions::default()
        };
        let mut worker = super::tests::coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        let a = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        worker.action(Action::Insert("one one".into())).unwrap();
        let b = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        worker.action(Action::Insert("one".into())).unwrap();
        let c = worker.tab.screen.tab.unwrap();
        let mut search = request(search::Operation::ReplaceAll);
        search.all = true;
        worker.action(Action::Search(search)).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.processed, 1);
        assert_eq!(worker.search.status.changed, 1);
        assert!(
            worker
                .search
                .status
                .outcomes
                .iter()
                .any(|o| o.tab == b && o.count == 0 && o.message.contains("failure"))
        );
        assert!(
            worker
                .search
                .status
                .outcomes
                .iter()
                .any(|o| o.tab == c && o.message == "Unprocessed")
        );
        assert_eq!(super::tests::contents(&mut worker), "one");
        worker.switch_tab(b).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "one one");
        worker.switch_tab(a).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "123456789");
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "one");
    }

    #[test]
    fn quit_waits_for_cancelled_plan_before_deciding_to_exit() {
        let (mut worker, entered, release) = gated("ede-search-plan");
        let (events, inbox) = mpsc::sync_channel(16);
        worker.events = events;
        worker.action(Action::Insert("one".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.active_sequence = 42;
        worker.action(Action::FinishQuit).unwrap();
        assert!(
            inbox.try_recv().is_err(),
            "quit was acknowledged before planning released its authority"
        );
        release.send(()).unwrap();
        settle(&mut worker);
        assert!(matches!(
            inbox.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::QuitReady(42)
        ));
        assert_eq!(super::tests::contents(&mut worker), "one");
    }

    #[test]
    fn search_current_and_replace_all_use_unsaved_content_and_one_undo() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker.add_new().unwrap();
        worker.action(Action::Insert("one two one".into())).unwrap();
        worker
            .action(Action::Command(Command::Move(Movement::Start, false)))
            .unwrap();
        let request = search::SearchRequest {
            replacement_scope: None,
            navigation: None,
            query: Query {
                literal: "one".into(),
                ..Query::default()
            },
            replacement: "1".into(),
            all: false,
            operation: search::Operation::Next,
            browse: false,
            expected: None,
        };
        worker.action(Action::Search(request.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.current.as_ref().unwrap().range, 0..3);
        assert_eq!(worker.search.status.total(), 2);
        worker
            .action(Action::Search(search::SearchRequest {
                operation: search::Operation::ReplaceAll,
                ..request
            }))
            .unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "1 two 1");
        worker.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "one two one");
    }

    #[test]
    fn review_snapshot_admission_failure_settles_search() {
        let mut options = OpenOptions::default();
        options.limits.leases = 1;
        let mut worker = super::tests::coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        let _held = worker.snapshot().unwrap();
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        assert!(matches!(worker.poll_search(), Err(Error::Busy)));
        assert!(!worker.search.busy());
        assert!(
            !worker.search.status.busy,
            "snapshot admission failure left the visible search busy"
        );
    }

    #[test]
    fn search_budget_rejection_does_not_leave_a_busy_session() {
        let mut options = OpenOptions::default();
        options.limits.buffer_bytes = 8 << 20;
        let mut worker = super::tests::coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        assert!(matches!(
            worker.action(Action::Search(request(search::Operation::ReplaceAll))),
            Err(Error::ResourceLimit(_))
        ));
        assert!(!worker.search.status.busy);
        assert!(!worker.search.busy());
        worker.action(Action::Insert("!".into())).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "one!");
    }

    #[test]
    fn exact_combining_match_never_reuses_an_unrelated_ordinary_selection() {
        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker
            .action(Action::Insert("e\u{301} foo".into()))
            .unwrap();
        worker.tab.screen.position = Position {
            anchor: 4,
            caret: 7,
        };
        worker.action(Action::Command(Command::Copy)).unwrap();
        let mut search = request(search::Operation::Next);
        search.query.literal = "\u{301}".into();
        search.replacement.clear();
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        assert_eq!(
            worker.tab.screen.position,
            Position {
                anchor: 0,
                caret: 0
            }
        );
        assert_eq!(worker.search.status.current.as_ref().unwrap().range, 1..3);
        worker.action(Action::Command(Command::Cut)).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "e\u{301} foo");
        worker.action(Action::Insert("X".into())).unwrap();
        assert_eq!(super::tests::contents(&mut worker), "Xe\u{301} foo");
        worker.action(Action::Command(Command::Undo)).unwrap();
        worker.action(Action::Search(search.clone())).unwrap();
        settle(&mut worker);
        search.operation = search::Operation::ReplaceNext;
        worker.action(Action::Search(search)).unwrap();
        settle(&mut worker);
        assert_eq!(super::tests::contents(&mut worker), "e foo");
    }

    #[test]
    fn review_newly_submitted_navigation_revokes_completed_older_navigation() {
        let (mut worker, entered, release) = gated("ede-search-scan");
        worker.action(Action::Insert("one".into())).unwrap();
        worker.add_new().unwrap();
        let second = worker.tab.screen.tab;
        let mut search = request(search::Operation::Next);
        search.all = true;
        worker.action(Action::Search(search.clone())).unwrap();
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let (sender, _inbox) = mpsc::sync_channel(256);
        let (_, events) = mpsc::sync_channel(16);
        let mut editor = Editor {
            sender: Some(sender),
            events,
            screen: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            activity: Arc::new(Mutex::new(Cancellation::default())),
            view_cancel: worker.view_cancel.clone(),
            used: Arc::new(QueueBudget::default()),
            paste_slot: Arc::new(Mutex::new(None)),
            sequence: 0,
            worker: None,
        };
        assert!(editor.submit(Action::Search(search)));
        release.send(()).unwrap();
        settle(&mut worker);
        assert_eq!(
            worker.tab.screen.tab, second,
            "older navigation activated its result after newer navigation was submitted"
        );
    }

    #[derive(Debug)]
    struct ReviewFailScanReads;
    impl document::FaultInjector for ReviewFailScanReads {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::Read
                && thread::current().name() == Some("ede-search-scan")
            {
                return Err(std::io::Error::other("review persistent scan read failure"));
            }
            Ok(())
        }
    }
    #[test]
    fn review_scan_failure_waits_for_explicit_retry_without_unbounded_outcomes() {
        let options = OpenOptions {
            faults: Some(Arc::new(ReviewFailScanReads)),
            ..OpenOptions::default()
        };
        let mut worker = super::tests::coordinator(options);
        worker.action(Action::Insert("one".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.outcomes.len(), 1);
        for _ in 0..4 {
            worker.poll_background();
            settle(&mut worker);
        }
        assert_eq!(
            worker.search.status.outcomes.len(),
            1,
            "failed scan retried automatically and appended outcomes"
        );
    }

    /// Quit remains available while post-replacement reclamation is paused.
    #[test]
    fn review_quit_during_postreplacement_maintenance_is_not_lost() {
        let (mut worker, entered, release) = gated("ede-search-maintain");
        worker.action(Action::Insert("one".into())).unwrap();
        worker
            .action(Action::Search(request(search::Operation::ReplaceAll)))
            .unwrap();
        settle(&mut worker);
        let tab = worker.tab.screen.tab.unwrap();
        worker.tab.info.as_mut().unwrap().compaction_pending = true;
        worker.search.queue_maintenance(tab);
        worker.poll_background();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let (events, inbox) = mpsc::sync_channel(16);
        worker.events = events;
        worker.active_sequence = 42;
        worker.action(Action::FinishQuit).unwrap();
        let acknowledged = matches!(inbox.try_recv(), Ok(Event::QuitReady(42)));
        release.send(()).unwrap();
        settle(&mut worker);
        assert!(
            acknowledged,
            "Quit was deferred behind maintenance without a completion replay"
        );
    }

    /// H07: retained read-only text and editable EOF use one tab choice and fresh logical offsets.
    #[test]
    fn retained_revision_highlighting_rebases_and_override_survives_eof() {
        use crate::syntax::{Category, Choice, ViewKey, project_snapshot_for_test};

        let mut worker = super::tests::coordinator(OpenOptions::default());
        worker
            .action(Action::Insert("one\nfn item() {}\n".into()))
            .unwrap();
        worker.tab.capability = crate::search::Capability::ReadOnly;
        let tab = worker.tab.screen.tab.unwrap();
        let document = worker.tab.screen.document.unwrap();
        let language = Choice::options()
            .find_map(|choice| match choice {
                Choice::Syntax(language) if language.id == "rust" => Some(language),
                _ => None,
            })
            .unwrap();
        worker
            .action(Action::SetLanguage(tab, Choice::Syntax(language)))
            .unwrap();
        worker
            .action(Action::Search(request(search::Operation::Next)))
            .unwrap();
        settle(&mut worker);
        assert_eq!(worker.search.status.total(), 1);
        let old = worker.snapshot().unwrap();
        let view = worker.tab.screen.view.as_ref().unwrap();
        let old_key = ViewKey::new(tab, document, view);
        let spans = project_snapshot_for_test(&old, view, language);
        assert!(
            spans
                .iter()
                .any(|span| span.start == 4 && span.category == Category::Keyword)
        );

        // Simulate retained-base eviction by changing the authoritative storage
        // revision without using the read-only tab's user edit path.
        let response = worker
            .call(Request::Replace {
                document,
                expected: old.revision(),
                range: TextRange::new(0, 4).unwrap(),
                text: String::new(),
            })
            .unwrap();
        assert!(matches!(response, Response::Changed { .. }));
        worker.refresh().unwrap();
        worker.tab.screen.position = Position::default();
        worker.layout(false).unwrap();
        let next = worker.snapshot().unwrap();
        let view = worker.tab.screen.view.as_ref().unwrap();
        assert_ne!(ViewKey::new(tab, document, view), old_key);
        let spans = project_snapshot_for_test(&next, view, language);
        assert!(
            spans
                .iter()
                .any(|span| span.start == 0 && span.category == Category::Keyword)
        );

        worker.tab.capability = crate::search::Capability::Editable;
        assert_eq!(worker.tab.screen.tab, Some(tab));
        assert_eq!(worker.tab.screen.syntax_language, Choice::Syntax(language));
        worker.action(Action::Insert("// EOF\n".into())).unwrap();
        assert_eq!(worker.tab.screen.syntax_language, Choice::Syntax(language));
    }
}
