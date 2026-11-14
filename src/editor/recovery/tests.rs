//! Synchronized lifecycle checks exercise the same coordinator and storage workers as the TUI.

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
    coordinator_with_paths(root, faults, storage_faults, Vec::new())
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
            assert!(row.label.contains("failed") && row.label.contains(&*path.to_string_lossy()));
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
