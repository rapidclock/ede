//! Read-only tab capabilities and serialized transfer to scratch ownership at EOF.

use super::*;
use crate::stream::{self, Phase, Source};

/// Small display state independent of storage ownership or the file descriptor.
#[derive(Clone)]
pub(crate) struct StreamStatus {
    pub following: bool,
    pub unseen: bool,
    pub retained: u64,
    pub generations: u64,
    pub boundary: Option<&'static str>,
    pub phase: &'static str,
    pub detail: Option<String>,
    pub stopped: bool,
    pub retry: bool,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self {
            following: true,
            unseen: false,
            retained: 0,
            generations: 1,
            boundary: None,
            phase: "Opening stream",
            detail: None,
            stopped: false,
            retry: false,
        }
    }
}

pub(super) struct LiveTab {
    reader: stream::Reader,
    pub snapshot: Option<Snapshot>,
    adoption: Option<document::CaptureAdoption>,
    draw_after: std::time::Instant,
    protected_path: Option<PathBuf>,
    pub syntax_due: std::time::Instant,
}

/// Publication may already have committed when cancellation arrives. Replay a
/// pending Close/Quit only after reporting the actual result.
pub(super) struct Export {
    pub tab: TabId,
    worker: Option<JoinHandle<document::SaveReport>>,
    cancel: Cancellation,
    deferred: Option<(Action, u64)>,
}

impl Export {
    pub(super) fn defer(&mut self, action: Action, sequence: u64) {
        self.cancel.cancel();
        self.deferred = Some((action, sequence));
    }

    pub(super) fn cancel_deferred(&mut self) {
        self.deferred = None;
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Coordinator {
    pub(super) fn export_retained(&mut self, tab: TabId, path: PathBuf) -> Result<(), Error> {
        if self.export.is_some() {
            return Err(Error::Busy);
        }
        let state = self
            .state(tab)
            .filter(|state| {
                state
                    .screen
                    .stream
                    .as_ref()
                    .is_some_and(|status| status.stopped)
            })
            .ok_or(Error::ReadOnly)?;
        let source = state.screen.path.clone();
        let protected_path = state
            .live
            .as_ref()
            .and_then(|live| live.protected_path.clone());
        let snapshot = state
            .live
            .as_ref()
            .and_then(|live| live.snapshot.as_ref())
            .ok_or(Error::Busy)?
            .duplicate_live()?;
        let path = absolute(&self.launch_directory, &path);
        let cancel = Cancellation::default();
        *self
            .view_cancel
            .export
            .lock()
            .map_err(|_| Error::CorruptStorage)? = Some(cancel.clone());
        let token = cancel.clone();
        let worker = thread::Builder::new()
            .name("ede-export".into())
            .spawn(move || {
                snapshot.export_retained(
                    path,
                    [source.as_deref(), protected_path.as_deref()],
                    &token,
                )
            })?;
        self.export = Some(Export {
            tab,
            worker: Some(worker),
            cancel,
            deferred: None,
        });
        Ok(())
    }

    pub(super) fn poll_export(&mut self) -> Result<bool, Error> {
        let Some(mut export) = self.export.take_if(|export| {
            export
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
        }) else {
            return Ok(false);
        };
        let deferred = export.deferred.take();
        if let Ok(mut cancellation) = self.view_cancel.export.lock() {
            cancellation.take();
        }
        let result = export.worker.take().expect("finished export").join();
        let text = match result {
            Ok(report) => match report.outcome {
                SaveOutcome::Saved => "Exported retained copy".into(),
                SaveOutcome::NotCommitted => format!(
                    "Export did not publish: {}",
                    report.error.unwrap_or(Error::CorruptStorage)
                ),
                SaveOutcome::CommittedDurabilityUncertain => format!(
                    "Export published; durability uncertain: {}",
                    report.error.unwrap_or(Error::CorruptStorage)
                ),
            },
            Err(_) => "Export worker stopped; publication may have occurred".into(),
        };
        self.event(Event::Exported(text));
        if let Some((action, sequence)) = deferred {
            let current = std::mem::replace(&mut self.active_sequence, sequence);
            let result = self.action(action);
            self.active_sequence = current;
            result?;
        }
        Ok(true)
    }
    /// Source admission uses the same tab budget, with a separate limit on actual readers.
    pub(super) fn add_stream(&mut self, source: Source, focus: bool) -> Result<(), Error> {
        let path = match &source {
            Source::Stdin(_) => None,
            Source::Follow(path) => Some(absolute(&self.launch_directory, path)),
        };
        let source = match path.as_ref() {
            Some(path) => Source::Follow(path.clone()),
            None => source,
        };
        let id = self.admit_tab(path.clone())?;
        self.view_cancel.streamed.store(true, Ordering::Release);
        let reader =
            match stream::Reader::spawn(source, self.service.captures(), &self.reader_slots) {
                Ok(reader) => reader,
                Err(error) => {
                    self.remove_tab(id)?;
                    return Err(error);
                }
            };
        if path.is_none() {
            self.label_tab(id, Arc::from("Stdin"));
        }
        let state = self.state_mut(id).expect("new source tab");
        state.capability = crate::search::Capability::ReadOnly;
        state.input.read_only.store(true, Ordering::Release);
        state.screen.can_replace = false;
        state.screen.stream = Some(StreamStatus::default());
        state.screen.message = "Opening read-only stream".into();
        state.live = Some(LiveTab {
            reader,
            snapshot: None,
            adoption: None,
            draw_after: std::time::Instant::now(),
            protected_path: None,
            syntax_due: std::time::Instant::now(),
        });
        if focus {
            self.switch_tab(id)?;
        }
        Ok(())
    }

    /// Consume at most the newest prepared revision of each source once per frame interval.
    pub(super) fn poll_streams(&mut self) -> Result<bool, Error> {
        if self.quit_ready {
            return Ok(false);
        }
        let mut changed = false;
        let now = std::time::Instant::now();
        for index in 0..self.tabs.len() {
            let id = self.tabs[index].id;
            if self.search.holds_stream(id) {
                continue;
            }
            let Some(live) = self.state_mut(id).and_then(|state| state.live.as_mut()) else {
                continue;
            };
            if now < live.draw_after {
                continue;
            }
            let update = live.reader.latest();
            if update.snapshot.is_none()
                && update.phase.is_none()
                && update.boundary.is_none()
                && update.protected_path.is_none()
            {
                continue;
            }
            live.draw_after = now + Duration::from_millis(32);
            if update.protected_path.is_some() {
                live.protected_path = update.protected_path;
            }
            changed = true;
            if let Some(phase) = &update.phase
                && let Phase::OpenFailed(reason) = phase
            {
                let startup = self.startup_tab(id);
                let label = self
                    .state(id)
                    .and_then(|state| state.screen.path.as_deref())
                    .map_or_else(
                        || "Stdin".into(),
                        |path| crate::text::safe(&path.to_string_lossy()),
                    );
                self.remove_tab(id)?;
                self.event(Event::OpenFailed(format!("{label}: {reason}")));
                if startup {
                    self.restore_startup()?;
                }
                break;
            }
            let active = self.tab.screen.tab == Some(id);
            if let Some(snapshot) = update.snapshot {
                self.publish_extent(id, snapshot)?;
            }
            if let Some((generations, reason)) = update.boundary {
                let status = self.state_mut(id).unwrap().screen.stream.as_mut().unwrap();
                status.generations = generations;
                status.boundary = Some(reason);
            }
            let mut adoption = false;
            if let Some(phase) = update.phase {
                let state = self.state_mut(id).expect("source is still open");
                let status = state.screen.stream.as_mut().unwrap();
                status.detail = None;
                match phase {
                    Phase::Reading(warning) => {
                        status.phase = "Receiving";
                        status.detail = warning;
                    }
                    Phase::Waiting(warning) => {
                        status.phase = "Waiting for more";
                        status.detail = warning;
                    }
                    Phase::Finishing => status.phase = "Finishing stdin",
                    Phase::Failed(reason) => {
                        status.phase = "Incomplete capture";
                        status.stopped = true;
                        status.detail = Some(reason);
                    }
                    Phase::FinishFailed(reason) => {
                        status.phase = "Finish failed";
                        status.stopped = true;
                        status.retry = true;
                        status.detail = Some(reason);
                    }
                    Phase::Finished(prepared) => {
                        state.live.as_mut().unwrap().adoption = Some(prepared);
                        adoption = true;
                    }
                    Phase::OpenFailed(_) => unreachable!("handled above"),
                }
            }
            if adoption {
                self.finish_stream(id)?;
            }
            if let Some(state) = self.state_mut(id) {
                state.describe_stream();
            }
            if active && self.tab.screen.document.is_some() {
                self.layout(false)?;
            }
        }
        Ok(changed)
    }

    fn publish_extent(&mut self, id: TabId, snapshot: Snapshot) -> Result<(), Error> {
        let active = self.tab.screen.tab == Some(id);
        let state = self
            .state_mut(id)
            .expect("extent belongs to an admitted tab");
        let live = state.live.as_mut().expect("live extent");
        let old = live.snapshot.as_ref().map(|s| (s.revision(), s.len()));
        live.syntax_due = std::time::Instant::now() + Duration::from_millis(120);
        let status = state.screen.stream.as_mut().expect("stream status");
        let len = snapshot.len();
        status.retained = len + u64::from(snapshot.format().bom) * 3;
        let position = &mut state.screen.position;
        if status.following {
            position.move_to(len, false);
        } else if let Some((_, previous_len)) = old
            && len > previous_len
        {
            status.unseen = true;
            let cancel = Cancellation::default();
            let mut text = Text::new(&snapshot, &cancel);
            if position.caret == previous_len {
                position.caret = text.ceil(position.caret).unwrap_or(len);
            }
            if position.anchor == previous_len {
                position.anchor = text.ceil(previous_len).unwrap_or(len);
            }
        }
        state.screen.document = Some(snapshot.document());
        state.screen.loading = false;
        state.info = Some(DocumentInfo {
            revision: snapshot.revision(),
            dirty: false,
            len,
            lines: snapshot.line_count(),
            path: state.screen.path.clone(),
            history: 0,
            history_begin: 0,
            compaction_pending: false,
            file_observation: None,
        });
        let revision = snapshot.revision();
        live.snapshot = Some(snapshot);
        if active && let Some((previous, bytes)) = old {
            self.cache.edited(previous, revision, bytes);
        }
        self.search.stream_advanced(id, revision);
        Ok(())
    }

    /// Install the disk-backed result only while this tab still owns this source.
    fn finish_stream(&mut self, id: TabId) -> Result<(), Error> {
        let Some(prepared) = self
            .state(id)
            .and_then(|s| s.live.as_ref())
            .and_then(|live| live.adoption.clone())
        else {
            return Ok(());
        };
        let active = self.tab.screen.tab == Some(id);
        let prior_active = self
            .retention
            .as_ref()
            .and_then(|retention| retention.active);
        let outcome = (|| {
            let scratch = self.fresh_scratch()?;
            if !active && let Some(retention) = &mut self.retention {
                retention.active = prior_active;
            }
            let Response::Ready { document, revision } =
                self.call_background(Request::Adopt(prepared))?
            else {
                return Err(Error::CorruptStorage);
            };
            let info = self.read_info(document);
            Ok::<_, Error>((document, revision, info, scratch))
        })();
        let state = self
            .state_mut(id)
            .expect("adoption is serialized on the tab owner");
        match outcome {
            Ok((document, revision, info, scratch)) => {
                let prior = state.info.as_ref().map(|info| (info.revision, info.len));
                state.live = None;
                state.screen.stream = None;
                state.capability = crate::search::Capability::Editable;
                state.scratch = scratch;
                state.screen.document = Some(document);
                state.input.read_only.store(false, Ordering::Release);
                state.screen.can_replace = true;
                state.screen.message = "Editable scratch".into();
                if let Ok(info) = info {
                    state.apply_info(info);
                } else if let Some(info) = &mut state.info {
                    info.revision = revision;
                    info.dirty = true;
                    state.screen.modified = info.len != 0;
                }
                if active && let Some((old, len)) = prior {
                    self.cache.edited(old, revision, len);
                }
                self.search.stream_promoted(id);
                Ok(())
            }
            Err(error) => {
                let status = state.screen.stream.as_mut().unwrap();
                status.phase = "Finish failed";
                status.stopped = true;
                status.retry = true;
                status.detail = Some(error.to_string());
                Ok(())
            }
        }
    }

    pub(super) fn retry_stream_finish(&mut self, id: TabId) -> Result<(), Error> {
        if let Some(live) = self.state(id).and_then(|s| s.live.as_ref()) {
            if live.adoption.is_some() {
                self.finish_stream(id)?;
                if self.tab.screen.tab == Some(id) {
                    self.layout(false)?;
                }
            } else {
                live.reader.retry();
            }
        }
        Ok(())
    }

    pub(super) fn stop_streams(&mut self) {
        self.export = None;
        self.tab.live = None;
        for tab in &mut self.tabs {
            if let Some(state) = &mut tab.state {
                state.live = None;
            }
        }
    }

    pub(super) fn background_snapshot(
        &mut self,
        id: TabId,
        document: DocumentId,
    ) -> Result<Snapshot, Error> {
        if let Some(snapshot) = self
            .state(id)
            .and_then(|s| s.live.as_ref())
            .and_then(|live| live.snapshot.as_ref())
        {
            return snapshot.duplicate_live();
        }
        match self.call_background(Request::BackgroundSnapshot(document))? {
            Response::Snapshot(snapshot) => Ok(snapshot),
            _ => Err(Error::CorruptStorage),
        }
    }
}

impl TabState {
    pub(super) fn pause_follow(&mut self) {
        if let Some(status) = &mut self.screen.stream {
            status.following = false;
            self.describe_stream();
        }
    }

    pub(super) fn describe_stream(&mut self) {
        if let Some(status) = &self.screen.stream {
            let view = if status.following {
                "Following"
            } else if status.unseen {
                "Paused · new text"
            } else {
                "Paused"
            };
            self.screen.message = if status.phase == "Receiving" {
                format!("{view} | {} bytes · read-only", status.retained)
            } else {
                format!(
                    "{view} | {} · {} bytes · read-only",
                    status.phase, status.retained
                )
            };
            if let Some(detail) = &status.detail {
                self.screen.message.push_str(&format!(" | {detail}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Write, time::Instant};

    #[test]
    fn eof_cannot_inherit_stream_discard_or_queued_replacement_authority() {
        let mut owner = super::super::tests::coordinator(OpenOptions::default());
        owner.action(Action::Insert("one".into())).unwrap();
        let companion = owner.tab.screen.tab.unwrap();
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut writer = File::from(write);
        owner
            .add_stream(Source::Stdin(File::from(read)), true)
            .unwrap();
        writer.write_all(b"one").unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while owner.tab.info.as_ref().is_none_or(|info| info.len != 3) {
            owner.poll_streams().unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }

        let stream = owner.tab.screen.tab.unwrap();
        let scope = owner.view_cancel.replacement_scope();
        let close = After::Close(stream);
        owner.action(Action::Guard(close)).unwrap();
        assert_eq!(owner.stream_guard, Some(close));
        drop(writer);
        while owner.tab.live.is_some() {
            owner.poll_streams().unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        owner.action(Action::Discard(close, None)).unwrap();
        assert!(
            owner.state(stream).is_some(),
            "a stream confirmation discarded the scratch"
        );

        owner
            .action(Action::Search(search::SearchRequest {
                replacement_scope: Some(scope),
                navigation: None,
                query: crate::search::Query {
                    literal: "one".into(),
                    ..Default::default()
                },
                replacement: "two".into(),
                all: true,
                operation: search::Operation::ReplaceAll,
            }))
            .unwrap();
        while owner.search.status.busy {
            owner.poll_search().unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(super::super::tests::contents(&mut owner), "one");
        assert!(
            owner
                .search
                .status
                .outcomes
                .iter()
                .any(|outcome| outcome.tab == stream && outcome.message.contains("read-only"))
        );
        owner.switch_tab(companion).unwrap();
        assert_eq!(super::super::tests::contents(&mut owner), "two");
    }

    #[test]
    fn cancel_after_quit_readiness_keeps_the_source_and_unfreezes_publication() {
        let mut owner = super::super::tests::coordinator(OpenOptions::default());
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut writer = File::from(write);
        owner
            .add_stream(Source::Stdin(File::from(read)), true)
            .unwrap();
        writer.write_all(b"old").unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while owner.tab.info.as_ref().is_none_or(|info| info.len != 3) {
            owner.poll_streams().unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }

        owner.action(Action::FinishQuit).unwrap();
        writer.write_all(b" plus").unwrap();
        assert!(
            owner.tab.live.is_some(),
            "QuitReady revoked the producer before UI acceptance"
        );
        assert!(
            !owner.poll_streams().unwrap(),
            "QuitReady must freeze publication"
        );
        assert_eq!(owner.tab.info.as_ref().unwrap().len, 3);
        owner.action(Action::CancelGuard).unwrap();
        while owner.tab.info.as_ref().unwrap().len != 8 {
            owner.poll_streams().unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
    }
}
