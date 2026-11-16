//! Tab ownership and asynchronous file, save, and close coordination.

use super::*;

/// Stable session identity, allocated before a document has finished loading.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct TabId(pub(crate) u64);

/// Bounded data shared with the terminal for tab rendering and hit-testing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabSummary {
    pub id: TabId,
    pub label: Arc<str>,
    pub path_hint: Option<Arc<str>>,
    pub modified: bool,
    pub waiting: bool,
    pub loading: bool,
    pub saving: bool,
    pub queued_save: bool,
    pub attention: bool,
    pub scratch: bool,
    pub retention: &'static str,
    pub warning: Option<Arc<str>>,
}

/// The active or a parked document's editor state. Only the active tab has a layout.
#[derive(Default)]
pub(super) struct TabState {
    pub(super) live: Option<super::streams::LiveTab>,
    pub(super) syntax_attempt: u64,
    pub(super) syntax_failure: Option<syntax_state::Failure>,
    pub(super) syntax_effective: Option<&'static syntax::Language>,
    pub(super) capability: crate::search::Capability,
    pub(super) input: Arc<InputEpoch>,
    pub screen: Screen,
    pub info: Option<DocumentInfo>,
    pub uncertain: Option<JobId>,
    pub undo_views: VecDeque<(Position, Position)>,
    pub redo_views: Vec<(Position, Position)>,
    pub pending_edit: Option<CommittedEdit>,
    pub top: u64,
    pub left: u64,
    pub preferred: Option<u64>,
    pub(super) attention: bool,
    pub(super) scratch: Option<RetainedScratch>,
    pub(super) watch: Watch,
    pub(super) view_retry: Option<std::time::Instant>,
}

/// Creation order stays stable; the active entry's state lives in the coordinator.
pub(super) struct Tab {
    pub(super) id: TabId,
    label: Arc<str>,
    path_hint: Option<Arc<str>>,
    startup: bool,
    pub(super) state: Option<TabState>,
    path_charge: Option<document::Permit>,
    _metadata: document::Permit,
}

pub(super) struct OpenJob {
    tab: TabId,
    job: Job,
    request: u64,
    released: Option<Tab>,
}

/// The destination and its charge are retained until the save outcome is reconciled.
pub(super) struct SaveIntent {
    pub(super) tab: TabId,
    destination: Option<(Arc<Path>, document::Permit)>,
}

pub(super) struct RunningSave {
    pub job: Job,
    request: u64,
    pub(super) intent: SaveIntent,
}

pub(super) struct WaitingGuard {
    after: After,
    sequence: u64,
    tab: TabId,
    decision: WaitForSave,
}

enum WaitForSave {
    Clean,
    Save,
    Discard(Option<JobId>),
}

impl After {
    pub(crate) fn target(self) -> Option<TabId> {
        match self {
            Self::Close(tab) | Self::QuitTab(tab) => Some(tab),
            Self::Quit => None,
        }
    }
}

/// Join native path spellings without normalizing separators, dots, or symlink aliases.
pub(crate) fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

/// Compare the supplied spellings without Path's component normalization.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str() == b.as_os_str()
}

/// Limit display expansion independently of the native path and storage admission.
fn path_label(path: &Path) -> Arc<str> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    crate::text::safe(&name.chars().take(256).collect::<String>()).into()
}

/// Compact native path display for distinguishing tabs with the same basename.
fn tab_path_hint(path: &Path) -> Arc<str> {
    let spelling = path.to_string_lossy();
    let mut reversed: Vec<_> = spelling.chars().rev().take(256).collect();
    reversed.reverse();
    crate::text::safe(&reversed.into_iter().collect::<String>()).into()
}

impl Coordinator {
    #[cfg(test)]
    pub(super) fn open_and_switch(&mut self, path: &Path) -> Result<(), Error> {
        self.begin();
        self.add_named(path, false)?;
        self.poll_opens()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.opening.is_some() {
            self.receive()?;
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let id = self
            .tabs
            .iter()
            .find(|tab| {
                self.state(tab.id)
                    .and_then(|state| state.screen.path.as_deref())
                    .is_some_and(|p| same_path(p, path))
            })
            .map(|tab| tab.id)
            .ok_or(Error::Closed)?;
        self.switch_tab(id)
    }

    /// Establish startup order before admitting the first named preparation.
    pub(super) fn start(&mut self, directory: PathBuf, paths: Vec<PathBuf>) {
        self.launch_directory = directory;
        self.startup_fallback = true;
        for path in paths {
            if let Err(error) = self.add_named(&path, false) {
                self.open_error(&path, &error);
            }
        }
        for tab in &mut self.tabs {
            tab.startup = true;
        }
        if let Some(first) = self.tabs.first() {
            self.activate(first.id);
        }
        if let Err(error) = self.restore_startup() {
            self.event(Event::OpenFailed(format!("Scratch recovery: {error}")));
        }
    }

    /// Resolve a tab to its active or parked state without disturbing the active view.
    pub(super) fn state(&self, id: TabId) -> Option<&TabState> {
        if self.tab.screen.tab == Some(id) {
            Some(&self.tab)
        } else {
            self.tabs.iter().find(|tab| tab.id == id)?.state.as_ref()
        }
    }

    pub(super) fn state_mut(&mut self, id: TabId) -> Option<&mut TabState> {
        if self.tab.screen.tab == Some(id) {
            Some(&mut self.tab)
        } else {
            self.tabs
                .iter_mut()
                .find(|tab| tab.id == id)?
                .state
                .as_mut()
        }
    }

    /// Apply input to its original document, even when a paste finishes after a switch.
    pub(super) fn tab_action(&mut self, id: TabId, action: Action) -> Result<(), Error> {
        if let Action::Save(path) = action {
            return self.queue_save(id, path);
        }
        if self.tab.screen.tab == Some(id) {
            return if self.tab.screen.document.is_some() {
                self.action(action)
            } else {
                Ok(())
            };
        }
        // Pointer hits are valid only for the active viewport. A paste owns the
        // document it started in and has no dependency on pointer geometry.
        if matches!(action, Action::Hit { .. } | Action::Scroll(..)) {
            return Ok(());
        }
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return Ok(());
        };
        let Some(mut parked) = self.tabs[index].state.take() else {
            return Ok(());
        };
        if parked.screen.document.is_none() {
            self.tabs[index].state = Some(parked);
            return Ok(());
        }
        std::mem::swap(&mut self.tab, &mut parked);
        let cache = std::mem::take(&mut self.cache);
        self.parked_action = true;
        let result = self.action(action);
        self.parked_action = false;
        self.cache = cache;
        std::mem::swap(&mut self.tab, &mut parked);
        self.tabs[index].state = Some(parked);
        result
    }

    /// Publish compact summaries, reusing the Arc for ordinary input and progress frames.
    pub(super) fn summarize(&mut self) {
        let summaries = self.tabs.iter().map(|tab| {
            let state = tab.state.as_ref().unwrap_or(&self.tab);
            let opening = self.opening.as_ref().is_some_and(|job| job.tab == tab.id);
            TabSummary {
                id: tab.id,
                label: tab.label.clone(),
                path_hint: tab.path_hint.clone(),
                modified: state.screen.modified,
                waiting: state.screen.loading && !opening,
                loading: state.screen.loading && opening,
                saving: self
                    .saving
                    .as_ref()
                    .is_some_and(|save| save.intent.tab == tab.id),
                queued_save: self.saves.iter().any(|save| save.tab == tab.id),
                attention: state.attention
                    || state.uncertain.is_some()
                    || state.watch.warning.is_some(),
                scratch: state.screen.path.is_none() && state.live.is_none(),
                retention: if self
                    .retention
                    .as_ref()
                    .is_some_and(|retention| retention.checkpointing(tab.id))
                {
                    "Checkpointing"
                } else {
                    state.scratch.as_ref().map_or("", |scratch| {
                        scratch.status(state.info.as_ref().map(|info| info.revision))
                    })
                },
                warning: state
                    .scratch
                    .as_ref()
                    .and_then(|scratch| scratch.warning.as_deref())
                    .or(state.watch.warning.as_deref())
                    .map(Arc::from)
                    .or_else(|| {
                        (state.attention || state.uncertain.is_some())
                            .then(|| Arc::from(state.screen.message.as_str()))
                    }),
            }
        });
        if !summaries.clone().eq(self.summaries.iter().cloned()) {
            self.summaries = summaries.collect();
        }
        self.tab.screen.tabs = self.summaries.clone();
        let reserved = self
            .saves
            .iter()
            .chain(self.saving.iter().map(|save| &save.intent))
            .filter_map(|intent| intent.destination.as_ref().map(|(path, _)| path.clone()));
        if self.tab.screen.reserved_paths.len() != reserved.clone().count()
            || reserved
                .clone()
                .zip(self.tab.screen.reserved_paths.iter())
                .any(|(current, previous)| !Arc::ptr_eq(&current, previous))
        {
            self.tab.screen.reserved_paths = reserved.collect();
        }
        self.tab.screen.acknowledged = self.acknowledged;
    }

    /// Charge retained path copies and the bounded Stage 02 history positions up front.
    pub(super) fn admit_tab(&mut self, path: Option<PathBuf>) -> Result<TabId, Error> {
        let cancelling = self
            .opening
            .as_ref()
            .is_some_and(|job| self.state(job.tab).is_none());
        if self.tabs.len() + usize::from(cancelling) >= crate::TAB_LIMIT {
            return Err(Error::ResourceLimit("64 open tabs"));
        }
        // Charge both 1,000-entry history stacks, Vec growth, and tab metadata.
        let metadata = self.service.reserve_interaction(128 << 10)?;
        let path_charge = path.as_ref().map(|p| self.reserve_path(p)).transpose()?;
        self.next_tab += 1;
        let id = TabId(self.next_tab);
        let label = path.as_ref().map_or_else(
            || {
                self.next_untitled += 1;
                Arc::from(format!("Untitled {}", self.next_untitled))
            },
            |p| path_label(p),
        );
        let path_hint = path.as_deref().map(tab_path_hint);
        let state = TabState {
            screen: Screen {
                tab: Some(id),
                path,
                message: "Waiting to load".into(),
                ..Screen::default()
            },
            ..TabState::default()
        };
        self.view_cancel
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((id, state.input.clone()));
        self.tabs.push(Tab {
            id,
            label,
            path_hint,
            startup: false,
            state: Some(state),
            path_charge,
            _metadata: metadata,
        });
        Ok(id)
    }

    fn reserve_path(&self, path: &Path) -> Result<document::Permit, Error> {
        self.service
            .reserve_interaction(4096 + path.as_os_str().len().saturating_mul(8))
    }

    pub(super) fn label_tab(&mut self, id: TabId, label: Arc<str>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.label = label;
        }
    }

    pub(super) fn startup_tab(&self, id: TabId) -> bool {
        self.tabs.iter().any(|tab| tab.id == id && tab.startup)
    }

    /// Create an unnamed tab without joining the named-file preparation queue.
    pub(super) fn add_new(&mut self) -> Result<(), Error> {
        self.startup_fallback = false;
        let id = self.admit_tab(None)?;
        match self.call(Request::New) {
            Ok(Response::Ready { document, .. }) => {
                self.activate(id);
                self.tab.screen.document = Some(document);
                self.tab.screen.loading = false;
                self.tab.screen.message = "Ready".into();
                self.tab.scratch = self.fresh_scratch()?;
                self.refresh()?;
                self.layout(true)
            }
            response => {
                self.remove_tab(id)?;
                Err(response.err().unwrap_or(Error::CorruptStorage))
            }
        }
    }

    /// Focus an exact duplicate or append a new named tab in admission order.
    pub(super) fn add_named(&mut self, path: &Path, focus: bool) -> Result<(), Error> {
        let path = absolute(&self.launch_directory, path);
        if self.reserved(&path) {
            return Err(Error::ResourceLimit("path busy with Save As"));
        }
        let existing = self
            .tabs
            .iter()
            .find(|tab| {
                self.state(tab.id)
                    .and_then(|s| s.screen.path.as_deref())
                    .is_some_and(|p| same_path(p, &path))
                    && self.state(tab.id).is_some_and(|state| state.live.is_none())
            })
            .map(|tab| tab.id);
        let id = match existing {
            Some(id) => id,
            None => self.admit_tab(Some(path))?,
        };
        if focus {
            self.switch_tab(id)?;
        }
        Ok(())
    }

    /// Drop the outgoing layout, preserving the exact viewport coordinates and selection.
    pub(super) fn activate(&mut self, id: TabId) -> bool {
        if self.tab.screen.tab == Some(id) {
            return false;
        }
        let Some(next) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == id)
            .and_then(|t| t.state.take())
        else {
            return false;
        };
        let mut old = std::mem::replace(&mut self.tab, next);
        *self
            .view_cancel
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(id);
        if let Some(retention) = &mut self.retention
            && let Some(scratch) = &self.tab.scratch
        {
            retention.active = Some(scratch.id);
        }
        old.screen.view = None;
        old.screen.syntax_projection = None;
        old.screen.syntax_config = Arc::default();
        old.screen.syntax_notice = None;
        old.screen.tabs = Arc::default();
        old.screen.reserved_paths = Arc::default();
        old.screen.pointer_press = None;
        self.cache = Checkpoints::default();
        if let Some(entry) = self.tabs.iter_mut().find(|t| Some(t.id) == old.screen.tab) {
            entry.state = Some(old);
        }
        true
    }

    pub(super) fn switch_tab(&mut self, id: TabId) -> Result<(), Error> {
        if self.activate(id) && self.tab.screen.document.is_some() {
            self.layout(false)?;
        }
        Ok(())
    }

    pub(super) fn cycle_tab(&mut self, backwards: bool) -> Result<(), Error> {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| Some(t.id) == self.tab.screen.tab)
        {
            let count = self.tabs.len();
            let next = if backwards {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            };
            self.switch_tab(self.tabs[next].id)?;
        }
        Ok(())
    }

    /// Remove only the indicated entry. A running open remains charged until it settles.
    pub(super) fn remove_tab(&mut self, id: TabId) -> Result<(), Error> {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return Ok(());
        };
        let active = self.tab.screen.tab == Some(id);
        if let Some(document) = self.state(id).and_then(|state| state.screen.document) {
            self.service.close(document);
        }
        let removed = self.tabs.remove(index);
        self.view_cancel
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .retain(|(tab, _)| *tab != id);
        if let Some(opening) = self.opening.as_mut().filter(|job| job.tab == id) {
            opening.job.cancel();
            self.service.close(opening.job.document);
            opening.released = Some(removed);
        }
        if active {
            *self
                .view_cancel
                .current
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
            self.tab = TabState::default();
            self.tab.screen.loading = false;
            self.tab.screen.message = "No open tabs".into();
            self.cache = Checkpoints::default();
            if let Some(next) = self.tabs.get(index).or_else(|| self.tabs.last()) {
                self.switch_tab(next.id)?;
            }
        }
        Ok(())
    }

    pub(super) fn cancel_open(&mut self, id: TabId) -> Result<(), Error> {
        if self.state(id).is_some_and(|state| {
            state.live.is_none()
                && (state.screen.loading
                    || state.info.as_ref().is_some_and(|info| info.revision.0 == 0))
        }) && self
            .saving
            .as_ref()
            .is_none_or(|save| save.intent.tab != id)
            && !self.saves.iter().any(|save| save.tab == id)
        {
            let startup = self.tabs.iter().any(|tab| tab.id == id && tab.startup);
            self.remove_tab(id)?;
            if startup && self.tabs.is_empty() {
                self.startup_fallback = true;
            }
        }
        Ok(())
    }

    /// A pending Open can become ready before its immediate Escape reaches the coordinator.
    pub(super) fn cancel_active_open(&mut self, switch: Option<u64>) -> Result<(), Error> {
        if let Some(tab) = self.tab.screen.tab
            && (self.tab.screen.loading
                || switch
                    .is_some_and(|sequence| self.active_open_admission == Some((tab, sequence))))
        {
            self.cancel_open(tab)?;
        }
        Ok(())
    }

    /// Run one named preparation. New and ready-document requests stay independent.
    pub(super) fn poll_opens(&mut self) -> Result<bool, Error> {
        let mut changed = false;
        if let Some(opening) = self.opening.as_ref() {
            let tab = opening.tab;
            while let Some(progress) = self
                .opening
                .as_ref()
                .and_then(|open| open.job.take_progress())
            {
                if let Some(state) = self.state_mut(tab) {
                    state.screen.message = match progress {
                        OpenProgress::Preview { text, raw_bytes } => {
                            state.screen.preview = Some(text);
                            format!("Loading {raw_bytes} bytes (read-only preview)")
                        }
                        OpenProgress::Preparing { completed, total } => {
                            format!("Preparing {completed}/{total} bytes")
                        }
                    };
                    changed = true;
                }
            }
        } else if let Some((tab, path, restore)) = self.tabs.iter().find_map(|t| {
            let state = t.state.as_ref().unwrap_or(&self.tab);
            if state.screen.loading && state.live.is_none() {
                if let Some(scratch) = &state.scratch
                    && let Some((entry, path)) = &scratch.restore
                {
                    Some((t.id, path.clone(), Some(*entry)))
                } else {
                    state.screen.path.clone().map(|path| (t.id, path, None))
                }
            } else {
                None
            }
        }) {
            self.counter += 1;
            match self.service.submit(
                self.counter,
                restore.map_or_else(
                    || Request::Open(path.clone()),
                    |entry| Request::Restore {
                        path: path.clone(),
                        format: entry.format,
                        len: entry.len,
                    },
                ),
            ) {
                Ok(job) => {
                    self.opening = Some(OpenJob {
                        tab,
                        job,
                        request: self.counter,
                        released: None,
                    });
                    if let Some(state) = self.state_mut(tab) {
                        state.screen.message = "Preparing document".into();
                    }
                }
                Err(error) => {
                    self.remove_tab(tab)?;
                    self.open_error(&path, &error.error);
                }
            }
            changed = true;
        }
        if self.startup_fallback && self.tabs.is_empty() && self.opening.is_none() {
            self.startup_fallback = false;
            self.add_new()?;
            changed = true;
        }
        Ok(changed)
    }

    fn open_error(&self, path: &Path, error: &Error) {
        self.event(Event::OpenFailed(format!(
            "Open {}: {error} — {}",
            path_label(path),
            crate::text::safe(&path.to_string_lossy()),
        )));
    }

    /// Recognize completion even if the originating tab was closed in the meantime.
    pub(super) fn open_completed(&mut self, completion: document::Completion) -> Result<(), Error> {
        let opening = self.opening.take().expect("matching open job");
        let id = opening.tab;
        let startup = self.tabs.iter().any(|tab| tab.id == id && tab.startup);
        let failed = completion.result.is_err();
        let Some(state) = self.state_mut(id) else {
            self.service.close(completion.document);
            return Ok(());
        };
        match completion.result {
            Ok(Response::Ready { document, .. }) => {
                let info = match self.read_info(document) {
                    Ok(info) => info,
                    Err(error) => {
                        self.service.close(document);
                        self.record_restore_error(id, &error);
                        let path = self
                            .state(id)
                            .and_then(|state| {
                                state.screen.path.clone().or_else(|| {
                                    state
                                        .scratch
                                        .as_ref()
                                        .and_then(|scratch| scratch.restore.as_ref())
                                        .map(|(_, path)| path.clone())
                                })
                            })
                            .unwrap_or_default();
                        self.remove_tab(id)?;
                        self.open_error(&path, &error);
                        if startup {
                            self.restore_startup()?;
                        }
                        return Ok(());
                    }
                };
                self.startup_fallback = false;
                let state = self.state_mut(id).expect("open tab still exists");
                state.screen.document = Some(document);
                state.screen.loading = false;
                state.screen.preview = None;
                state.screen.message = "Ready".into();
                state.info = Some(info);
                let restored = self.restore_view(id)?;
                if self.tab.screen.tab == Some(id) {
                    self.layout(!restored)?;
                }
            }
            result => {
                let path = state
                    .screen
                    .path
                    .clone()
                    .or_else(|| {
                        state
                            .scratch
                            .as_ref()
                            .and_then(|scratch| scratch.restore.as_ref())
                            .map(|(_, path)| path.clone())
                    })
                    .unwrap_or_default();
                let error = result.err().unwrap_or(Error::CorruptStorage);
                if !matches!(error, Error::Cancelled) {
                    self.record_restore_error(id, &error);
                }
                self.remove_tab(id)?;
                if !matches!(error, Error::Cancelled) {
                    self.open_error(&path, &error);
                }
            }
        }
        if startup && failed {
            self.restore_startup()?;
        }
        Ok(())
    }

    pub(super) fn receive(&mut self) -> Result<bool, Error> {
        let mut changed = false;
        while let Some(completion) = self.service.try_recv()? {
            self.route_completion(completion);
            changed |= self.settle_completions()?;
        }
        Ok(changed)
    }

    /// Foreground calls may encounter a completed background job. Defer reconciliation.
    pub(super) fn route_completion(&mut self, completion: document::Completion) {
        if self.opening.as_ref().is_some_and(|job| {
            job.job.id == completion.job
                && job.job.document == completion.document
                && job.request == completion.request
        }) {
            self.completed_open = Some(completion);
        } else if self.saving.as_ref().is_some_and(|job| {
            job.job.id == completion.job
                && job.job.document == completion.document
                && job.request == completion.request
        }) {
            self.completed_save = Some(completion);
        } else {
            self.syntax.receive(completion);
        }
    }

    pub(super) fn settle_completions(&mut self) -> Result<bool, Error> {
        let mut changed = false;
        // Reconciliation may receive another completion through a foreground Info call.
        while self.completed_open.is_some() || self.completed_save.is_some() {
            if let Some(completion) = self.completed_open.take() {
                self.open_completed(completion)?;
            }
            if let Some(completion) = self.completed_save.take() {
                self.save_completed(completion)?;
            }
            changed = true;
        }
        Ok(changed)
    }

    fn reserved(&self, path: &Path) -> bool {
        self.saves
            .iter()
            .chain(self.saving.iter().map(|save| &save.intent))
            .any(|intent| {
                intent
                    .destination
                    .as_ref()
                    .is_some_and(|(reserved, _)| same_path(path, reserved))
            })
    }

    /// Retain at most one pending intent per tab and reserve Save As immediately.
    pub(super) fn queue_save(&mut self, id: TabId, path: Option<PathBuf>) -> Result<(), Error> {
        let Some(state) = self.state(id) else {
            return Ok(());
        };
        if state.capability == crate::search::Capability::ReadOnly {
            return Err(Error::ReadOnly);
        }
        if state.screen.loading {
            return Err(Error::Busy);
        }
        let pending = self.saves.iter().any(|save| save.tab == id);
        let running_as = self.running_save_as(id);
        let destination = if let Some(path) = path {
            if pending || running_as {
                return Err(Error::ResourceLimit(
                    "tab already has a queued save or running Save As",
                ));
            }
            let path = absolute(&self.launch_directory, &path);
            if self.reserved(&path)
                || self.tabs.iter().any(|tab| {
                    tab.id != id
                        && self
                            .state(tab.id)
                            .and_then(|s| s.screen.path.as_deref())
                            .is_some_and(|p| same_path(p, &path))
                })
            {
                return Err(Error::ResourceLimit(
                    "Save As destination is busy in another tab or save",
                ));
            }
            let charge = self.reserve_path(&path)?;
            Some((path.into(), charge))
        } else {
            if pending {
                return Ok(());
            }
            if state.screen.path.is_none() && !running_as {
                self.event(Event::NeedSaveAs(self.active_sequence, id));
                return Ok(());
            }
            None
        };
        self.saves.push_back(SaveIntent {
            tab: id,
            destination,
        });
        self.start_next_save()
    }

    fn running_save_as(&self, id: TabId) -> bool {
        self.saving
            .as_ref()
            .is_some_and(|save| save.intent.tab == id && save.intent.destination.is_some())
    }

    /// Continue the specific close/quit decision through the save queue.
    pub(super) fn save_for_guard(
        &mut self,
        after: After,
        path: Option<PathBuf>,
    ) -> Result<(), Error> {
        let Some(id) = after.target().or(self.tab.screen.tab) else {
            return Ok(());
        };
        let Some(state) = self.state(id) else {
            return Ok(());
        };
        if state.capability == crate::search::Capability::ReadOnly {
            return Err(Error::ReadOnly);
        }
        if path.is_none()
            && state.screen.path.is_none()
            && !self.running_save_as(id)
            && !self
                .saves
                .iter()
                .any(|save| save.tab == id && save.destination.is_some())
        {
            self.event(Event::NeedGuardSaveAs(self.active_sequence, after, id));
            return Ok(());
        }
        self.waiting_guard = Some(WaitingGuard {
            after,
            sequence: self.active_sequence,
            tab: id,
            decision: WaitForSave::Save,
        });
        let result = self.queue_save(id, path);
        if result.is_err() {
            self.waiting_guard = None;
        }
        result
    }

    /// Capture the latest acknowledged state when the single save worker becomes available.
    pub(super) fn start_next_save(&mut self) -> Result<(), Error> {
        while self.saving.is_none() {
            let Some(intent) = self.saves.pop_front() else {
                break;
            };
            let Some(document) = self
                .state(intent.tab)
                .and_then(|state| state.screen.document)
            else {
                continue;
            };
            let target = intent
                .destination
                .as_ref()
                .map_or(SaveTarget::Current, |(path, _)| {
                    SaveTarget::New(path.to_path_buf())
                });
            let result = self.snapshot_of(document).and_then(|snapshot| {
                self.counter += 1;
                self.service
                    .submit(self.counter, Request::Save { snapshot, target })
                    .map_err(|error| error.error)
            });
            match result {
                Ok(job) => {
                    if let Some(state) = self.state_mut(intent.tab) {
                        state.screen.message = "Saving".into();
                    }
                    self.saving = Some(RunningSave {
                        job,
                        intent,
                        request: self.counter,
                    });
                }
                Err(Error::Closed) => return Err(Error::Closed),
                Err(error) => {
                    self.saves.retain(|next| next.tab != intent.tab);
                    self.waiting_guard.take_if(|guard| guard.tab == intent.tab);
                    self.report_save(
                        intent.tab,
                        SaveResult::Failed,
                        format!("Save failed: {error}. Use Save As (F4)."),
                    );
                }
            }
        }
        Ok(())
    }

    fn report_save(&mut self, id: TabId, result: SaveResult, mut message: String) {
        if self.tab.screen.tab != Some(id)
            && let Some(tab) = self.tabs.iter().find(|tab| tab.id == id)
        {
            message = format!("{} — {message}", tab.label);
        }
        if let Some(state) = self.state_mut(id) {
            state.attention = matches!(result, SaveResult::Failed | SaveResult::Uncertain)
                || state.uncertain.is_some();
            state.screen.message.clone_from(&message);
        }
        self.event(Event::Saved(id, result, message));
    }

    /// Apply the exact saved point and pathname only to the originating document.
    fn save_completed(&mut self, completion: document::Completion) -> Result<(), Error> {
        let saving = self.saving.take().expect("matching save job");
        let id = saving.intent.tab;
        let (result, mut message) = match completion.result {
            Ok(Response::Saved(report)) => match report.outcome {
                SaveOutcome::Saved => (SaveResult::Saved, "Saved".into()),
                SaveOutcome::NotCommitted if matches!(report.error, Some(Error::Cancelled)) => {
                    (SaveResult::Cancelled, "Save cancelled".into())
                }
                SaveOutcome::NotCommitted => (
                    SaveResult::Failed,
                    format!(
                        "Save failed: {}. Use Save As (F4).",
                        report
                            .error
                            .map_or_else(|| "Unknown error".into(), |e| e.to_string())
                    ),
                ),
                SaveOutcome::CommittedDurabilityUncertain => (
                    SaveResult::Uncertain,
                    format!(
                        "Destination may have changed; save is uncertain: {}",
                        report
                            .error
                            .map_or_else(|| "Unknown error".into(), |e| e.to_string())
                    ),
                ),
            },
            Err(Error::SaveWorkerPanicked) => {
                (SaveResult::Uncertain, Error::SaveWorkerPanicked.to_string())
            }
            Err(Error::Cancelled) => (SaveResult::Cancelled, "Save cancelled".into()),
            Err(error) => (
                SaveResult::Failed,
                format!("Save failed: {error}. Use Save As (F4)."),
            ),
            _ => (SaveResult::Failed, "Unexpected save response".into()),
        };
        if result == SaveResult::Saved
            && let Some((path, charge)) = saving.intent.destination
            && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id)
        {
            tab.label = path_label(&path);
            tab.path_hint = Some(tab_path_hint(&path));
            tab.path_charge = Some(charge);
        }
        if let Some(state) = self.state_mut(id) {
            match result {
                SaveResult::Saved => state.uncertain = None,
                SaveResult::Uncertain => state.uncertain = Some(completion.job),
                _ => {}
            }
        }
        self.refresh_tab(id, completion.document)?;
        if result == SaveResult::Saved && self.state(id).is_some_and(|state| state.screen.modified)
        {
            message = "Saved captured revision; newer edits unsaved".into();
        }
        if result != SaveResult::Saved {
            self.saves.retain(|next| next.tab != id);
        }
        self.report_save(id, result, message);
        if let Some(guard) = self.waiting_guard.take_if(|guard| guard.tab == id) {
            match guard.decision {
                WaitForSave::Discard(acknowledged) if result != SaveResult::Failed => {
                    self.finish_discard(guard.after, id, guard.sequence, acknowledged);
                }
                WaitForSave::Clean
                    if matches!(result, SaveResult::Saved | SaveResult::Cancelled) =>
                {
                    self.check_guard(guard.after, guard.sequence)?;
                }
                WaitForSave::Save if result == SaveResult::Saved => {
                    if self.saves.iter().any(|save| save.tab == id) {
                        self.waiting_guard = Some(guard);
                    } else {
                        self.check_guard(guard.after, guard.sequence)?;
                    }
                }
                WaitForSave::Save if result == SaveResult::Cancelled => {
                    self.event(Event::GuardSaveStopped(guard.sequence));
                }
                _ => {}
            }
        }
        self.start_next_save()
    }

    /// Start quit as an ordered list of decisions. The UI commits only after all resolve.
    pub(super) fn guard(&mut self, after: After) -> Result<(), Error> {
        self.waiting_guard = None;
        self.stream_guard = None;
        let streams: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| {
                (after == After::Quit || after.target() == Some(tab.id))
                    && self
                        .state(tab.id)
                        .and_then(|state| state.screen.stream.as_ref())
                        .is_some_and(|status| !status.stopped || status.retained > 0)
            })
            .collect();
        if !streams.is_empty() && !matches!(after, After::QuitTab(_)) {
            self.stream_guard = Some(after);
            let source = if streams.len() == 1 {
                streams[0].label.to_string()
            } else {
                format!("{} streams", streams.len())
            };
            self.event(Event::StreamGuard(
                self.active_sequence,
                after,
                format!("{source}\nStop capture and discard retained stream history?"),
            ));
            return Ok(());
        }
        self.guard_regular(after)
    }

    fn guard_regular(&mut self, after: After) -> Result<(), Error> {
        if after == After::Quit && self.tabs.len() > 1 {
            self.event(Event::QuitTabs(
                self.active_sequence,
                self.tabs.iter().map(|tab| tab.id).collect(),
            ));
            Ok(())
        } else {
            self.check_guard(after, self.active_sequence)
        }
    }

    fn check_guard(&mut self, after: After, sequence: u64) -> Result<(), Error> {
        let id = after.target().or(self.tab.screen.tab);
        let mut dirty = false;
        if let Some(id) = id
            && let Some(state) = self.state(id)
        {
            if state.live.is_some() {
                self.event(Event::Guard(sequence, after, false));
                return Ok(());
            }
            if let Some(document) = state.screen.document {
                self.refresh_tab(id, document)?;
            }
            let state = self.state(id).expect("checked tab");
            dirty = (state.screen.modified || state.uncertain.is_some())
                && !(self.retention.is_some()
                    && state.screen.path.is_none()
                    && state.scratch.is_some()
                    && !matches!(after, After::Close(_)));
            if self.retention.is_some()
                && state.screen.path.is_none()
                && state.scratch.is_some()
                && matches!(after, After::Close(_))
            {
                dirty = true;
            }
            let saving = self
                .saving
                .as_ref()
                .is_some_and(|save| save.intent.tab == id);
            let queued = self.saves.iter().any(|save| save.tab == id);
            if !dirty && (saving || queued) {
                self.waiting_guard = Some(WaitingGuard {
                    after,
                    sequence,
                    tab: id,
                    decision: WaitForSave::Clean,
                });
                return Ok(());
            }
        }
        self.event(Event::Guard(sequence, after, dirty));
        Ok(())
    }

    pub(super) fn discard(&mut self, after: After, acknowledged: Option<JobId>) {
        if self.stream_guard == Some(after) {
            self.stream_guard = None;
            let result = match after {
                After::Quit => self.guard_regular(after),
                After::Close(tab) if self.state(tab).is_some_and(|state| state.live.is_some()) => {
                    self.remove_tab(tab)
                        .map(|()| self.event(Event::Proceed(self.active_sequence, after)))
                }
                _ => self.check_guard(after, self.active_sequence),
            };
            if let Err(error) = result {
                self.event(Event::Failure(error.to_string()));
            }
            return;
        }
        let Some(id) = after.target().or(self.tab.screen.tab) else {
            self.event(Event::Proceed(self.active_sequence, after));
            return;
        };
        self.saves.retain(|save| save.tab != id);
        if let Some(save) = self.saving.as_ref().filter(|save| save.intent.tab == id) {
            save.job.cancel();
            self.waiting_guard = Some(WaitingGuard {
                after,
                sequence: self.active_sequence,
                tab: id,
                decision: WaitForSave::Discard(acknowledged),
            });
        } else {
            self.finish_discard(after, id, self.active_sequence, acknowledged);
        }
    }

    fn finish_discard(
        &mut self,
        after: After,
        id: TabId,
        sequence: u64,
        acknowledged: Option<JobId>,
    ) {
        if let Some(job) = self
            .state(id)
            .and_then(|state| state.uncertain)
            .filter(|job| Some(*job) != acknowledged)
        {
            self.event(Event::Uncertain(sequence, after, job));
        } else {
            if matches!(after, After::Close(_)) && self.retire_before_close(id, after, sequence) {
                return;
            }
            self.event(Event::Proceed(sequence, after));
        }
    }

    fn refresh_tab(&mut self, id: TabId, document: DocumentId) -> Result<(), Error> {
        let info = self.read_info(document)?;
        let config = self.syntax.config();
        if let Some(state) = self.state_mut(id) {
            state.apply_info(info);
            state.resolve_syntax(&config);
        }
        Ok(())
    }
}

impl TabState {
    /// Retire a latched failure whenever the effective language changes.
    pub(super) fn resolve_syntax(&mut self, config: &syntax::Config) {
        let language = self
            .screen
            .syntax_language
            .effective(self.screen.path.as_deref(), config);
        if self.syntax_effective != language {
            self.syntax_effective = language;
            self.syntax_attempt = self.syntax_attempt.wrapping_add(1);
            self.syntax_failure = None;
        }
    }

    pub(super) fn apply_info(&mut self, info: DocumentInfo) {
        self.screen.path = info.path.clone();
        self.screen.modified =
            info.dirty && (info.path.is_some() || info.len != 0 || self.uncertain.is_some());
        self.info = Some(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, time::Instant};

    /// A single fault boundary is held until the test releases it.
    #[derive(Debug)]
    struct Gate {
        point: document::IoPoint,
        entered: mpsc::SyncSender<()>,
        release: Mutex<Option<Receiver<()>>>,
    }

    impl document::FaultInjector for Gate {
        fn check(&self, point: document::IoPoint) -> io::Result<()> {
            if point != self.point {
                return Ok(());
            }
            let release = self.release.lock().unwrap().take();
            if let Some(release) = release {
                self.entered.send(()).unwrap();
                release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
            }
            Ok(())
        }
    }

    /// Build the production coordinator with a deterministically gated filesystem boundary.
    fn gated(point: document::IoPoint) -> (Coordinator, Receiver<()>, SyncSender<()>) {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let (_, inbox) = mpsc::sync_channel(16);
        let (events, _) = mpsc::sync_channel(16);
        let options = OpenOptions {
            faults: Some(Arc::new(Gate {
                point,
                entered,
                release: Mutex::new(Some(blocked)),
            })),
            ..OpenOptions::default()
        };
        let worker = Coordinator::new(
            StorageService::new(options).unwrap(),
            inbox,
            events,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Cancellation::default())),
            Arc::new(ViewControl::default()),
        );
        (worker, started, release)
    }

    fn until(worker: &mut Coordinator, ready: impl Fn(&Coordinator) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready(worker) {
            assert!(worker.poll_background());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Active editing proceeds behind a blocked open; the removed tab cannot reappear.
    #[test]
    fn blocked_open_can_be_cancelled_without_losing_edits_or_reviving_tab() {
        let (mut worker, entered, release) = gated(document::IoPoint::Read);
        let temp = tempfile::tempdir().unwrap();
        let slow = temp.path().join("slow");
        let queued = temp.path().join("queued");
        std::fs::write(&slow, "blocked open").unwrap();
        std::fs::write(&queued, "queued body").unwrap();
        worker.add_new().unwrap();
        let first = worker.tab.screen.tab.unwrap();
        worker.add_named(&slow, true).unwrap();
        let doomed = worker.tab.screen.tab.unwrap();
        worker.add_named(&queued, true).unwrap();
        let third = worker.tab.screen.tab.unwrap();
        worker.poll_opens().unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.switch_tab(first).unwrap();
        worker.action(Action::Insert("independent".into())).unwrap();
        worker.cancel_open(doomed).unwrap();
        assert_eq!(worker.tab.screen.tab, Some(first));
        assert!(worker.state(doomed).is_none());
        assert!(worker.state(third).unwrap().screen.loading);
        release.send(()).unwrap();
        until(&mut worker, |w| !w.state(third).unwrap().screen.loading);
        assert_eq!(worker.tab.screen.tab, Some(first));
        assert!(worker.state(doomed).is_none());
        assert_eq!(
            worker
                .snapshot()
                .unwrap()
                .read_window(ByteOffset(0), 64)
                .unwrap()
                .text(),
            "independent"
        );
    }

    /// Saves share one FIFO and acknowledge only the revision captured when each starts.
    #[test]
    fn queued_saves_stay_with_their_tab_and_latest_acknowledged_bytes() {
        let (mut worker, entered, release) = gated(document::IoPoint::Publish);
        let temp = tempfile::tempdir().unwrap();
        let path_a = temp.path().join("a");
        let path_b = temp.path().join("b");
        let path_c = temp.path().join("c");
        std::fs::write(&path_a, "A").unwrap();
        std::fs::write(&path_b, "B").unwrap();
        worker.open_and_switch(&path_a).unwrap();
        let a = worker.tab.screen.tab.unwrap();
        worker.open_and_switch(&path_b).unwrap();
        let b = worker.tab.screen.tab.unwrap();
        worker.add_new().unwrap();
        let c = worker.tab.screen.tab.unwrap();
        let (sender, events) = mpsc::sync_channel(16);
        worker.events = sender;
        worker.switch_tab(a).unwrap();
        worker.action(Action::Insert("first ".into())).unwrap();
        worker.queue_save(a, None).unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.switch_tab(b).unwrap();
        worker.action(Action::Insert("early ".into())).unwrap();
        worker.queue_save(b, None).unwrap();
        worker.queue_save(b, None).unwrap();
        worker.action(Action::Insert("later ".into())).unwrap();
        worker.queue_save(c, Some(path_c.clone())).unwrap();
        worker.queue_save(c, None).unwrap();
        assert!(worker.add_named(&path_c, true).is_err());
        assert!(worker.queue_save(b, Some(path_c.clone())).is_err());
        worker.switch_tab(a).unwrap();
        worker.action(Action::Insert("unsaved ".into())).unwrap();
        worker.switch_tab(b).unwrap();

        release.send(()).unwrap();
        until(&mut worker, |w| w.saves.is_empty() && w.saving.is_none());
        let saved_tabs: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::Saved(tab, SaveResult::Saved, _) => Some(tab),
                _ => None,
            })
            .collect();
        assert_eq!(saved_tabs, [a, b, c]);
        assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "first A");
        assert_eq!(std::fs::read_to_string(path_b).unwrap(), "early later B");
        assert!(path_c.exists());
        assert!(worker.state(a).unwrap().screen.modified);
        assert!(!worker.state(b).unwrap().screen.modified);
        assert_eq!(
            worker.state(c).unwrap().screen.path.as_deref(),
            Some(path_c.as_path())
        );
        assert_eq!(worker.tab.screen.tab, Some(b));
    }

    /// Tab admission remains bounded, reusable, and compares raw path spelling.
    #[test]
    fn capacity_reuse_and_exact_native_spelling() {
        let (mut worker, _, _) = gated(document::IoPoint::Publish);
        let temp = tempfile::tempdir().unwrap();
        worker.start(temp.path().to_owned(), vec![]);
        worker.add_named(Path::new("file"), true).unwrap();
        let first = worker.tab.screen.tab.unwrap();
        worker.add_named(&temp.path().join("file"), true).unwrap();
        assert_eq!(worker.tab.screen.tab, Some(first));
        worker.add_named(Path::new("./file"), true).unwrap();
        assert_ne!(worker.tab.screen.tab, Some(first));
        for n in 2..crate::TAB_LIMIT {
            worker
                .add_named(Path::new(&format!("file-{n}")), false)
                .unwrap();
        }
        assert!(worker.add_new().is_err());
        assert!(worker.add_named(Path::new("overflow"), true).is_err());
        worker.cancel_open(first).unwrap();
        worker.add_new().unwrap();
        assert_eq!(worker.tabs.len(), crate::TAB_LIMIT);
        assert!(worker.tab.screen.document.is_some());
    }

    /// Escape accepted against an opening tab still wins if Ready arrives before dispatch.
    #[test]
    fn late_open_cancellation_respects_dispatch_and_startup_fallback() {
        let (mut worker, _, _) = gated(document::IoPoint::Publish);
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let next = temp.path().join("next");
        std::fs::write(&first, "startup").unwrap();
        std::fs::write(&next, "interactive").unwrap();
        worker.start(temp.path().to_owned(), vec![first]);
        let startup = worker.tab.screen.tab.unwrap();
        until(&mut worker, |w| !w.tab.screen.loading);
        worker.cancel_open(startup).unwrap();
        worker.poll_opens().unwrap();
        let fallback = worker.tab.screen.tab.unwrap();
        assert_ne!(fallback, startup);
        assert!(worker.tab.screen.path.is_none());

        worker.active_sequence = 10;
        worker.action(Action::Open(next)).unwrap();
        let opening = worker.tab.screen.tab.unwrap();
        until(&mut worker, |w| !w.tab.screen.loading);
        worker.cancel_active_open(Some(9)).unwrap();
        assert_eq!(worker.tab.screen.tab, Some(opening));
        worker.cancel_active_open(Some(10)).unwrap();
        assert_eq!(worker.tab.screen.tab, Some(fallback));
        assert_eq!(worker.tabs.len(), 1);
    }
}
