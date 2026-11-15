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
        worker.run(PathBuf::new(), Vec::new())
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
        navigation: None,
        query: Query {
            literal: "one".into(),
            ..Query::default()
        },
        replacement: "123456789".into(),
        all: false,
        operation,
    }
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
        worker.run(PathBuf::new(), Vec::new())
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
        let Response::RefreshTask(task) = worker.call(Request::PrepareRefresh(document)).unwrap()
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
    worker.action(Action::Search(request.clone())).unwrap();
    settle(&mut worker);
    assert_eq!(worker.search.status.processed, 64);
    assert_eq!(worker.search.status.changed, 64);
    assert_eq!(worker.search.status.outcomes.len(), 64);
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
    worker
        .action(Action::Search(request(search::Operation::ReplaceAll)))
        .unwrap();
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
        if point == document::IoPoint::Write && thread::current().name() == Some("ede-search-plan")
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
        navigation: None,
        query: Query {
            literal: "one".into(),
            ..Query::default()
        },
        replacement: "1".into(),
        all: false,
        operation: search::Operation::Next,
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
        if point == document::IoPoint::Read && thread::current().name() == Some("ede-search-scan") {
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
