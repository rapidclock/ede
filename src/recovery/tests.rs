//! Recovery acceptance checks use private profiles and real filesystem publication.

use super::*;
use crate::document::{Document, Edit, OpenOptions, TextRange};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// Stop at a real publication boundary once, with explicit cross-thread ordering.
#[derive(Debug)]
struct Gate {
    point: IoPoint,
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    armed: AtomicBool,
}

impl FaultInjector for Gate {
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if point == self.point && self.armed.swap(false, Ordering::AcqRel) {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
        Ok(())
    }
}

/// Reject one specific I/O boundary to distinguish pre- and post-publication failure.
#[derive(Debug)]
struct Fail(IoPoint);
impl FaultInjector for Fail {
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if point == self.0 {
            Err(io::Error::other("injected recovery failure"))
        } else {
            Ok(())
        }
    }
}

/// Construct a private store without imposing production disk headroom on tiny fixtures.
fn open(root: &Path) -> Store {
    Store::open(Options {
        root: root.join("state"),
        reserve_bytes: 0,
        ..Options::default()
    })
    .unwrap()
}

/// Prepare real immutable document text rather than a mock persistence payload.
fn document(text: &str) -> Document {
    let mut document = Document::new(OpenOptions::default()).unwrap();
    document
        .apply(
            document.revision(),
            [Edit {
                range: TextRange::new(0, 0).unwrap(),
                text,
            }],
            &Cancellation::default(),
        )
        .unwrap();
    document
}

/// R01/R04: active owners stay isolated and a later owner claims every retired set.
#[test]
fn independent_live_owners_and_combined_retired_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut a = open(root.path());
    let first = Id::fresh().unwrap();
    let text = document("first 界e\u{301}\r\nlast");
    a.checkpoint(
        first,
        Some(text.snapshot().unwrap()),
        View::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let mut b = open(root.path());
    assert!(b.page(None).unwrap().is_empty());
    let second = Id::fresh().unwrap();
    b.checkpoint(
        second,
        Some(document("second").snapshot().unwrap()),
        View::default(),
        &Cancellation::default(),
    )
    .unwrap();
    drop(a);
    let c = open(root.path());
    assert_eq!(
        c.page(None)
            .unwrap()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [first]
    );
    assert_eq!(
        b.page(None)
            .unwrap()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [second]
    );
    drop(b);
    drop(c);
    let restored = open(root.path());
    let entries = restored.page(None).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.id == first));
    assert!(entries.iter().any(|entry| entry.id == second));
    let first = entries.iter().find(|entry| entry.id == first).unwrap();
    assert_eq!(
        fs::read(restored.object_path(first)).unwrap(),
        "first 界e\u{301}\r\nlast".as_bytes()
    );
}

/// R04: simultaneous startup claims partition one retired catalog without duplicates.
#[test]
fn simultaneous_claims_never_duplicate_or_lose_scratches() {
    let root = tempfile::tempdir().unwrap();
    let mut previous = open(root.path());
    let mut expected = Vec::new();
    for order in 0..8 {
        let id = Id::fresh().unwrap();
        previous
            .checkpoint(
                id,
                Some(document("retained").snapshot().unwrap()),
                View {
                    order,
                    ..View::default()
                },
                &Cancellation::default(),
            )
            .unwrap();
        expected.push(id);
    }
    drop(previous);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let root = root.path().to_owned();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                open(&root)
            })
        })
        .collect();
    let owners: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let mut actual: Vec<_> = owners
        .iter()
        .flat_map(|store| store.page(None).unwrap().into_iter().map(|entry| entry.id))
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected);
}

/// R06: a discarded identity cannot be reinserted by a late checkpoint.
#[test]
fn discard_revokes_late_checkpoint_and_keeps_other_scratch() {
    let root = tempfile::tempdir().unwrap();
    let mut store = open(root.path());
    let id = Id::fresh().unwrap();
    let text = document("must stay discarded");
    store
        .checkpoint(
            id,
            Some(text.snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    store.discard(id, &Cancellation::default()).unwrap();
    assert!(
        store
            .checkpoint(
                id,
                Some(text.snapshot().unwrap()),
                View::default(),
                &Cancellation::default()
            )
            .is_err()
    );
    assert!(store.page(None).unwrap().is_empty());
    drop(store);
    assert!(open(root.path()).page(None).unwrap().is_empty());
}

/// R02: metadata-only checkpoints reuse immutable text and retain the matching view.
#[test]
fn view_only_checkpoint_reuses_text_object() {
    let root = tempfile::tempdir().unwrap();
    let mut store = open(root.path());
    let id = Id::fresh().unwrap();
    let before = store
        .checkpoint(
            id,
            Some(document("abc").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    let view = View {
        caret: 2,
        anchor: 1,
        top: 0,
        left: 9,
        preferred: Some(17),
        active: true,
        order: 4,
    };
    let after = store
        .checkpoint(id, None, view, &Cancellation::default())
        .unwrap();
    assert_eq!(before.object, after.object);
    assert_eq!(after.view, view);
    drop(store);
    let recovered = open(root.path());
    assert_eq!(recovered.page(None).unwrap()[0].view.caret, 2);
}

/// A malformed catalog is reported without replacing any existing bytes.
#[test]
fn malformed_catalog_is_never_replaced_with_empty_state() {
    let root = tempfile::tempdir().unwrap();
    drop(open(root.path()));
    let path = root.path().join("state/catalog");
    fs::write(&path, b"incomplete catalog").unwrap();
    assert!(
        Store::open(Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..Options::default()
        })
        .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), b"incomplete catalog");
}

/// R04: an owner exiting after live-copy classification cannot be copied again as retired.
#[test]
fn owner_exit_mid_claim_never_duplicates_scratch_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut a = open(root.path());
    let id = Id::fresh().unwrap();
    a.checkpoint(
        id,
        Some(document("one copy only").snapshot().unwrap()),
        View::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let options = Options {
        root: root.path().join("state"),
        reserve_bytes: 0,
        faults: Some(Arc::new(Gate {
            point: IoPoint::Metadata,
            entered,
            release: Mutex::new(released),
            armed: AtomicBool::new(true),
        })),
        ..Options::default()
    };
    let claiming = thread::spawn(move || Store::open(options).unwrap());
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(a);
    release.send(()).unwrap();
    let b = claiming.join().unwrap();
    assert!(b.page(None).unwrap().is_empty());
    drop(b);
    let c = open(root.path());
    let entries = c.page(None).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "the exiting owner's scratch must never be duplicated"
    );
    assert_eq!(entries[0].id, id);
}

/// R03/R07: failed staging leaves the complete previous checkpoint and its text reachable.
#[test]
fn failed_checkpoint_preserves_previous_complete_publication() {
    for point in [IoPoint::Write, IoPoint::Sync, IoPoint::Publish] {
        let root = tempfile::tempdir().unwrap();
        let mut store = open(root.path());
        let id = Id::fresh().unwrap();
        let first = store
            .checkpoint(
                id,
                Some(document("checkpoint A").snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        store.options.faults = Some(Arc::new(Fail(point)));
        assert!(
            store
                .checkpoint(
                    id,
                    Some(document("checkpoint B").snapshot().unwrap()),
                    View::default(),
                    &Cancellation::default()
                )
                .is_err()
        );
        let retained = store.page(None).unwrap();
        assert_eq!(retained[0].object, first.object);
        assert_eq!(
            fs::read(store.object_path(&retained[0])).unwrap(),
            b"checkpoint A"
        );
        drop(store);
        let restored = open(root.path());
        assert_eq!(
            fs::read(restored.object_path(&restored.page(None).unwrap()[0])).unwrap(),
            b"checkpoint A"
        );
    }
}

/// A failed sync after tombstone publication explicitly reports revoked old authority.
#[test]
fn uncertain_discard_reports_publication_and_new_identity_can_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let mut store = open(root.path());
    let id = Id::fresh().unwrap();
    let text = document("still editable after failure");
    store
        .checkpoint(
            id,
            Some(text.snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    store.options.faults = Some(Arc::new(Fail(IoPoint::DirectorySync)));
    let error = store.discard(id, &Cancellation::default()).unwrap_err();
    assert!(removal_published(&error));
    store.options.faults = None;
    let replacement = Id::fresh().unwrap();
    store
        .checkpoint(
            replacement,
            Some(text.snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    drop(store);
    let restored = open(root.path());
    let entries = restored.page(None).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, replacement);
    assert_eq!(
        fs::read(restored.object_path(&entries[0])).unwrap(),
        b"still editable after failure"
    );
}

/// A test-process checkpoint pauses on a parent-observable marker until forcibly terminated.
#[derive(Debug)]
struct CrashGate {
    point: IoPoint,
    marker: PathBuf,
}
impl FaultInjector for CrashGate {
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if point == self.point {
            fs::write(&self.marker, b"blocked at checkpoint boundary")?;
            loop {
                thread::park();
            }
        }
        Ok(())
    }
}

/// Test-only child entry point; absent explicit isolated test variables it does no work.
#[test]
fn crash_checkpoint_child() {
    let Some(root) = std::env::var_os("EDE_RECOVERY_CRASH_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut store = open(&root);
    let id = Id::fresh().unwrap();
    store
        .checkpoint(
            id,
            Some(document("complete A").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    let point = match std::env::var("EDE_RECOVERY_CRASH_POINT").unwrap().as_str() {
        "write" => IoPoint::Write,
        "sync" => IoPoint::Sync,
        "publish" => IoPoint::Publish,
        "directory" => IoPoint::DirectorySync,
        _ => panic!("invalid test-only crash point"),
    };
    store.options.faults = Some(Arc::new(CrashGate {
        point,
        marker: root.join("blocked"),
    }));
    store
        .checkpoint(
            id,
            Some(document("complete B").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    panic!("parent must terminate the blocked checkpoint");
}

/// Always kill and reap a fixture if a parent-side crash assertion fails.
struct CrashChild(std::process::Child);
impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// R03/R04: real process death releases ownership and never exposes partial checkpoint text.
#[test]
fn process_death_during_checkpoint_recovers_only_complete_publication() {
    for point in ["write", "sync", "publish", "directory"] {
        let root = tempfile::tempdir().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "recovery::tests::crash_checkpoint_child",
                "--nocapture",
            ])
            .env("EDE_RECOVERY_CRASH_ROOT", root.path())
            .env("EDE_RECOVERY_CRASH_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut child = CrashChild(child);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !root.path().join("blocked").try_exists().unwrap() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "checkpoint fixture stopped before the gate"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "checkpoint never reached {point}"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(child.0.try_wait().unwrap().is_none());
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        let recovered = open(root.path());
        let entries = recovered.page(None).unwrap();
        assert_eq!(entries.len(), 1);
        let expected = if point == "directory" {
            b"complete B"
        } else {
            b"complete A"
        };
        assert_eq!(
            fs::read(recovered.object_path(&entries[0])).unwrap(),
            expected
        );
    }
}

/// R05: pages can traverse a catalog larger than both a tab session and one listing page.
#[test]
fn recovery_pages_are_bounded_bidirectional_and_have_no_total_cutoff() {
    let root = tempfile::tempdir().unwrap();
    let mut store = open(root.path());
    let id = Id::fresh().unwrap();
    let entry = store
        .checkpoint(
            id,
            Some(document("fixture text").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
    // Reuse one immutable fixture object, isolating paging from content-copy cost.
    let fixtures = (0..131).map(|order| {
        Ok(Entry {
            id: Id::fresh().unwrap(),
            view: View {
                order,
                ..View::default()
            },
            ..entry
        })
    });
    store
        .rewrite(store.next_generation().unwrap(), fixtures)
        .unwrap();
    drop(store);
    let restored = open(root.path());
    let first = restored.page(None).unwrap();
    assert_eq!(first.len(), PAGE_ENTRIES);
    let last = first.last().unwrap();
    let second = restored.page(Some((last.view.order, last.id))).unwrap();
    assert_eq!(second.len(), 3);
    let previous = restored
        .previous_page(Some((second[0].view.order, second[0].id)))
        .unwrap();
    assert_eq!(
        previous.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        first.iter().map(|entry| entry.id).collect::<Vec<_>>()
    );
    assert_eq!(second.last().unwrap().view.order, 130);
}
