//! One detached settings operation with a coalesced reload and explicit publication authority.

use super::{Base, Location, Revision, file};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveState {
    Staging,
    Finishing,
    Cancelled,
}

const STAGING: u8 = 0;
const GRANTED: u8 = 1;
const CANCELLED: u8 = 2;

#[derive(Debug, Default)]
struct Grant(AtomicU8);

impl Grant {
    fn grant(&self) -> bool {
        self.0
            .compare_exchange(STAGING, GRANTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn cancel(&self) -> bool {
        self.0
            .compare_exchange(STAGING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn state(&self) -> SaveState {
        match self.0.load(Ordering::Acquire) {
            STAGING => SaveState::Staging,
            GRANTED => SaveState::Finishing,
            _ => SaveState::Cancelled,
        }
    }
}

type Reply = Result<(Revision, Base), file::Failure>;

#[derive(Debug)]
enum Operation {
    Read { obsolete: bool },
    Write(Arc<Grant>),
}

#[derive(Debug)]
struct Task {
    operation: Operation,
    worker: JoinHandle<Reply>,
}

/// Outcome reconciled with the Settings draft before accepting another key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Update {
    ReadApplied,
    WriteApplied,
    Diagnostic,
    Cancelled,
}

/// Main-thread revision and settings file scheduling. Dropping never joins I/O.
#[derive(Debug, Default)]
pub(crate) struct Service {
    pub applied: Revision,
    pub base: Option<Base>,
    pub user_config: bool,
    pub diagnostic: Option<String>,
    pub starting: bool,
    queued_reload: bool,
    task: Option<Task>,
    ready: Option<(Update, Revision, Base)>,
}

impl Service {
    /// Scheduling a read performs no filesystem I/O.
    pub fn startup(&mut self, location: &Location) {
        if location.user_directory().is_some() {
            self.starting = true;
            self.reload();
        }
    }

    /// A later read revokes any earlier read and waits for an admitted write.
    pub fn reload(&mut self) {
        self.queued_reload = true;
        if self
            .ready
            .as_ref()
            .is_some_and(|(update, _, _)| *update == Update::ReadApplied)
        {
            self.ready = None;
        }
        if let Some(Task {
            operation: Operation::Read { obsolete },
            ..
        }) = &mut self.task
        {
            *obsolete = true;
        }
    }

    /// Escape the initial read with defaults and no verified save base.
    pub fn bypass(&mut self) {
        if self.starting {
            self.reload();
            self.queued_reload = false;
            self.starting = false;
        }
    }

    /// Starting a write transfers one immutable draft and its original base.
    pub fn save(&mut self, location: &Location, base: Base, draft: Revision) -> Result<(), String> {
        let root = location
            .user_directory()
            .ok_or("Settings are read-only in this session")?
            .to_owned();
        if self.base.as_ref() != Some(&base) {
            return Err("The settings base has changed. Reload settings before saving.".into());
        }
        if self.task.is_some() || self.queued_reload || self.ready.is_some() {
            return Err("Busy: a settings operation is still running".into());
        }
        let grant = Arc::new(Grant::default());
        let authority = grant.clone();
        let worker = spawn(move || file::save(&root, &base, draft, || authority.grant()))
            .map_err(|error| error.to_string())?;
        self.task = Some(Task {
            operation: Operation::Write(grant),
            worker,
        });
        Ok(())
    }

    /// Show pending writes, including an irreversible publication in progress.
    pub fn save_state(&self) -> Option<SaveState> {
        if self
            .ready
            .as_ref()
            .is_some_and(|(update, _, _)| *update == Update::WriteApplied)
        {
            return Some(SaveState::Finishing);
        }
        match self.task.as_ref().map(|task| &task.operation) {
            Some(Operation::Write(grant)) => Some(grant.state()),
            _ => None,
        }
    }

    /// Cancel has no authority after the worker acquired the publication grant.
    pub fn cancel_save(&self) -> bool {
        match self.task.as_ref().map(|task| &task.operation) {
            Some(Operation::Write(grant)) => grant.cancel(),
            _ => false,
        }
    }

    /// Poll one completion and then start the latest pending reload.
    pub fn poll(&mut self, location: &Location) -> Option<Update> {
        if self.ready.is_some() {
            return None;
        }
        if let Some(task) = self.task.take_if(|task| task.worker.is_finished()) {
            let result = task.worker.join().unwrap_or_else(|_| {
                Err(file::Failure {
                    message: "Settings worker stopped unexpectedly".into(),
                    uncertain: matches!(&task.operation, Operation::Write(grant) if grant.state() == SaveState::Finishing),
                })
            });
            // Report a definitive completion before admitting a queued reload.
            return self.complete(task.operation, result);
        }
        if self.task.is_none() && self.queued_reload {
            self.queued_reload = false;
            if let Some(root) = location.user_directory().map(PathBuf::from) {
                match spawn(move || file::load(&root)) {
                    Ok(worker) => {
                        self.task = Some(Task {
                            operation: Operation::Read { obsolete: false },
                            worker,
                        })
                    }
                    Err(error) => {
                        self.starting = false;
                        self.diagnostic = Some(format!("Unable to start settings worker: {error}"));
                        return Some(Update::Diagnostic);
                    }
                }
            }
        }
        None
    }

    fn complete(&mut self, operation: Operation, result: Reply) -> Option<Update> {
        let applied = match operation {
            Operation::Read { obsolete: true } => return None,
            Operation::Read { obsolete: false } => Update::ReadApplied,
            Operation::Write(grant) if grant.state() == SaveState::Cancelled => {
                return Some(Update::Cancelled);
            }
            Operation::Write(_) => Update::WriteApplied,
        };
        match result {
            Ok((revision, base)) => {
                self.ready = Some((applied, revision, base));
                None
            }
            Err(error) => {
                self.starting = false;
                if error.uncertain {
                    self.base = None;
                }
                self.diagnostic = Some(error.message);
                Some(Update::Diagnostic)
            }
        }
    }

    /// The terminal owner admits these values ahead of further actions before publishing the map.
    pub fn ready_values(&self) -> Option<super::Values> {
        self.ready.as_ref().map(|(_, revision, _)| revision.values)
    }

    /// Make a successful and ordered revision visible in the terminal owner.
    pub fn apply_ready(&mut self) -> Option<Update> {
        let (update, revision, base) = self.ready.take()?;
        self.starting = false;
        self.applied = revision;
        self.user_config = base.present();
        self.base = Some(base);
        self.diagnostic = None;
        Some(update)
    }

    /// Serialize exit with publication. A stalled worker is always left detached.
    pub fn exit(&mut self, location: &Location) -> Option<String> {
        self.queued_reload = false;
        let writing = self
            .save_state()
            .is_some_and(|state| state != SaveState::Cancelled);
        let report = self.poll(location).or_else(|| self.apply_ready());
        self.cancel_save();
        match report {
            Some(Update::WriteApplied) => Some("Settings saved".into()),
            Some(Update::Diagnostic) if writing => self.diagnostic.clone(),
            _ => None,
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.cancel_save();
    }
}

fn spawn(task: impl FnOnce() -> Reply + Send + 'static) -> std::io::Result<JoinHandle<Reply>> {
    thread::Builder::new()
        .name("ede-settings".into())
        .spawn(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Scalar;
    use std::{
        fs,
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn fixture() -> (tempfile::TempDir, Location) {
        let root = tempfile::tempdir().unwrap();
        let location = Location {
            directory: Some(root.path().to_owned()),
            safe: false,
        };
        (root, location)
    }

    fn wait_finished(service: &Service) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !service.task.as_ref().unwrap().worker.is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn bypass_and_superseded_reads_cannot_publish_values_or_diagnostics() {
        for stale_success in [true, false] {
            let (root, location) = fixture();
            let mut service = Service::default();
            let mut old = Revision::default();
            old.values.set(Scalar::Checkpoint, Some(22));
            let (release, held) = mpsc::sync_channel(0);
            service.starting = true;
            service.task = Some(Task {
                operation: Operation::Read { obsolete: false },
                worker: spawn(move || {
                    held.recv_timeout(Duration::from_secs(3)).unwrap();
                    if stale_success {
                        Ok((old, Base::Missing))
                    } else {
                        Err(file::Failure {
                            message: "obsolete error".into(),
                            uncertain: false,
                        })
                    }
                })
                .unwrap(),
            });

            service.bypass();
            fs::write(
                root.path().join("config.toml"),
                "[recovery]\ncheckpoint_interval_seconds = 7",
            )
            .unwrap();
            service.reload();
            release.send(()).unwrap();
            wait_finished(&service);
            assert_eq!(service.poll(&location), None);
            assert!(
                service.ready_values().is_none(),
                "revoked initial read offered a revision"
            );
            assert!(
                service.diagnostic.is_none(),
                "revoked initial read published a diagnostic"
            );
            assert!(service.base.is_none());
            assert_eq!(service.applied.values.get(Scalar::Checkpoint), 5);

            assert_eq!(service.poll(&location), None);
            wait_finished(&service);
            service.poll(&location);
            assert_eq!(
                service.applied.values.get(Scalar::Checkpoint),
                5,
                "before editor admission"
            );
            assert_eq!(service.ready_values().unwrap().get(Scalar::Checkpoint), 7);
            assert_eq!(service.apply_ready(), Some(Update::ReadApplied));
            assert_eq!(service.applied.values.get(Scalar::Checkpoint), 7);
            assert!(service.user_config);
        }
    }

    #[test]
    fn explicit_reload_waits_for_write_and_reports_both_revisions_in_order() {
        let (root, location) = fixture();
        let mut service = Service::default();
        let mut candidate = Revision::default();
        candidate.values.set(Scalar::Checkpoint, Some(13));
        let path = root.path().to_owned();
        let (release, held) = mpsc::sync_channel(0);
        let grant = Arc::new(Grant::default());
        let permit = grant.clone();
        service.task = Some(Task {
            operation: Operation::Write(grant),
            worker: spawn(move || {
                file::save(&path, &Base::Missing, candidate, || {
                    held.recv_timeout(Duration::from_secs(3)).unwrap();
                    permit.grant()
                })
            })
            .unwrap(),
        });
        service.reload();
        assert_eq!(service.poll(&location), None);
        assert_eq!(service.save_state(), Some(SaveState::Staging));
        release.send(()).unwrap();
        wait_finished(&service);
        assert_eq!(service.poll(&location), None);
        assert_eq!(service.apply_ready(), Some(Update::WriteApplied));
        assert_eq!(service.applied.values.get(Scalar::Checkpoint), 13);
        assert_eq!(service.poll(&location), None);
        wait_finished(&service);
        service.poll(&location);
        assert_eq!(service.apply_ready(), Some(Update::ReadApplied));
        assert_eq!(service.applied.values.get(Scalar::Checkpoint), 13);
    }

    #[test]
    fn save_cancel_or_shutdown_serializes_against_publication_grant() {
        for granted_first in [false, true] {
            for shutdown in [false, true] {
                let (root, location) = fixture();
                let mut service = Service::default();
                let grant = Arc::new(Grant::default());
                let authority = grant.clone();
                let path = root.path().to_owned();
                let (staged, at_grant) = mpsc::sync_channel(0);
                let (release, held) = mpsc::sync_channel(0);
                service.task = Some(Task {
                    operation: Operation::Write(grant),
                    worker: spawn(move || {
                        file::save(&path, &Base::Missing, Revision::default(), || {
                            let authorized = granted_first && authority.grant();
                            staged.send(()).unwrap();
                            held.recv_timeout(Duration::from_secs(3)).unwrap();
                            if granted_first {
                                authorized
                            } else {
                                authority.grant()
                            }
                        })
                    })
                    .unwrap(),
                });
                at_grant.recv_timeout(Duration::from_secs(3)).unwrap();
                if shutdown {
                    assert!(
                        service.exit(&location).is_none(),
                        "exit must leave a held worker detached"
                    );
                } else {
                    assert_eq!(service.cancel_save(), !granted_first);
                }
                assert_eq!(
                    service.save_state(),
                    Some(if granted_first {
                        SaveState::Finishing
                    } else {
                        SaveState::Cancelled
                    })
                );
                release.send(()).unwrap();
                wait_finished(&service);
                assert_eq!(
                    root.path().join("config.toml").exists(),
                    granted_first,
                    "publication violated grant ordering"
                );
                if shutdown {
                    assert_eq!(
                        service.exit(&location).as_deref(),
                        granted_first.then_some("Settings saved")
                    );
                } else {
                    assert_eq!(
                        service.poll(&location),
                        (!granted_first).then_some(Update::Cancelled)
                    );
                    assert_eq!(
                        service.apply_ready(),
                        granted_first.then_some(Update::WriteApplied)
                    );
                }
            }
        }
    }

    #[test]
    fn uncertain_publication_keeps_old_values_and_revokes_the_save_base() {
        let (_root, location) = fixture();
        let mut service = Service::default();
        service.applied.values.set(Scalar::Checkpoint, Some(3));
        service.user_config = true;
        service.base = Some(Base::Missing);
        let grant = Arc::new(Grant::default());
        assert!(grant.grant());
        assert_eq!(
            service.complete(
                Operation::Write(grant),
                Err(file::Failure {
                    uncertain: true,
                    message: "reload required".into(),
                })
            ),
            Some(Update::Diagnostic)
        );
        assert_eq!(service.applied.values.get(Scalar::Checkpoint), 3);
        assert!(service.user_config);
        assert!(service.base.is_none());
        assert!(
            service
                .save(&location, Base::Missing, Revision::default())
                .unwrap_err()
                .contains("Reload")
        );
    }
}
