//! Active-tab syntax coordination. Storage leases and child I/O stay off the foreground path.

use std::{
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{Coordinator, TabId};
use crate::{
    document::{self, Cancellation, DocumentId, Error, Job, Request, Response, Revision},
    syntax::{
        Config, Fallback, Language, ProcessControl, Session, Status, ViewKey, Work, WorkerResult,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    tab: TabId,
    document: DocumentId,
    revision: Revision,
    language: &'static Language,
    attempt: u64,
    rules: u64,
    limits: u64,
}

/// A tab's fallback is separate from its screen and may survive view and revision changes.
#[derive(Clone, Copy)]
pub(super) struct Failure {
    identity: Identity,
    reason: Fallback,
}

impl Failure {
    fn for_request(self, request: Identity) -> Option<Fallback> {
        let identity = self.identity;
        (identity.language == request.language
            && identity.document == request.document
            && identity.attempt == request.attempt
            && identity.rules == request.rules
            && identity.limits == request.limits
            && (!self.reason.revision_specific() || identity.revision == request.revision))
            .then_some(self.reason)
    }
}

struct SnapshotRequest {
    identity: Identity,
    job: Job,
    result: Option<Result<Response, Error>>,
}

struct Running {
    identity: Identity,
    view: ViewKey,
    cancel: Cancellation,
    task: JoinHandle<WorkerResult>,
}

struct ConfigTask {
    generation: u64,
    validation: bool,
    cancel: Cancellation,
    task: JoinHandle<Result<Arc<Config>, String>>,
}

/// Only one snapshot request, child, or retiring child exists at a time.
#[derive(Default)]
pub(super) struct State {
    identity: Option<Identity>,
    snapshot: Option<SnapshotRequest>,
    running: Option<Running>,
    parked: Option<(Identity, Session)>,
    retiring: Option<JoinHandle<()>>,
    directory: Option<PathBuf>,
    config: Arc<Config>,
    theme: Option<Arc<str>>,
    rules: u64,
    limits: crate::settings::SyntaxLimits,
    limit_generation: u64,
    requested: u64,
    started: u64,
    loader: Option<ConfigTask>,
    candidate: Option<(u64, Arc<Config>)>,
    notice: Option<Arc<str>>,
    storage_retry: Option<Instant>,
    process_epoch: u64,
}

impl State {
    /// A missing read root uses bundled configuration and never starts a loader.
    pub(super) fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            ..Self::default()
        }
    }

    /// Revoke stale jobs without changing user language or palette selections.
    pub(super) fn set_limits(&mut self, limits: crate::settings::SyntaxLimits) -> bool {
        if self.limits == limits {
            return false;
        }
        self.limits = limits;
        self.limit_generation = self.limit_generation.wrapping_add(1);
        self.cancel();
        true
    }

    /// Read the current immutable configuration for active or parked tab choices.
    pub(super) fn config(&self) -> Arc<Config> {
        self.config.clone()
    }
    /// Retain only the newest reload request and cancel superseded compilation.
    pub(super) fn reload(&mut self) {
        if self.directory.is_none() {
            return;
        }
        self.requested = self.requested.wrapping_add(1);
        self.candidate = None;
        if let Some(task) = &self.loader {
            task.cancel.cancel();
        }
    }

    /// Theme choice is independent of the live syntax process.
    pub(super) fn theme(&mut self, name: Arc<str>) {
        if self.config.palette(&name).is_some() {
            self.theme = Some(name);
        }
    }

    /// Receive a matching completion even if it arrives during a foreground storage request.
    pub(super) fn receive(&mut self, completion: document::Completion) {
        if let Some(pending) = &mut self.snapshot
            && pending.job.id == completion.job
            && pending.job.document == completion.document
        {
            pending.result = Some(completion.result);
        }
    }

    /// Publish session-wide metadata along with every tab's screen.
    pub(super) fn screen(&self, screen: &mut super::Screen) {
        screen.syntax_config = self.config.clone();
        screen.syntax_notice = self.notice.clone();
        let theme = self
            .theme
            .as_deref()
            .or(self.config.default_theme.as_deref())
            .unwrap_or("Dark");
        if screen.syntax_theme.as_ref() != theme {
            screen.syntax_theme = Arc::from(theme);
        }
        screen.syntax_palette = self.config.palette(theme).unwrap_or_default();
    }

    /// Cancel stale work while allowing an in-progress initial parse to serve a newer viewport.
    fn cancel(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            snapshot.job.cancel();
        }
        if let Some(running) = &self.running {
            running.cancel.cancel();
        }
        if let Some((_, process)) = self.parked.take() {
            self.retire(process);
        }
    }

    /// A failed spawn drops its closure and reaps on the caller as a last resort.
    fn retire(&mut self, process: Session) {
        self.retiring = thread::Builder::new()
            .name("ede-syntax-cleanup".into())
            .spawn(move || drop(process))
            .ok();
    }

    /// Drive cancellation, ordered config reload, snapshot admission and projection publication.
    pub(super) fn poll(&mut self, editor: &mut Coordinator) -> bool {
        let control = editor.view_cancel.syntax.clone();
        let mut changed = false;
        let epoch = control.epoch();
        if self.process_epoch != epoch {
            self.process_epoch = epoch;
            self.cancel();
            if let Some(task) = &self.loader
                && task.validation
            {
                self.reload();
            }
            editor.tab.screen.syntax_projection = None;
            changed = true;
        }
        if let Some(handle) = self.retiring.take_if(|handle| handle.is_finished()) {
            let _ = handle.join();
        }
        if self.poll_config(&control) {
            for tab in &mut editor.tabs {
                if let Some(state) = &mut tab.state {
                    state.resolve_syntax(&self.config);
                }
            }
            changed = true;
        }
        editor.tab.resolve_syntax(&self.config);
        let screen = &mut editor.tab.screen;
        let desired = (|| {
            let info = editor.tab.info.as_ref()?;
            if screen.loading || control.paused() || !control.enabled() {
                return None;
            }
            Some(Identity {
                tab: screen.tab?,
                document: screen.document?,
                revision: info.revision,
                language: editor.tab.syntax_effective?,
                attempt: editor.tab.syntax_attempt,
                rules: self.rules,
                limits: self.limit_generation,
            })
        })();
        screen.syntax_epoch = epoch;
        if self.identity != desired {
            self.cancel();
            self.identity = desired;
            self.storage_retry = None;
            changed |= screen.syntax_projection.take().is_some();
        }

        let target = desired.and_then(|id| {
            screen
                .view
                .as_ref()
                .filter(|view| view.revision == id.revision)
                .map(|view| ViewKey::new(id.tab, id.document, view))
        });
        if screen
            .syntax_projection
            .as_ref()
            .is_some_and(|projection| Some(projection.key) != target)
        {
            screen.syntax_projection = None;
            changed = true;
        }
        if let Some(running) = self.running.take_if(|running| running.task.is_finished()) {
            let result = running.task.join().unwrap_or(Err(Fallback::Worker));
            let current = Some(running.identity) == desired && !running.cancel.is_cancelled();
            match result {
                Ok((session, projection)) if current => {
                    self.parked = Some((running.identity, session));
                    if Some(projection.key) == target {
                        screen.syntax_projection = Some(projection);
                        changed = true;
                    }
                }
                Ok((session, _)) => self.retire(session),
                Err(reason)
                    if current && (Some(running.view) == target || reason.revision_specific()) =>
                {
                    self.failed(editor, running.identity, reason);
                }
                Err(_) => {}
            }
        }

        let mut snapshot = None;
        if let Some(mut pending) = self.snapshot.take_if(|pending| pending.result.is_some()) {
            let result = pending.result.take().unwrap();
            if Some(pending.identity) == desired {
                match result {
                    Ok(Response::Snapshot(value))
                        if value.revision() == pending.identity.revision =>
                    {
                        snapshot = Some(value)
                    }
                    Ok(Response::Snapshot(_)) | Err(Error::Cancelled | Error::StaleRevision) => {}
                    Err(Error::Busy | Error::ResourceLimit(_)) => {
                        self.failed(editor, pending.identity, Fallback::Busy)
                    }
                    _ => self.failed(editor, pending.identity, Fallback::Read),
                }
            }
        }

        let reason = desired.and_then(|id| {
            editor
                .tab
                .syntax_failure
                .and_then(|failure| failure.for_request(id))
        });
        let pending_validation =
            self.candidate.is_some() || self.loader.as_ref().is_some_and(|task| task.validation);
        if !pending_validation
            && let (Some(id), Some(view)) = (desired, target)
            && reason.is_none()
            && editor.tab.screen.syntax_projection.is_none()
            && self.running.is_none()
            && self.snapshot.is_none()
            && self.retiring.is_none()
            && self.storage_retry.is_none_or(|time| Instant::now() >= time)
        {
            if editor
                .tab
                .info
                .as_ref()
                .is_some_and(|info| info.len > self.limits.document)
            {
                self.failed(editor, id, Fallback::Size);
            } else if snapshot.is_some() || self.parked.is_some() {
                self.dispatch(editor, id, view, snapshot.take());
            } else if let Some(live) = &editor.tab.live {
                if Instant::now() >= live.syntax_due
                    && let Some(snapshot) = &live.snapshot
                    && let Ok(snapshot) = snapshot.duplicate_live()
                {
                    self.dispatch(editor, id, view, Some(snapshot));
                }
            } else {
                self.admit(editor, id);
            }
        }

        let screen = &mut editor.tab.screen;
        let status = desired.map_or(Status::Plain, |id| {
            if let Some(reason) = editor
                .tab
                .syntax_failure
                .and_then(|failure| failure.for_request(id))
            {
                Status::Fallback(reason)
            } else if screen.syntax_projection.is_some() {
                Status::Ready
            } else {
                Status::Preparing
            }
        });
        if screen.syntax_status != status {
            screen.syntax_status = status;
            changed = true;
        }
        changed
    }

    /// Submit one asynchronous document lease, without waiting on the storage owner.
    fn admit(&mut self, editor: &mut Coordinator, identity: Identity) {
        editor.counter += 1;
        match editor.service.submit(
            editor.counter,
            Request::BackgroundSnapshot(identity.document),
        ) {
            Ok(job) => {
                self.snapshot = Some(SnapshotRequest {
                    identity,
                    job,
                    result: None,
                });
                self.storage_retry = None;
            }
            Err(error) if matches!(error.error, Error::Busy | Error::ResourceLimit(_)) => {
                self.failed(editor, identity, Fallback::Busy)
            }
            Err(_) => self.failed(editor, identity, Fallback::Read),
        }
    }

    /// Reserve space, prepare a compact descriptor, and release the view before dispatch.
    fn dispatch(
        &mut self,
        editor: &mut Coordinator,
        identity: Identity,
        key: ViewKey,
        snapshot: Option<document::Snapshot>,
    ) {
        let permit = match editor
            .service
            .reserve_interaction(crate::syntax::WORK_CHARGE)
        {
            Ok(permit) => permit,
            Err(_) => {
                self.failed(editor, identity, Fallback::Busy);
                return;
            }
        };
        let Some(runs) = editor
            .tab
            .screen
            .view
            .as_ref()
            .and_then(|view| Work::descriptors(view))
        else {
            self.failed(editor, identity, Fallback::Output);
            return;
        };
        let cancel = Cancellation::default();
        let work = Work {
            limits: self.limits,
            key,
            language: identity.language,
            config: self.config.clone(),
            cancel: cancel.clone(),
            control: editor.view_cancel.syntax.clone(),
            snapshot,
            session: self.parked.take().map(|(_, session)| session),
            permit,
        };
        match thread::Builder::new()
            .name("ede-syntax".into())
            .spawn(move || work.run(runs))
        {
            Ok(task) => {
                self.running = Some(Running {
                    identity,
                    view: key,
                    cancel,
                    task,
                })
            }
            Err(_) => self.failed(editor, identity, Fallback::Worker),
        }
    }

    /// Busy storage defers admission; other errors latch until their specified reset.
    fn failed(&mut self, editor: &mut Coordinator, identity: Identity, reason: Fallback) {
        match reason {
            Fallback::Cancelled => {}
            Fallback::Busy => {
                self.storage_retry = Some(Instant::now() + Duration::from_millis(100))
            }
            _ => {
                editor.tab.syntax_failure = Some(Failure { identity, reason });
                editor.tab.screen.syntax_projection = None;
                self.cancel();
            }
        }
    }

    /// A palette and association-only edit keeps the current process and tree.
    fn poll_config(&mut self, control: &Arc<ProcessControl>) -> bool {
        let mut changed = false;
        if let Some(job) = self.loader.take_if(|job| job.task.is_finished()) {
            let result = job
                .task
                .join()
                .unwrap_or_else(|_| Err("configuration loader failed".into()));
            if job.generation == self.requested {
                match result {
                    Ok(candidate) if self.config.same_rules(&candidate) || job.validation => {
                        if !self.config.same_rules(&candidate) {
                            self.rules = self.rules.wrapping_add(1);
                        }
                        self.config = candidate;
                        if self
                            .theme
                            .as_ref()
                            .is_some_and(|name| self.config.palette(name).is_none())
                        {
                            self.theme = None;
                        }
                        self.notice = None;
                        changed = true;
                    }
                    Ok(candidate) => {
                        self.candidate = Some((job.generation, candidate));
                        self.cancel();
                    }
                    Err(error) => {
                        self.notice = Some(Arc::from(error));
                        changed = true;
                    }
                }
            }
        }
        if self.loader.is_none()
            && self.started != self.requested
            && let Some(directory) = self.directory.clone()
        {
            self.started = self.requested;
            let task = thread::Builder::new()
                .name("ede-syntax-config".into())
                .spawn(move || crate::syntax::read_config(&directory).map(Arc::new));
            changed |= self.load_task(self.started, false, Cancellation::default(), task);
        } else if self.loader.is_none()
            && self.running.is_none()
            && self.retiring.is_none()
            && !control.paused()
            && let Some((generation, candidate)) = self.candidate.take()
        {
            let control = control.clone();
            let cancel = Cancellation::default();
            let stop = cancel.clone();
            let task = thread::Builder::new()
                .name("ede-syntax-validate".into())
                .spawn(move || crate::syntax::validate_config(candidate, control, stop));
            changed |= self.load_task(generation, true, cancel, task);
        }
        changed
    }

    fn load_task(
        &mut self,
        generation: u64,
        validation: bool,
        cancel: Cancellation,
        task: std::io::Result<JoinHandle<Result<Arc<Config>, String>>>,
    ) -> bool {
        match task {
            Ok(task) => {
                self.loader = Some(ConfigTask {
                    generation,
                    task,
                    validation,
                    cancel,
                });
                false
            }
            Err(_) => {
                self.notice = Some(Arc::from(
                    "unable to start highlighting configuration worker",
                ));
                true
            }
        }
    }

    /// Stop admitted work and reclaim child and leases on session shutdown.
    pub(super) fn shutdown(&mut self, control: &ProcessControl) {
        control.pause();
        self.cancel();
        if let Some(task) = self.running.take() {
            let _ = task.task.join();
        }
        if let Some(task) = self.retiring.take() {
            let _ = task.join();
        }
        if let Some(task) = self.loader.take() {
            task.cancel.cancel();
            if task.validation || task.task.is_finished() {
                let _ = task.task.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::OpenOptions, editor::Action, syntax::Choice};
    use std::sync::mpsc;

    #[test]
    fn old_viewport_error_cannot_latch_on_resized_view() {
        let mut editor = crate::editor::tests::coordinator(OpenOptions::default());
        Arc::get_mut(&mut editor.view_cancel).unwrap().syntax =
            Arc::new(ProcessControl::new(Some(PathBuf::from("/bin/false"))));
        let language = Choice::options()
            .find_map(|choice| match choice {
                Choice::Syntax(language) if language.id == "rust" => Some(language),
                _ => None,
            })
            .unwrap();
        let config = Config::default();
        editor.tab.screen.syntax_language = Choice::Syntax(language);
        editor.tab.resolve_syntax(&config);
        let identity = Identity {
            tab: editor.tab.screen.tab.unwrap(),
            document: editor.tab.screen.document.unwrap(),
            revision: editor.tab.info.as_ref().unwrap().revision,
            language,
            attempt: editor.tab.syntax_attempt,
            rules: 0,
            limits: 0,
        };
        let view = ViewKey::new(
            identity.tab,
            identity.document,
            editor.tab.screen.view.as_ref().unwrap(),
        );
        let (release, wait) = mpsc::sync_channel(0);
        let job = thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            Err(Fallback::Output)
        });
        let mut state = State {
            identity: Some(identity),
            running: Some(Running {
                identity,
                view,
                cancel: Cancellation::default(),
                task: job,
            }),
            ..State::default()
        };
        editor
            .action(Action::Size {
                width: 40,
                height: 8,
                generation: 2,
                search_cover: 0,
            })
            .unwrap();
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !state.running.as_ref().unwrap().task.is_finished() {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        state.poll(&mut editor);
        assert_eq!(editor.tab.screen.syntax_status, Status::Preparing);
        assert!(editor.tab.syntax_failure.is_none());
    }

    #[test]
    fn stricter_limits_revoke_a_held_timeout_and_relaxing_retries_the_revision() {
        let mut editor = crate::editor::tests::coordinator(OpenOptions::default());
        Arc::get_mut(&mut editor.view_cancel).unwrap().syntax =
            Arc::new(ProcessControl::new(Some(PathBuf::from("/bin/false"))));
        editor
            .action(Action::Insert("fn example() {}".into()))
            .unwrap();
        let language = Choice::options()
            .find_map(|choice| match choice {
                Choice::Syntax(language) if language.id == "rust" => Some(language),
                _ => None,
            })
            .unwrap();
        editor.tab.screen.syntax_language = Choice::Syntax(language);
        editor.tab.resolve_syntax(&Config::default());
        let identity = Identity {
            tab: editor.tab.screen.tab.unwrap(),
            document: editor.tab.screen.document.unwrap(),
            revision: editor.tab.info.as_ref().unwrap().revision,
            language,
            attempt: editor.tab.syntax_attempt,
            rules: 0,
            limits: 0,
        };
        let view = ViewKey::new(
            identity.tab,
            identity.document,
            editor.tab.screen.view.as_ref().unwrap(),
        );
        let (release, held) = mpsc::sync_channel(0);
        let task = thread::spawn(move || {
            held.recv_timeout(Duration::from_secs(3)).unwrap();
            Err(Fallback::Timeout)
        });
        let mut state = State {
            identity: Some(identity),
            running: Some(Running {
                identity,
                view,
                cancel: Cancellation::default(),
                task,
            }),
            ..State::default()
        };
        let limits = crate::settings::SyntaxLimits {
            document: 1,
            ..Default::default()
        };
        assert!(state.set_limits(limits));
        assert!(state.running.as_ref().unwrap().cancel.is_cancelled());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !state.running.as_ref().unwrap().task.is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        state.poll(&mut editor);
        assert_eq!(
            editor.tab.screen.syntax_status,
            Status::Fallback(Fallback::Size)
        );
        assert_eq!(editor.tab.screen.syntax_language, Choice::Syntax(language));
        assert!(state.set_limits(Default::default()));
        state.poll(&mut editor);
        assert_eq!(editor.tab.screen.syntax_status, Status::Preparing);
    }

    #[test]
    fn superseded_reload_cannot_publish_after_new_request() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = State::new(Some(directory.path().to_owned()));
        let matching = state.config.clone();
        let stale = Arc::new(Config::default());
        let (release, wait) = mpsc::sync_channel(0);
        let task = thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(stale)
        });
        state.reload();
        state.loader = Some(ConfigTask {
            generation: state.requested,
            validation: true,
            cancel: Cancellation::default(),
            task,
        });
        state.reload();
        state.started = state.requested;
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !state.loader.as_ref().unwrap().task.is_finished() {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        assert!(!state.poll_config(&Arc::new(ProcessControl::default())));
        assert!(Arc::ptr_eq(&matching, &state.config));
    }
}
