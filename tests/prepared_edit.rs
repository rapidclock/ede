//! Private batch preparation must never replace a newer live document revision.
use ede::document::{Cancellation, Document, Edit, Error, OpenOptions, TextRange};

/// Regression from independent review: compaction must not import foreign roots.
#[test]
fn prepared_edit_preserves_save_baseline_after_history_churn() {
    use ede::document::{SaveOutcome, SaveTarget};
    let cancel = Cancellation::default();
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("saved");
    let mut options = OpenOptions::default();
    options.limits.undo_transactions = 1;
    options.limits.undo_bytes = 128 << 10;
    let mut doc = Document::new(options).unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(0, 0).unwrap(),
            text: "abcd",
        }],
        &cancel,
    )
    .unwrap();
    assert_eq!(
        doc.save(
            &doc.snapshot().unwrap(),
            SaveTarget::New(destination.clone()),
            &cancel
        )
        .outcome,
        SaveOutcome::Saved
    );
    for index in 0..22 {
        let text = if index % 2 == 0 { "a" } else { "b" }.repeat(64 << 10);
        doc.apply(
            doc.revision(),
            [Edit {
                range: TextRange::new(0, doc.snapshot().unwrap().len()).unwrap(),
                text: &text,
            }],
            &cancel,
        )
        .unwrap();
    }
    let task = doc.prepare_edit().unwrap();
    let mut transaction = doc.transaction(doc.revision()).unwrap();
    transaction
        .replace(TextRange::new(0, 1).unwrap(), b"z".as_slice(), &cancel)
        .unwrap();
    doc.finish_edit(task.run(transaction, &cancel).unwrap(), &cancel)
        .unwrap();
    let report = doc.save(&doc.snapshot().unwrap(), SaveTarget::Current, &cancel);
    assert_eq!(report.outcome, SaveOutcome::Saved, "{:?}", report.error);
    let bytes = std::fs::read(destination).unwrap();
    assert_eq!(bytes[0], b'z');
    assert_eq!(bytes.len(), 64 << 10);
}

/// Stage a real replacement against a captured root without monopolizing its owner.
#[test]
fn prepared_batch_is_atomic_undoable_and_revision_guarded() {
    let cancel = Cancellation::default();
    let mut document = Document::new(OpenOptions::default()).unwrap();
    document
        .apply(
            document.revision(),
            [Edit {
                range: TextRange::new(0, 0).unwrap(),
                text: "one two one",
            }],
            &cancel,
        )
        .unwrap();
    let task = document.prepare_edit().unwrap();
    let mut transaction = document.transaction(document.revision()).unwrap();
    transaction
        .replace(TextRange::new(0, 3).unwrap(), "1".as_bytes(), &cancel)
        .unwrap();
    transaction
        .replace(TextRange::new(8, 11).unwrap(), "1".as_bytes(), &cancel)
        .unwrap();
    let prepared = task.run(transaction, &cancel).unwrap();
    assert_eq!(
        document
            .snapshot()
            .unwrap()
            .read_window(ede::document::ByteOffset(0), 64)
            .unwrap()
            .text(),
        "one two one"
    );
    document.finish_edit(prepared, &cancel).unwrap();
    assert_eq!(
        document
            .snapshot()
            .unwrap()
            .read_window(ede::document::ByteOffset(0), 64)
            .unwrap()
            .text(),
        "1 two 1"
    );
    document.undo(document.revision()).unwrap();
    assert_eq!(
        document
            .snapshot()
            .unwrap()
            .read_window(ede::document::ByteOffset(0), 64)
            .unwrap()
            .text(),
        "one two one"
    );

    let task = document.prepare_edit().unwrap();
    let mut transaction = document.transaction(document.revision()).unwrap();
    transaction
        .replace(TextRange::new(0, 3).unwrap(), "old".as_bytes(), &cancel)
        .unwrap();
    let prepared = task.run(transaction, &cancel).unwrap();
    document
        .apply(
            document.revision(),
            [Edit {
                range: TextRange::new(0, 0).unwrap(),
                text: "new ",
            }],
            &cancel,
        )
        .unwrap();
    let rejected = document.finish_edit(prepared, &cancel);
    assert_eq!(
        document
            .snapshot()
            .unwrap()
            .read_window(ede::document::ByteOffset(0), 64)
            .unwrap()
            .text(),
        "new one two one"
    );
    assert!(matches!(rejected, Err(Error::StaleRevision)));
}

/// The preparation token remains authoritative through the final owner handoff.
#[test]
fn cancelled_prepared_batch_never_changes_text_revision_or_undo() {
    let cancel = Cancellation::default();
    let mut document = Document::new(OpenOptions::default()).unwrap();
    document
        .apply(
            document.revision(),
            [Edit {
                range: TextRange::new(0, 0).unwrap(),
                text: "keep",
            }],
            &cancel,
        )
        .unwrap();
    let revision = document.revision();
    let task = document.prepare_edit().unwrap();
    let mut transaction = document.transaction(revision).unwrap();
    transaction
        .replace(TextRange::new(0, 4).unwrap(), b"lost".as_slice(), &cancel)
        .unwrap();
    let prepared = task.run(transaction, &cancel).unwrap();
    cancel.cancel();
    let rejected = document.finish_edit(prepared, &Cancellation::default());
    assert_eq!(
        document
            .snapshot()
            .unwrap()
            .read_window(ede::document::ByteOffset(0), 64)
            .unwrap()
            .text(),
        "keep"
    );
    assert!(matches!(rejected, Err(Error::Cancelled)));
    assert_eq!(document.revision(), revision);
    document.undo(revision).unwrap();
    assert!(document.snapshot().unwrap().is_empty());
}

/// Off-owner maintenance must retain mapped save roots and cannot cross a live save lease.
#[test]
fn private_maintenance_preserves_text_revision_history_and_save_baseline() {
    use ede::document::{ByteOffset, SaveOutcome, SaveTarget};
    let cancel = Cancellation::default();
    let root = tempfile::tempdir().unwrap();
    let mut options = OpenOptions::default();
    options.limits.undo_transactions = 1;
    options.limits.undo_bytes = 128 << 10;
    let mut doc = Document::new(options).unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(0, 0).unwrap(),
            text: "baseline",
        }],
        &cancel,
    )
    .unwrap();
    assert_eq!(
        doc.save(
            &doc.snapshot().unwrap(),
            SaveTarget::New(root.path().join("saved")),
            &cancel
        )
        .outcome,
        SaveOutcome::Saved
    );
    for index in 0..22 {
        let text = if index % 2 == 0 { "a" } else { "b" }.repeat(64 << 10);
        doc.apply(
            doc.revision(),
            [Edit {
                range: TextRange::new(0, doc.snapshot().unwrap().len()).unwrap(),
                text: &text,
            }],
            &cancel,
        )
        .unwrap();
    }
    let revision = doc.revision();
    let before = doc.stats().unwrap();
    let held = doc.snapshot().unwrap();
    let maintenance = doc.prepare_edit().unwrap().compact(&cancel).unwrap();
    assert!(matches!(
        doc.finish_edit(maintenance, &cancel),
        Err(Error::Busy)
    ));
    drop(held);
    let maintenance = doc.prepare_edit().unwrap().compact(&cancel).unwrap();
    let report = doc.finish_edit(maintenance, &cancel).unwrap();
    assert!(!report.changed);
    assert_eq!(doc.revision(), revision);
    assert!(doc.stats().unwrap().text_bytes < before.text_bytes);
    assert_eq!(
        doc.snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 4)
            .unwrap()
            .text(),
        "bbbb"
    );
    assert_eq!(
        doc.save(&doc.snapshot().unwrap(), SaveTarget::Current, &cancel)
            .outcome,
        SaveOutcome::Saved
    );
    doc.undo(doc.revision()).unwrap();
    assert_eq!(
        doc.snapshot()
            .unwrap()
            .read_window(ByteOffset(0), 4)
            .unwrap()
            .text(),
        "aaaa"
    );
}

/// Saving to a new destination changes authority without changing text revision.
#[test]
fn private_maintenance_cannot_restore_an_obsolete_save_destination() {
    use ede::document::{SaveOutcome, SaveTarget};
    let cancel = Cancellation::default();
    let root = tempfile::tempdir().unwrap();
    let mut doc = Document::new(OpenOptions::default()).unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    assert_eq!(
        doc.save(&doc.snapshot().unwrap(), SaveTarget::New(first), &cancel)
            .outcome,
        SaveOutcome::Saved
    );
    let prepared = doc.prepare_edit().unwrap().compact(&cancel).unwrap();
    assert_eq!(
        doc.save(
            &doc.snapshot().unwrap(),
            SaveTarget::New(second.clone()),
            &cancel
        )
        .outcome,
        SaveOutcome::Saved
    );
    let result = doc.finish_edit(prepared, &cancel);
    assert_eq!(doc.path(), Some(second.as_path()));
    assert!(matches!(result, Err(Error::StaleRevision)));
}
