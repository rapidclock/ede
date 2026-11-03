//! Public storage contracts: byte integrity, immutable leases, transactions and safe saves.

use ede::document::*;
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// Force actual paging/eviction with small cache pages and a low tree fanout.
fn options(parent: &Path) -> OpenOptions {
    OpenOptions {
        working_directory: Some(parent.to_owned()),
        force_copy: true,
        limits: StorageLimits {
            text_page_bytes: 64,
            node_page_bytes: 256,
            text_cache_bytes: 512,
            node_cache_bytes: 2048,
            buffer_bytes: 1 << 20,
            undo_transactions: 32,
            disk_reserve_bytes: 0,
            ..StorageLimits::default()
        },
        faults: None,
    }
}
/// Write and open a fixture, retaining its isolated owner directory for test lifetime.
fn fixture(bytes: &[u8]) -> (TempDir, Document) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    fs::write(&path, bytes).unwrap();
    let document = Document::open(
        path,
        options(directory.path()),
        &Cancellation::default(),
        |_| {},
    )
    .unwrap();
    (directory, document)
}
/// Collect small test fixtures through bounded scalar windows, never a whole-line shortcut.
fn text(snapshot: &Snapshot) -> String {
    let mut result = String::new();
    let mut offset = ByteOffset(0);
    loop {
        let window = snapshot.read_window(offset, 7).unwrap();
        result.push_str(window.text());
        if let Some(next) = window.continuation() {
            assert!(next.0 > offset.0);
            offset = next;
        } else {
            break;
        }
    }
    result
}
/// Replace one range using the current epoch.
fn replace(document: &mut Document, start: u64, end: u64, insertion: &str) {
    document
        .apply(
            document.revision(),
            [Edit {
                range: TextRange::new(start, end).unwrap(),
                text: insertion,
            }],
            &Cancellation::default(),
        )
        .unwrap();
}
/// An armable fault seam isolates save/edit failures from successful open preparation.
#[derive(Default, Debug)]
struct Faults(Mutex<Option<IoPoint>>);
impl FaultInjector for Faults {
    /// Fail every occurrence of the selected operation until disarmed.
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if *self.0.lock().unwrap() == Some(point) {
            Err(io::Error::other("injected filesystem failure"))
        } else {
            Ok(())
        }
    }
}

/// BOM, mixed terminators, Unicode, empty/final lines and non-UTF-8 filenames round-trip exactly.
#[test]
fn supported_formats_round_trip_and_preserve_paths() {
    for bytes in [
        b"".as_slice(),
        b"a\n",
        b"\xef\xbb\xbf",
        "\u{feff}a\r\nβ\n\rX\n😀".as_bytes(),
        b"a\rb",
        b"\x1b[31m\ttext",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source-β");
        fs::write(&path, bytes).unwrap();
        let mut document = Document::open(
            &path,
            options(directory.path()),
            &Cancellation::default(),
            |_| {},
        )
        .unwrap();
        assert!(!document.is_dirty());
        let snapshot = document.snapshot().unwrap();
        let expected = if bytes.starts_with(b"\xef\xbb\xbf") {
            &bytes[3..]
        } else {
            bytes
        };
        assert_eq!(text(&snapshot).as_bytes(), expected);
        assert_eq!(
            snapshot.line_count(),
            expected.iter().filter(|&&b| b == b'\n').count() as u64 + 1
        );
        let destination = directory.path().join("copy");
        let report = document.save(
            &snapshot,
            SaveTarget::New(destination.clone()),
            &Cancellation::default(),
        );
        assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(fs::metadata(destination).unwrap().mode() & 0o777, 0o600);
    }
}
/// Prefix success never allows editing a late invalid UTF-8/binary file.
#[test]
fn preview_precedes_complete_validation_and_cancellation() {
    for suffix in [vec![0], vec![0xff], vec![0xf0, 0x9f]] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        let mut bytes = vec![b'a'; 4096];
        bytes.extend_from_slice(&suffix);
        fs::write(&path, bytes).unwrap();
        let mut preview = false;
        let result = Document::open(
            &path,
            options(directory.path()),
            &Cancellation::default(),
            |event| {
                if let OpenProgress::Preview { text, .. } = event {
                    preview = true;
                    assert_eq!(text.len(), 64);
                }
            },
        );
        assert!(preview);
        assert!(matches!(
            result,
            Err(Error::Binary | Error::UnsupportedEncoding { .. })
        ));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    fs::write(&path, vec![b'a'; 4096]).unwrap();
    let cancel = Cancellation::default();
    assert!(matches!(
        Document::open(&path, options(directory.path()), &cancel, |event| {
            if matches!(event, OpenProgress::Preview { .. }) {
                cancel.cancel();
            }
        }),
        Err(Error::Cancelled)
    ));
}
/// Recognized UTF-16/32 BOMs outrank NUL detection; invalid UTF-8 offsets include BOM bytes.
#[test]
fn text_policy_errors_are_distinct() {
    for bytes in [
        b"\xff\xfe\x00\x00".as_slice(),
        b"\xfe\xff\x00a",
        b"\x00\x00\xfe\xff",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            Document::open(
                &path,
                options(directory.path()),
                &Cancellation::default(),
                |_| {}
            ),
            Err(Error::UnsupportedEncoding { offset: 0 })
        ));
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    fs::write(&path, b"\xef\xbb\xbfabc\xff").unwrap();
    assert!(matches!(
        Document::open(
            &path,
            options(directory.path()),
            &Cancellation::default(),
            |_| {}
        ),
        Err(Error::UnsupportedEncoding { offset: 6 })
    ));
    assert!(matches!(
        Document::open(
            directory.path(),
            options(directory.path()),
            &Cancellation::default(),
            |_| {}
        ),
        Err(Error::UnsupportedType)
    ));
    let fifo = directory.path().join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(matches!(
        Document::open(
            fifo,
            options(directory.path()),
            &Cancellation::default(),
            |_| {}
        ),
        Err(Error::UnsupportedType)
    ));
}
/// Private accepted content remains readable through external in-place edits, unlink and rename.
#[test]
fn external_changes_do_not_mutate_snapshots_or_undo() {
    for mutation in 0..4 {
        let (directory, mut document) = fixture(b"original\n");
        let path = directory.path().join("source");
        let old = document.snapshot().unwrap();
        replace(&mut document, 0, 8, "edited");
        match mutation {
            0 => fs::write(&path, b"external overwrite").unwrap(),
            1 => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(0)
                .unwrap(),
            2 => fs::remove_file(&path).unwrap(),
            _ => fs::rename(&path, directory.path().join("moved")).unwrap(),
        }
        assert_eq!(text(&old), "original\n");
        assert_eq!(text(&document.snapshot().unwrap()), "edited\n");
        document.undo(document.revision()).unwrap();
        assert_eq!(text(&document.snapshot().unwrap()), "original\n");
        assert!(!document.is_dirty());
    }
}
/// Source modification observed during preparation is an explicit retry error.
#[test]
fn observed_source_change_rejects_ready() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    fs::write(&path, vec![b'a'; 2048]).unwrap();
    let mut changed = false;
    let result = Document::open(
        &path,
        options(directory.path()),
        &Cancellation::default(),
        |event| {
            if matches!(event, OpenProgress::Preparing { .. }) && !changed {
                fs::write(&path, vec![b'b'; 2048]).unwrap();
                changed = true;
            }
        },
    );
    assert!(matches!(result, Err(Error::SourceChanged)));
}
/// Invalid batches cannot partially publish, destroy redo or increment epochs.
#[test]
fn transaction_boundaries_and_failures_are_atomic() {
    let (_directory, mut document) = fixture("aé\r\nz".as_bytes());
    let old = document.snapshot().unwrap();
    for (start, end) in [(2, 3), (3, 4), (0, 99)] {
        assert!(matches!(
            document.apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(start, end).unwrap(),
                    text: "x"
                }],
                &Cancellation::default()
            ),
            Err(Error::InvalidRange)
        ));
        assert_eq!(text(&document.snapshot().unwrap()), "aé\r\nz");
    }
    replace(&mut document, 0, 1, "A");
    document.undo(document.revision()).unwrap();
    let revision = document.revision();
    assert!(
        document
            .apply(
                revision,
                [
                    Edit {
                        range: TextRange::new(2, 3).unwrap(),
                        text: "bad"
                    },
                    Edit {
                        range: TextRange::new(5, 6).unwrap(),
                        text: "changed"
                    }
                ],
                &Cancellation::default()
            )
            .is_err()
    );
    assert_eq!(document.revision(), revision);
    document.redo(revision).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "Aé\r\nz");
    assert_eq!(text(&old), "aé\r\nz");
    let epoch = document.revision();
    replace(&mut document, 0, 1, "A");
    assert_eq!(document.revision(), epoch);
    assert!(matches!(
        document.apply(Revision(0), [], &Cancellation::default()),
        Err(Error::StaleRevision)
    ));
}
/// A deterministic reference model exercises edit/undo/redo state transitions and B+ rebalancing.
#[test]
fn randomized_transactions_match_reference_model() {
    let (_directory, mut document) = fixture(b"start\n");
    let mut states = vec![String::from("start\n")];
    let mut cursor = 0;
    let mut random = 765_321u64;
    for iteration in 0..1500 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        match random % 10 {
            0 if cursor > 0 => {
                document.undo(document.revision()).unwrap();
                cursor -= 1;
            }
            1 if cursor + 1 < states.len() => {
                document.redo(document.revision()).unwrap();
                cursor += 1;
            }
            _ => {
                let current = &states[cursor];
                let len = current.len();
                let start = (random >> 8) as usize % (len + 1);
                let end = start + (random >> 24) as usize % (len - start + 1);
                let insertion = match iteration % 4 {
                    0 => "abc",
                    1 => "\n",
                    2 => "",
                    _ => "xyz\n",
                };
                let mut next = current.clone();
                next.replace_range(start..end, insertion);
                replace(&mut document, start as u64, end as u64, insertion);
                if next != *current {
                    states.truncate(cursor + 1);
                    states.push(next);
                    cursor += 1;
                    while states.len() > 33 {
                        states.remove(0);
                        cursor -= 1;
                    }
                }
            }
        }
        assert_eq!(
            text(&document.snapshot().unwrap()),
            states[cursor],
            "iteration {iteration}"
        );
        let snapshot = document.snapshot().unwrap();
        assert_eq!(
            snapshot.line_count(),
            states[cursor].bytes().filter(|&b| b == b'\n').count() as u64 + 1
        );
        for (line, offset) in std::iter::once(0)
            .chain(states[cursor].match_indices('\n').map(|(i, _)| i + 1))
            .enumerate()
        {
            assert_eq!(
                snapshot
                    .locate_line(line as u64, &Cancellation::default())
                    .unwrap(),
                ByteOffset(offset as u64)
            );
        }
    }
}
/// Hundreds of simultaneous pieces force multiple B+ levels and wide deletion repair.
#[test]
fn fragmented_batches_compact_without_losing_history() {
    let (directory, mut document) = fixture(&vec![b'a'; 2000]);
    let edits = (0..1000).map(|i| Edit {
        range: TextRange::new(i * 2, i * 2).unwrap(),
        text: "B\n",
    });
    document
        .apply(document.revision(), edits, &Cancellation::default())
        .unwrap();
    let expected = "B\naa".repeat(1000);
    assert_eq!(text(&document.snapshot().unwrap()), expected);
    replace(&mut document, 7, 3991, "middle\n");
    let newest = text(&document.snapshot().unwrap());
    let lease = document.snapshot().unwrap();
    assert!(matches!(
        document.compact(&Cancellation::default()),
        Err(Error::Busy)
    ));
    drop(lease);
    document.compact(&Cancellation::default()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), newest);
    document.undo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), expected);
    document.undo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "a".repeat(2000));
    assert!(!document.is_dirty());
    document.redo(document.revision()).unwrap();
    document.redo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), newest);
    let snapshot = document.snapshot().unwrap();
    let result = document.save(
        &snapshot,
        SaveTarget::New(directory.path().join("saved")),
        &Cancellation::default(),
    );
    assert_eq!(result.outcome, SaveOutcome::Saved, "{:?}", result.error);
}
/// Windows do not split UTF-8 even when physical pages do; CRLF endpoints remain atomic for edits.
#[test]
fn scalar_page_and_crlf_boundaries() {
    let input = format!("{}😀\r\n{}é", "a".repeat(63), "b".repeat(128));
    let (_directory, mut document) = fixture(input.as_bytes());
    let snapshot = document.snapshot().unwrap();
    assert_eq!(text(&snapshot), input);
    assert_eq!(
        snapshot.locate_line(1, &Cancellation::default()).unwrap(),
        ByteOffset(69)
    );
    assert!(matches!(
        snapshot.read_window(ByteOffset(64), 4),
        Err(Error::InvalidRange)
    ));
    assert!(matches!(
        snapshot.read_window(ByteOffset(0), 3),
        Err(Error::InvalidRange)
    ));
    assert_eq!(
        snapshot.read_window(ByteOffset(63), 4).unwrap().text(),
        "😀"
    );
    replace(&mut document, 63, 69, "\n");
    assert_eq!(
        text(&document.snapshot().unwrap()),
        format!("{}\n{}é", "a".repeat(63), "b".repeat(128))
    );
}
/// Save acknowledges exactly the leased state and preserves all undo history.
#[test]
fn older_snapshot_save_does_not_clean_newer_edits() {
    let (directory, mut document) = fixture(b"one\n");
    replace(&mut document, 0, 3, "two");
    let older = document.snapshot().unwrap();
    replace(&mut document, 0, 3, "three");
    let report = document.save(&older, SaveTarget::Current, &Cancellation::default());
    assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
    assert_eq!(report.revision, older.revision());
    assert_eq!(fs::read(directory.path().join("source")).unwrap(), b"two\n");
    assert!(document.is_dirty());
    document.undo(document.revision()).unwrap();
    assert!(!document.is_dirty());
    document.undo(document.revision()).unwrap();
    assert!(document.is_dirty());
}
/// Every save boundary reports the actual name-publication outcome and retains edits.
#[test]
fn save_failure_boundaries_preserve_integrity() {
    for point in [
        IoPoint::Read,
        IoPoint::Write,
        IoPoint::Flush,
        IoPoint::Sync,
        IoPoint::Metadata,
        IoPoint::Publish,
        IoPoint::DirectorySync,
        IoPoint::Verify,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source");
        fs::write(&path, b"before").unwrap();
        let faults = Arc::new(Faults::default());
        let mut configuration = options(directory.path());
        configuration.faults = Some(faults.clone());
        let mut document =
            Document::open(&path, configuration, &Cancellation::default(), |_| {}).unwrap();
        replace(&mut document, 0, 6, "after");
        let snapshot = document.snapshot().unwrap();
        *faults.0.lock().unwrap() = Some(point);
        let result = document.save(&snapshot, SaveTarget::Current, &Cancellation::default());
        let committed = matches!(point, IoPoint::DirectorySync | IoPoint::Verify);
        assert_eq!(
            result.outcome,
            if committed {
                SaveOutcome::CommittedDurabilityUncertain
            } else {
                SaveOutcome::NotCommitted
            },
            "{point:?}: {:?}",
            result.error
        );
        assert!(result.error.is_some());
        assert!(document.is_dirty());
        assert_eq!(
            fs::read(&path).unwrap(),
            if committed {
                b"after".as_slice()
            } else {
                b"before".as_slice()
            }
        );
        *faults.0.lock().unwrap() = None;
        assert_eq!(text(&snapshot), "after");
        assert_eq!(
            fs::read_dir(directory.path())
                .unwrap()
                .filter(|entry| entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ede-save-"))
                .count(),
            0
        );
    }
}
/// Links, mode bits, external replacement and no-clobber targets have explicit policies.
#[test]
fn save_conflicts_links_and_permissions() {
    let (directory, mut document) = fixture(b"initial");
    let path = directory.path().join("source");
    replace(&mut document, 0, 7, "edited");
    let snapshot = document.snapshot().unwrap();
    fs::write(&path, b"outside").unwrap();
    let report = document.save(&snapshot, SaveTarget::Current, &Cancellation::default());
    assert!(matches!(report.error, Some(Error::SaveConflict)));
    assert_eq!(fs::read(&path).unwrap(), b"outside");
    let destination = directory.path().join("exists");
    fs::write(&destination, b"keep").unwrap();
    let report = document.save(
        &snapshot,
        SaveTarget::New(destination.clone()),
        &Cancellation::default(),
    );
    assert_eq!(report.outcome, SaveOutcome::NotCommitted);
    assert_eq!(fs::read(&destination).unwrap(), b"keep");
    let target = directory.path().join("target");
    fs::write(&target, b"target").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let link = directory.path().join("link");
    symlink(&target, &link).unwrap();
    let mut linked = Document::open(
        &link,
        options(directory.path()),
        &Cancellation::default(),
        |_| {},
    )
    .unwrap();
    replace(&mut linked, 0, 6, "new");
    let report = linked.save(
        &linked.snapshot().unwrap(),
        SaveTarget::Current,
        &Cancellation::default(),
    );
    assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o640);
    fs::remove_file(&link).unwrap();
    symlink(&path, &link).unwrap();
    let report = linked.save(
        &linked.snapshot().unwrap(),
        SaveTarget::Current,
        &Cancellation::default(),
    );
    assert!(matches!(report.error, Some(Error::SaveConflict)));
    let hard = directory.path().join("hard");
    fs::hard_link(&path, hard).unwrap();
    let mut document = Document::open(
        &path,
        options(directory.path()),
        &Cancellation::default(),
        |_| {},
    )
    .unwrap();
    let report = document.save(
        &document.snapshot().unwrap(),
        SaveTarget::Current,
        &Cancellation::default(),
    );
    assert!(matches!(report.error, Some(Error::SaveAsRequired(_))));
}
/// Short readers and huge staged insertions never require a payload-sized allocation.
#[test]
fn incremental_staging_and_undo_budget() {
    struct Short<'a>(&'a [u8]);
    impl Read for Short<'_> {
        /// Yield one byte per call to force BOM/scalar carry through arbitrary reader boundaries.
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Ok(0);
            }
            out[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }
    let (directory, mut document) = fixture(b"abc");
    let mut transaction = document.transaction(document.revision()).unwrap();
    transaction
        .replace(
            TextRange::new(1, 2).unwrap(),
            Short("😀é".as_bytes()),
            &Cancellation::default(),
        )
        .unwrap();
    document
        .apply_staged(transaction, &Cancellation::default())
        .unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "a😀éc");
    let mut low = options(directory.path());
    low.limits.undo_bytes = 32;
    let mut document = Document::new(low).unwrap();
    replace(&mut document, 0, 0, "first");
    let revision = document.revision();
    assert!(matches!(
        document.apply(
            revision,
            [Edit {
                range: TextRange::new(0, 5).unwrap(),
                text: ""
            }],
            &Cancellation::default()
        ),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(document.revision(), revision);
    assert_eq!(text(&document.snapshot().unwrap()), "first");
}
/// Lease/window limits fail recoverably and release capacity when owners are dropped.
#[test]
fn leases_and_buffer_budgets_are_bounded() {
    let (_directory, document) = fixture(b"test");
    let mut snapshots = Vec::new();
    for _ in 0..8 {
        snapshots.push(document.snapshot().unwrap());
    }
    assert!(matches!(document.snapshot(), Err(Error::Busy)));
    snapshots.pop();
    assert!(document.snapshot().is_ok());
    assert!(matches!(
        snapshots[0].read_window(ByteOffset(0), 1 << 20),
        Err(Error::ResourceLimit(_))
    ));
    assert!(document.stats().unwrap().cache_bytes <= 512 + 2048);
}
/// Poll one asynchronous completion with a deadline, avoiding an unbounded test hang.
fn receive(service: &mut StorageService) -> Completion {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(completion) = service.try_recv().unwrap() {
            return completion;
        }
        assert!(Instant::now() < deadline, "storage worker timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
/// Service submission is bounded and close/cancel remain usable under result backpressure.
#[test]
fn asynchronous_ownership_backpressure_and_close() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = options(directory.path());
    config.limits.queue_entries = 2;
    let mut service = StorageService::new(config).unwrap();
    let job = service
        .submit(101, Request::New)
        .unwrap_or_else(|e| panic!("{}", e.error));
    let completion = receive(&mut service);
    assert_eq!(completion.request, 101);
    assert_eq!(completion.revision, Some(Revision(0)));
    assert_eq!(completion.job, job.id);
    assert!(matches!(completion.result, Ok(Response::Ready { .. })));
    drop(completion);
    drop(job);
    let job = service
        .submit(102, Request::New)
        .unwrap_or_else(|e| panic!("{}", e.error));
    let second = receive(&mut service);
    let second_id = second.document;
    drop(second);
    let a = service
        .submit(103, Request::Snapshot(second_id))
        .unwrap_or_else(|e| panic!("{}", e.error));
    let b = service
        .submit(104, Request::Snapshot(second_id))
        .unwrap_or_else(|e| panic!("{}", e.error));
    assert!(matches!(
        service.submit(105, Request::Snapshot(second_id)),
        Err(SubmissionError {
            error: Error::Busy,
            ..
        })
    ));
    service.close(second_id);
    a.cancel();
    b.cancel();
    for _ in 0..2 {
        let completion = receive(&mut service);
        assert!(matches!(
            completion.result,
            Err(Error::Closed | Error::Cancelled)
        ));
    }
    assert!(matches!(
        service.submit(106, Request::Snapshot(second_id)),
        Err(SubmissionError {
            error: Error::Closed,
            ..
        })
    ));
    drop(job);
    service.shutdown().wait().unwrap();
}

/// Linux filesystems accept arbitrary filename bytes; APFS rejects ill-formed UTF-8 at creation.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_filename_is_supported() {
    use std::os::unix::ffi::OsStringExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"source-\xff".to_vec()));
    fs::write(&path, b"text").unwrap();
    let mut document = Document::open(
        path,
        options(directory.path()),
        &Cancellation::default(),
        |_| {},
    )
    .unwrap();
    let report = document.save(
        &document.snapshot().unwrap(),
        SaveTarget::Current,
        &Cancellation::default(),
    );
    assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
}

/// A gate blocks one background read until released; foreground reads must still complete.
#[derive(Debug, Default)]
struct Gate {
    armed: std::sync::atomic::AtomicBool,
    state: Mutex<(bool, bool)>,
    wake: std::sync::Condvar,
}
impl FaultInjector for Gate {
    /// Block only the first armed read; independently readable snapshots remain available.
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if point == IoPoint::Read && self.armed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.wake.notify_all();
            while !state.1 {
                let (next, timeout) = self
                    .wake
                    .wait_timeout(state, Duration::from_secs(10))
                    .unwrap();
                state = next;
                if timeout.timed_out() {
                    return Err(io::Error::other("test gate timed out"));
                }
            }
        }
        Ok(())
    }
}
/// A blocked copy must not hold the foreground reader worker or its document lock.
#[test]
fn foreground_reads_progress_during_background_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("slow");
    fs::write(&path, vec![b'a'; 4096]).unwrap();
    let gate = Arc::new(Gate::default());
    let mut config = options(directory.path());
    config.faults = Some(gate.clone());
    let mut service = StorageService::new(config).unwrap();
    let job = service
        .submit(1, Request::New)
        .unwrap_or_else(|e| panic!("{}", e.error));
    let completion = receive(&mut service);
    let id = completion.document;
    drop(completion);
    drop(job);
    let job = service
        .submit(2, Request::Snapshot(id))
        .unwrap_or_else(|e| panic!("{}", e.error));
    let snapshot = match receive(&mut service).result {
        Ok(Response::Snapshot(snapshot)) => snapshot,
        _ => panic!("expected snapshot"),
    };
    drop(job);
    gate.armed.store(true, std::sync::atomic::Ordering::Release);
    let slow = service
        .submit(3, Request::Open(path))
        .unwrap_or_else(|e| panic!("{}", e.error));
    let state = gate.state.lock().unwrap();
    let (state, _) = gate
        .wake
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.0)
        .unwrap();
    assert!(state.0);
    drop(state);
    let read = service
        .submit(
            4,
            Request::Read {
                snapshot,
                start: ByteOffset(0),
                max_bytes: 4,
            },
        )
        .unwrap_or_else(|e| panic!("{}", e.error));
    let completion = receive(&mut service);
    assert_eq!(completion.request, 4);
    assert!(matches!(completion.result, Ok(Response::Window(_))));
    drop(completion);
    service.close(slow.document);
    gate.state.lock().unwrap().1 = true;
    gate.wake.notify_all();
    let completion = receive(&mut service);
    assert_eq!(completion.request, 3);
    assert!(completion.result.is_err());
    drop(completion);
    drop(slow);
    drop(read);
    service.shutdown().wait().unwrap();
}

/// A final-boundary interleaving injects either another writer or cancellation.
#[derive(Debug)]
struct PublishHook {
    point: IoPoint,
    path: Option<std::path::PathBuf>,
    cancel: Option<Cancellation>,
}
impl FaultInjector for PublishHook {
    /// Interleave once at the selected boundary without replacing the save's I/O implementation.
    fn check(&self, point: IoPoint) -> io::Result<()> {
        if point == self.point {
            if let Some(path) = &self.path {
                fs::write(path, b"other writer")?;
            }
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }
        Ok(())
    }
}
/// Atomic no-clobber must preserve a path created after save preflight, and known final conflicts.
#[test]
fn final_publish_races_never_overwrite_known_changes() {
    for existing in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target");
        if existing {
            fs::write(&path, b"original").unwrap();
        }
        let mut configuration = options(directory.path());
        configuration.faults = Some(Arc::new(PublishHook {
            point: IoPoint::Publish,
            path: Some(path.clone()),
            cancel: None,
        }));
        let mut document = if existing {
            Document::open(&path, configuration, &Cancellation::default(), |_| {}).unwrap()
        } else {
            Document::new(configuration).unwrap()
        };
        let target = if existing {
            SaveTarget::Current
        } else {
            SaveTarget::New(path.clone())
        };
        let report = document.save(
            &document.snapshot().unwrap(),
            target,
            &Cancellation::default(),
        );
        assert_eq!(report.outcome, SaveOutcome::NotCommitted);
        assert!(matches!(report.error, Some(Error::SaveConflict)));
        assert_eq!(fs::read(path).unwrap(), b"other writer");
    }
}
/// Cancellation before commit leaves the name untouched; cancellation afterwards cannot deny a save.
#[test]
fn save_cancellation_respects_commit_point() {
    for point in [IoPoint::Publish, IoPoint::DirectorySync] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target");
        let cancel = Cancellation::default();
        let mut configuration = options(directory.path());
        configuration.faults = Some(Arc::new(PublishHook {
            point,
            path: None,
            cancel: Some(cancel.clone()),
        }));
        let mut document = Document::new(configuration).unwrap();
        replace(&mut document, 0, 0, "content");
        let report = document.save(
            &document.snapshot().unwrap(),
            SaveTarget::New(path.clone()),
            &cancel,
        );
        if point == IoPoint::Publish {
            assert_eq!(report.outcome, SaveOutcome::NotCommitted);
            assert!(!path.exists());
        } else {
            assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
            assert_eq!(fs::read(path).unwrap(), b"content");
        }
    }
}
/// Native extended metadata is either preserved exactly or rejected before replacement.
#[test]
fn extended_metadata_is_preserved_or_requires_save_as() {
    use std::{ffi::CString, os::fd::AsRawFd};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    fs::write(&path, b"before").unwrap();
    let original = fs::File::open(&path).unwrap();
    let name = CString::new("user.ede-storage-test").unwrap();
    let value = b"metadata\0value";
    #[cfg(target_os = "macos")]
    // SAFETY: live descriptor, NUL-terminated name and valid immutable value bytes.
    let result = unsafe {
        nix::libc::fsetxattr(
            original.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: live descriptor, NUL-terminated name and valid immutable value bytes.
    let result = unsafe {
        nix::libc::fsetxattr(
            original.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    assert_eq!(result, 0, "{}", io::Error::last_os_error());
    let mut document = Document::open(
        &path,
        options(directory.path()),
        &Cancellation::default(),
        |_| {},
    )
    .unwrap();
    replace(&mut document, 0, 6, "after");
    let report = document.save(
        &document.snapshot().unwrap(),
        SaveTarget::Current,
        &Cancellation::default(),
    );
    #[cfg(target_os = "macos")]
    {
        assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
        let file = fs::File::open(&path).unwrap();
        let mut bytes = [0u8; 256];
        // SAFETY: valid output buffer, descriptor, name and Darwin positional/flag arguments.
        let count = unsafe {
            nix::libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                0,
            )
        };
        assert_eq!(count as usize, value.len());
        assert_eq!(&bytes[..value.len()], value);
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(report.outcome, SaveOutcome::NotCommitted);
        assert!(matches!(report.error, Some(Error::SaveAsRequired(_))));
        assert_eq!(fs::read(&path).unwrap(), b"before");
    }
}
/// Eviction-heavy line queries keep exact results for millions of very short lines and one long line.
#[test]
fn newline_dense_and_very_long_lines_use_bounded_windows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    use std::io::Write;
    let mut file = fs::File::create(&path).unwrap();
    let chunk = b"x\n".repeat(32768);
    for _ in 0..32 {
        file.write_all(&chunk).unwrap();
    } // 1,048,576 terminators, no line-offset array.
    file.write_all(&vec![b'z'; 1 << 20]).unwrap();
    drop(file);
    let mut configuration = options(directory.path());
    configuration.limits.text_page_bytes = 4096;
    configuration.limits.text_cache_bytes = 16384;
    let document = Document::open(path, configuration, &Cancellation::default(), |_| {}).unwrap();
    let snapshot = document.snapshot().unwrap();
    assert_eq!(snapshot.line_count(), 1_048_577);
    for line in [0, 1, 4095, 4096, 500_000, 1_048_576] {
        let offset = snapshot
            .locate_line(line, &Cancellation::default())
            .unwrap();
        assert_eq!(offset.0, line * 2);
        assert_eq!(snapshot.line_of_offset(offset).unwrap(), line);
    }
    let last = snapshot
        .locate_line(1_048_576, &Cancellation::default())
        .unwrap();
    let window = snapshot.read_window(last, 4096).unwrap();
    assert_eq!(window.text().len(), 4096);
    assert_eq!(window.continuation(), Some(ByteOffset(last.0 + 4096)));
    let stats = document.stats().unwrap();
    assert_eq!(stats.original_longest_line, 1 << 20);
    assert!(stats.cache_bytes <= 16384 + 2048);
}

/// Preparation writeback failure must not publish Ready or modify the source.
#[test]
fn long_copy_sync_failure_is_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large");
    use std::io::Write;
    let mut file = fs::File::create(&path).unwrap();
    let chunk = vec![b'a'; 65536];
    for _ in 0..144 {
        file.write_all(&chunk).unwrap();
    }
    drop(file);
    let faults = Arc::new(Faults::default());
    *faults.0.lock().unwrap() = Some(IoPoint::Sync);
    let mut config = options(directory.path());
    config.limits.text_page_bytes = 65536;
    config.limits.text_cache_bytes = 1 << 20;
    config.limits.buffer_bytes = 4 << 20;
    config.faults = Some(faults);
    let mut preview = false;
    let result = Document::open(&path, config, &Cancellation::default(), |event| {
        preview |= matches!(event, OpenProgress::Preview { .. });
    });
    assert!(preview);
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(fs::metadata(&path).unwrap().len(), 9 << 20);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

/// Clone-capable filesystems get independent inodes; fallback preserves the same semantics.
#[test]
fn optional_clone_is_stable_under_in_place_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    fs::write(&path, b"original").unwrap();
    let mut configuration = options(directory.path());
    configuration.force_copy = false;
    let document = Document::open(&path, configuration, &Cancellation::default(), |_| {}).unwrap();
    fs::write(&path, b"external").unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "original");
}
/// Pruning publishes a visible whole-transaction count and never silently removes newest undo.
#[test]
fn history_pruning_is_explicit_and_keeps_newest_undo() {
    let directory = tempfile::tempdir().unwrap();
    let mut configuration = options(directory.path());
    configuration.limits.undo_transactions = 2;
    let mut document = Document::new(configuration).unwrap();
    for _ in 0..4 {
        replace(&mut document, 0, 0, "x");
    }
    assert_eq!(document.take_history_pruned(), 2);
    assert_eq!(document.take_history_pruned(), 0);
    document.undo(document.revision()).unwrap();
    document.undo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "xx");
    let revision = document.revision();
    assert_eq!(document.undo(revision).unwrap(), revision);
}

/// Closing tabs cannot evade metadata admission while callers retain old snapshot leases.
#[test]
fn retained_closed_snapshots_keep_aggregate_metadata_reservations() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = StorageService::new(options(directory.path())).unwrap();
    let mut held = Vec::new();
    let mut exhausted = false;
    for request in 0..64 {
        let job = service.submit(request * 2, Request::New).unwrap();
        let completion = receive(&mut service);
        let id = completion.document;
        if matches!(completion.result, Err(Error::ResourceLimit(_))) {
            exhausted = true;
            break;
        }
        assert!(matches!(completion.result, Ok(Response::Ready { .. })));
        drop(completion);
        drop(job);
        let job = service
            .submit(request * 2 + 1, Request::Snapshot(id))
            .unwrap();
        match receive(&mut service).result {
            Ok(Response::Snapshot(snapshot)) => held.push(snapshot),
            _ => panic!("expected snapshot"),
        }
        service.close(id);
        drop(job);
    }
    assert!(exhausted);
    assert!(!held.is_empty());
    drop(held);
    let job = service.submit(1000, Request::New).unwrap();
    let completion = receive(&mut service);
    assert!(matches!(completion.result, Ok(Response::Ready { .. })));
    drop(completion);
    drop(job);
    service.shutdown().wait().unwrap();
}

/// Short path text cannot conceal an oversized owned queue allocation.
#[test]
fn queued_path_capacity_is_charged_before_admission() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = StorageService::new(options(directory.path())).unwrap();
    let mut path = std::path::PathBuf::with_capacity(2 << 20);
    path.push("short");
    assert!(matches!(
        service.submit(1, Request::Open(path)),
        Err(SubmissionError {
            error: Error::ResourceLimit(_),
            ..
        })
    ));
    service.shutdown().wait().unwrap();
}

/// History compaction must preserve sharing rather than replaying every retained edit.
#[test]
fn compaction_copies_shared_history_pages_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut configuration = options(directory.path());
    configuration.limits.undo_transactions = 256;
    let mut document = Document::new(configuration).unwrap();
    replace(&mut document, 0, 0, &"a".repeat(4096));
    for _ in 0..200 {
        replace(&mut document, 0, 0, "x");
    }
    let before = document.stats().unwrap();
    document.compact(&Cancellation::default()).unwrap();
    let after = document.stats().unwrap();
    assert!(
        after.metadata_bytes <= before.metadata_bytes,
        "compaction expanded metadata: {before:?} -> {after:?}"
    );
    for _ in 0..200 {
        document.undo(document.revision()).unwrap();
    }
    assert_eq!(text(&document.snapshot().unwrap()), "a".repeat(4096));
    for _ in 0..200 {
        document.redo(document.revision()).unwrap();
    }
    assert_eq!(
        text(&document.snapshot().unwrap()),
        format!("{}{}", "x".repeat(200), "a".repeat(4096))
    );
}
/// Packing source holes preserves byte/line semantics for current and undo roots.
#[test]
fn compaction_reclaims_unreachable_text_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let mut configuration = options(directory.path());
    configuration.limits.undo_transactions = 1;
    let mut document = Document::new(configuration).unwrap();
    let count = 65536;
    replace(&mut document, 0, 0, &"a".repeat(count));
    replace(&mut document, 0, count as u64, &"b".repeat(count));
    replace(&mut document, 0, count as u64, &"c".repeat(count));
    let before = document.stats().unwrap();
    document.compact(&Cancellation::default()).unwrap();
    let after = document.stats().unwrap();
    assert!(
        after.text_bytes < before.text_bytes,
        "{before:?} -> {after:?}"
    );
    assert_eq!(text(&document.snapshot().unwrap()), "c".repeat(count));
    document.undo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "b".repeat(count));
    document.redo(document.revision()).unwrap();
    assert_eq!(text(&document.snapshot().unwrap()), "c".repeat(count));
}
