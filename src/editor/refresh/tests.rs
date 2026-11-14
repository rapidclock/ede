//! Refresh ordering and mapping exercise real storage owners and private generations.

use super::*;
use crate::editor::recovery::tests::{coordinator, shutdown};

/// F02: input in B cannot indefinitely defer an inactive clean A's refresh.
#[test]
fn unrelated_tab_input_does_not_block_refresh() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "old").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    worker.action(Action::New).unwrap();
    let other = worker.tab.screen.tab.unwrap();
    let held = worker.view_cancel.admit(&Action::ForTab(
        other,
        Box::new(Action::Insert("queued".into())),
    ));
    std::fs::write(&path, "new").unwrap();
    let prepared = prepared(&mut worker, document);
    worker.install_refresh(tab, document, prepared).unwrap();
    assert_eq!(
        worker.state(tab).unwrap().info.as_ref().unwrap().revision.0,
        1
    );
    assert_eq!(worker.tab.screen.tab, Some(other));
    for epoch in held {
        epoch.pending.fetch_sub(1, Ordering::AcqRel);
    }
    shutdown(worker);
}

/// F02: a bracketed paste protects its original bytes before the paste is enqueued.
#[test]
fn receiving_paste_revokes_prepared_handoff() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "original").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    std::fs::write(&path, "external").unwrap();
    let mut candidate = prepared(&mut worker, document);
    let view = worker.refresh_view(tab).unwrap();
    let mapped = map_view(
        &mut candidate,
        view,
        worker.tab.input.value.clone(),
        &Cancellation::default(),
    )
    .unwrap();
    worker.view_cancel.set_receiving(Some(tab));
    assert!(matches!(
        worker.commit_refresh(tab, document, candidate, mapped),
        Err(Error::StaleRevision)
    ));
    worker.action(Action::Insert("paste ".into())).unwrap();
    worker.view_cancel.set_receiving(None);
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "paste original"
    );
    shutdown(worker);
}

/// F02: view I/O after publication cannot leave a stale owner revision or broken Undo.
#[test]
fn failed_postcommit_render_keeps_authoritative_revision_and_history() {
    use crate::document::{FaultInjector, IoPoint};
    #[derive(Debug, Default)]
    struct ReadFault(AtomicBool);
    impl FaultInjector for ReadFault {
        fn check(&self, point: IoPoint) -> io::Result<()> {
            if point == IoPoint::Read && self.0.load(Ordering::Acquire) {
                return Err(io::Error::other("injected view failure"));
            }
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "old text").unwrap();
    let faults = Arc::new(ReadFault::default());
    let (mut worker, _events) = crate::editor::recovery::tests::coordinator_with_storage(
        root.path(),
        None,
        Some(faults.clone()),
    );
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    std::fs::write(&path, "new text").unwrap();
    let mut candidate = prepared(&mut worker, document);
    let view = worker.refresh_view(tab).unwrap();
    let mapped = map_view(
        &mut candidate,
        view,
        worker.tab.input.value.clone(),
        &Cancellation::default(),
    )
    .unwrap();
    faults.0.store(true, Ordering::Release);
    worker
        .commit_refresh(tab, document, candidate, mapped)
        .unwrap();
    assert_eq!(worker.tab.info.as_ref().unwrap().revision.0, 1);
    assert!(worker.tab.view_retry.is_some());
    assert_eq!(worker.tab.undo_views.len(), 1);
    faults.0.store(false, Ordering::Release);
    worker.action(Action::Command(Command::Undo)).unwrap();
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "old text"
    );
    worker.action(Action::Insert("usable ".into())).unwrap();
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "usable old text"
    );
    shutdown(worker);
}

fn prepared(worker: &mut Coordinator, document: DocumentId) -> document::PreparedRefresh {
    let task = match worker.call(Request::PrepareRefresh(document)).unwrap() {
        Response::RefreshTask(task) => task,
        _ => panic!("refresh task required"),
    };
    task.run(&Cancellation::default()).unwrap()
}

/// F04: retry deadlines grow to 30 seconds; explicit Retry bypasses the deadline.
#[test]
fn refresh_backoff_is_bounded_and_manual_retry_is_immediate() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "last good").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    for seconds in [1, 2, 4, 8, 16, 30, 30] {
        let before = Instant::now();
        worker.refresh_failed(tab, Error::UnsupportedEncoding { offset: 0 });
        let after = Instant::now();
        let retry = worker.tab.watch.retry.unwrap();
        assert!(
            (before + Duration::from_secs(seconds)..=after + Duration::from_secs(seconds))
                .contains(&retry)
        );
    }
    worker.retry_refresh(tab).unwrap();
    assert!(worker.tab.watch.retry.is_none());
    assert_eq!(worker.tab.watch.failures, 0);
    assert!(worker.monitor.as_ref().unwrap().due <= Instant::now());
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "last good"
    );
    shutdown(worker);
}

/// F02/F03: logical grapheme ordinals survive changed widths and tab expansion.
#[test]
fn refresh_maps_graphemes_instead_of_terminal_cells() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "界x\n\tend\n").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    worker
        .action(Action::Command(Command::Move(Movement::Right, false)))
        .unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    assert_eq!(worker.tab.screen.position.caret, 3);
    std::fs::write(&path, "abx\n end\n").unwrap();
    let prepared = prepared(&mut worker, document);
    worker.install_refresh(tab, document, prepared).unwrap();
    assert_eq!(worker.tab.screen.position.caret, 1);
    assert_eq!(worker.tab.screen.position.anchor, 1);
    let authoritative = worker.read_info(document).unwrap().revision;
    assert_eq!(worker.tab.info.as_ref().unwrap().revision, authoritative);
    worker.action(Action::Command(Command::Undo)).unwrap();
    assert_eq!(worker.tab.screen.position.caret, 3);
    shutdown(worker);
}

/// F02: an old foreground cancellation never disables an unattended background handoff.
#[test]
fn foreground_cancellation_does_not_poison_refresh() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "before").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    std::fs::write(&path, "after").unwrap();
    let prepared = prepared(&mut worker, document);
    worker.cancel.cancel();
    worker.install_refresh(tab, document, prepared).unwrap();
    assert_eq!(worker.tab.info.as_ref().unwrap().revision.0, 1);
    assert!(!worker.tab.screen.modified);
    assert!(worker.tab.screen.view.is_some());
    worker.begin();
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "after"
    );
    shutdown(worker);
}

/// F02: a newer delivered observation revokes a fully prepared older replacement.
#[test]
fn newer_observation_revokes_ready_old_generation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "original").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    std::fs::write(&path, "version A").unwrap();
    let observation_a = document::observe(&path).unwrap();
    let prepared = prepared(&mut worker, document);
    worker.tab.watch.observed = Some(observation_a.clone());
    worker.tab.watch.confirmed = true;
    worker.tab.watch.stable = Some(Instant::now());
    let (sender, inbox) = mpsc::sync_channel(1);
    let monitor = worker.monitor.as_mut().unwrap();
    monitor.inbox = inbox;
    monitor.due = Instant::now() + Duration::from_secs(3600);
    monitor.preparing = Some(Preparing {
        tab,
        document,
        path: path.clone(),
        revision: worker.tab.info.as_ref().unwrap().revision,
        observation: observation_a,
        cancel: Cancellation::default(),
        worker: None,
        ready: Some(prepared),
        progress: Arc::new(Mutex::new(None)),
    });
    std::fs::write(&path, "version B").unwrap();
    sender
        .send(vec![Observation {
            target: Target {
                tab,
                document,
                path: path.clone(),
            },
            result: Ok(document::observe(&path).unwrap()),
        }])
        .unwrap();
    worker.poll_refreshes().unwrap();
    assert_eq!(
        worker
            .snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 64)
            .unwrap()
            .text(),
        "original"
    );
    assert!(worker.monitor.as_ref().unwrap().preparing.is_none());
    assert_eq!(
        worker.tab.watch.observed.as_ref(),
        Some(&document::observe(&path).unwrap())
    );
    shutdown(worker);
}

/// F02: navigation while copying is mapped from the latest acknowledged view.
#[test]
fn mapping_uses_navigation_after_preparation_started() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("named");
    std::fs::write(&path, "界xy\nsecond\nthird").unwrap();
    let (mut worker, _events) = coordinator(root.path(), None);
    worker.open_and_switch(&path).unwrap();
    let tab = worker.tab.screen.tab.unwrap();
    let document = worker.document().unwrap();
    std::fs::write(&path, "abc\nchanged\nthird").unwrap();
    let prepared = prepared(&mut worker, document);
    worker
        .action(Action::Command(Command::Move(Movement::Right, false)))
        .unwrap();
    worker
        .action(Action::Command(Command::Move(Movement::Right, true)))
        .unwrap();
    worker.action(Action::Scroll(1, 0)).unwrap();
    let top = worker.tab.top;
    worker.install_refresh(tab, document, prepared).unwrap();
    assert_eq!(
        worker.tab.screen.position,
        Position {
            caret: 2,
            anchor: 1
        }
    );
    assert_eq!(worker.tab.top, top);
    shutdown(worker);
}
