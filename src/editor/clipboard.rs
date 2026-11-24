//! Ordered retention and guarded desktop handoff for one editor session.
use super::*;
use crate::clipboard::Access;
use std::io::Read;

/// Document identity and exact selection authorized for a delayed edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Target {
    tab: TabId,
    document: DocumentId,
    revision: document::Revision,
    position: Position,
}

impl Coordinator {
    pub(super) fn clipboard_target(&self) -> Option<Target> {
        Some(Target {
            tab: self.tab.screen.tab?,
            document: self.tab.screen.document?,
            revision: self.tab.info.as_ref()?.revision,
            position: self.tab.screen.position,
        })
    }

    fn stage_selection(
        &mut self,
        range: &std::ops::Range<u64>,
        cancel: &Cancellation,
    ) -> Result<Arc<Spool>, Error> {
        let original = self.clipboard_target();
        let snapshot = self.snapshot()?;
        if range.end > snapshot.len()
            || self.clipboard_target() != original
            || original.is_some_and(|target| target.revision != snapshot.revision())
        {
            return Err(Error::StaleRevision);
        }
        let mut spool = Spool::new()?;
        let mut offset = range.start;
        while offset < range.end {
            let window = snapshot.read_window_with_cancel(
                ByteOffset(offset),
                (range.end - offset).clamp(4, 64 << 10) as usize,
                cancel,
            )?;
            let count = (range.end - offset).min(window.text().len() as u64) as usize;
            if count == 0 {
                return Err(Error::CorruptStorage);
            }
            spool.append(&window.text().as_bytes()[..count])?;
            offset += count as u64;
        }
        cancel.check()?;
        Ok(Arc::new(spool))
    }

    fn clipboard_status(&mut self, message: impl Into<String>) {
        self.tab.screen.message = message.into();
        self.tab.screen.clipboard_sequence = Some(self.active_sequence);
    }

    fn cut_selection(
        &mut self,
        spool: Arc<Spool>,
        range: std::ops::Range<u64>,
        authority: Option<(Target, Cancellation)>,
    ) -> Result<(), Error> {
        self.cut_clipboard = Some(spool);
        let result = self.replace_authorized(Replacement::Text(String::new()), range, authority);
        self.cut_clipboard = None;
        result
    }

    /// Retain a complete private copy and prepare at most one bounded OSC 52 write.
    pub(super) fn copy(&mut self, cut: bool, epoch: Option<u64>) -> Result<(), Error> {
        let range = self.tab.screen.position.selection();
        if range.is_empty() {
            self.clipboard_status("No selection");
            return Ok(());
        }
        let spool = self.stage_selection(&range, &self.cancel.clone())?;
        let result = if cut {
            self.cut_selection(spool.clone(), range, None)
        } else {
            self.clipboard = Some(spool.clone());
            Ok(())
        };
        if self
            .clipboard
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &spool))
        {
            let status = if cut {
                CUT_INTERNALLY
            } else {
                COPIED_INTERNALLY
            };
            let reason = epoch.and_then(|epoch| self.prepare_clipboard(&spool, epoch, cut).err());
            self.clipboard_status(
                reason.map_or_else(|| status.into(), |reason| format!("{status} | {reason}")),
            );
        }
        result
    }

    fn prepare_clipboard(&self, spool: &Spool, epoch: u64, cut: bool) -> Result<(), &'static str> {
        use base64::Engine;

        if spool.len > SYSTEM_CLIPBOARD_LIMIT {
            return Err("system limit 8 KiB");
        }
        let generation = self.active_sequence;
        let current = || {
            *self
                .view_cancel
                .clipboard_generation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        };
        if current() != generation {
            return Ok(());
        }
        let mut bytes = vec![0; spool.len as usize];
        spool
            .reader()
            .read_exact(&mut bytes)
            .map_err(|_| "system request prep failed")?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let frame = format!("\x1b]52;c;{data}\x1b\\").into_bytes();
        if current() != generation {
            return Ok(());
        }
        *self
            .view_cancel
            .clipboard
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(ClipboardTransfer {
            epoch,
            cut,
            sequence: self.active_sequence,
            frame,
            generation,
            authority: self.view_cancel.clipboard_generation.clone(),
        });
        Ok(())
    }

    /// Obtain a native write result before authorizing a destructive Cut.
    pub(super) fn native_copy(&mut self, cut: bool, access: Access) {
        self.last_native_write = Some((self.active_sequence, false));
        let range = self.tab.screen.position.selection();
        if range.is_empty() {
            self.clipboard_status("No selection");
            return;
        }
        let target = self.clipboard_target();
        let cancel = access.cancel.clone().with_authority(self.cancel.clone());
        let spool = match self.stage_selection(&range, &cancel) {
            Ok(spool) => spool,
            Err(error) => {
                self.clipboard_status(format!("Desktop copy staging failed | {error}"));
                return;
            }
        };
        let handoff = {
            let _in_flight = access.begin();
            access.backend.write(&spool, &cancel)
        };
        self.last_native_write = Some((self.active_sequence, handoff.is_ok()));
        if cut {
            if let Err(reason) = handoff {
                self.clipboard_status(format!("Selection kept | {reason}"));
                return;
            }
            let Some(target) = target.filter(|target| {
                !cancel.is_cancelled() && self.clipboard_target() == Some(*target)
            }) else {
                self.clipboard_status("Desktop copied | selection kept");
                return;
            };
            let result = self.cut_selection(spool.clone(), range, Some((target, cancel)));
            if self
                .clipboard
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &spool))
            {
                let message = if result.is_ok() {
                    "Cut to desktop and ede"
                } else {
                    "Cut committed | view pending"
                };
                self.clipboard_status(message);
            } else {
                self.clipboard_status("Desktop copied | selection kept");
            }
        } else if cancel.is_cancelled() {
            self.clipboard_status(if handoff.is_ok() {
                "Desktop copied | ede copy cancelled"
            } else {
                "Desktop copy cancelled"
            });
        } else {
            self.clipboard = Some(spool);
            self.clipboard_status(match handoff {
                Ok(()) => "Copied to desktop and ede".into(),
                Err(reason) => format!("{COPIED_INTERNALLY} | {reason}"),
            });
        }
    }

    /// Read only on explicit Paste. A queued read follows its preceding write or reports failure.
    pub(super) fn native_paste(&mut self, access: Access, observed: u64) {
        if self
            .last_native_write
            .is_some_and(|(sequence, success)| sequence > observed && !success)
        {
            self.clipboard_status("Desktop paste cancelled | copy failed");
            return;
        }
        let Some(target) = self.clipboard_target() else {
            return;
        };
        let cancel = access.cancel.clone().with_authority(self.cancel.clone());
        let read = {
            let _in_flight = access.begin();
            access.backend.read(&cancel)
        };
        let spool = match read {
            Ok(spool) => spool,
            Err(reason) => {
                self.clipboard_status(reason);
                return;
            }
        };
        if cancel.is_cancelled() || self.clipboard_target() != Some(target) {
            self.clipboard_status("Desktop paste cancelled");
            return;
        }
        if spool.len == 0 {
            self.clipboard_status("Desktop clipboard is empty");
            return;
        }
        let result = self.replace_authorized(
            Replacement::LiteralPaste(&spool),
            target.position.selection(),
            Some((target, cancel)),
        );
        match result {
            Ok(()) => self.clipboard_status("Pasted from desktop"),
            Err(Error::Cancelled) => self.clipboard_status("Desktop paste cancelled"),
            Err(error) if self.tab.pending_edit.is_some() => {
                self.clipboard_status(format!("Desktop paste committed | {error}"));
            }
            Err(error) => self.clipboard_status(format!("Desktop paste failed | {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clipboard::{Backend, TestBackend},
        editor::tests::{contents, coordinator},
    };

    #[derive(Debug)]
    struct Gate {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl Gate {
        fn pair() -> (Self, Receiver<()>, SyncSender<()>) {
            let (entered, observed) = mpsc::sync_channel(1);
            let (resume, release) = mpsc::sync_channel(1);
            (
                Self {
                    entered,
                    release: Mutex::new(release),
                },
                observed,
                resume,
            )
        }

        fn hold(&self) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }

    #[derive(Debug, Default)]
    struct Desktop {
        bytes: Mutex<Vec<u8>>,
        reads: AtomicUsize,
        writes: AtomicUsize,
        failed: AtomicBool,
        write_gate: Option<Gate>,
        read_gate: Option<Gate>,
    }

    impl TestBackend for Desktop {
        fn write(&self, spool: &Spool, _cancel: &Cancellation) -> Result<(), &'static str> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            if self.failed.load(Ordering::Relaxed) {
                return Err("desktop clipboard write failed");
            }
            let mut bytes = self.bytes.lock().unwrap();
            bytes.clear();
            spool.reader().read_to_end(&mut bytes).unwrap();
            drop(bytes);
            if let Some(gate) = &self.write_gate {
                gate.hold();
            }
            Ok(())
        }

        fn read(&self, _cancel: &Cancellation) -> Result<Spool, &'static str> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if let Some(gate) = &self.read_gate {
                gate.hold();
            }
            let mut spool = Spool::new().unwrap();
            spool.append(&self.bytes.lock().unwrap()).unwrap();
            Ok(spool)
        }
    }

    fn access(desktop: &Arc<Desktop>) -> Access {
        Access {
            backend: Arc::new(Backend::Test(desktop.clone())),
            cancel: Cancellation::default(),
            active: Arc::default(),
        }
    }

    fn seeded(options: OpenOptions) -> Coordinator {
        let mut owner = coordinator(options);
        owner.action(Action::Insert("retained".into())).unwrap();
        owner.action(Action::Command(Command::SelectAll)).unwrap();
        owner.action(Action::Command(Command::Copy)).unwrap();
        owner
            .action(Action::Insert("selected 界\r\nsecond".into()))
            .unwrap();
        owner.action(Action::Command(Command::SelectAll)).unwrap();
        owner
    }

    fn assert_unchanged(owner: &mut Coordinator, target: Target) {
        assert_eq!(owner.clipboard_target(), Some(target));
        assert_eq!(contents(owner), "selected 界\r\nsecond");
        let mut retained = String::new();
        owner
            .clipboard
            .as_ref()
            .unwrap()
            .reader()
            .read_to_string(&mut retained)
            .unwrap();
        assert_eq!(retained, "retained");
    }

    /// A revocation after successful handoff preserves text, selection, and private copy.
    #[test]
    fn native_cut_revoked_after_desktop_handoff() {
        let (gate, entered, release) = Gate::pair();
        let desktop = Arc::new(Desktop {
            write_gate: Some(gate),
            ..Desktop::default()
        });
        let mut owner = seeded(OpenOptions::default());
        let target = owner.clipboard_target().unwrap();
        let access = access(&desktop);
        let cancel = access.cancel.clone();
        thread::scope(|scope| {
            scope.spawn(move || {
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                cancel.cancel();
                release.send(()).unwrap();
            });
            owner
                .action(Action::NativeCopy { cut: true, access })
                .unwrap();
        });
        assert_unchanged(&mut owner, target);
        assert_eq!(
            *desktop.bytes.lock().unwrap(),
            "selected 界\r\nsecond".as_bytes()
        );
        assert_eq!(owner.tab.screen.message, "Desktop copied | selection kept");
        assert_eq!(desktop.reads.load(Ordering::Relaxed), 0);
    }

    /// A superseded Copy cannot publish its staged selection to ede after handoff.
    #[test]
    fn superseded_native_copy_preserves_retained_data() {
        let (gate, entered, release) = Gate::pair();
        let desktop = Arc::new(Desktop {
            write_gate: Some(gate),
            ..Desktop::default()
        });
        let mut owner = seeded(OpenOptions::default());
        let target = owner.clipboard_target().unwrap();
        let access = access(&desktop);
        let cancel = access.cancel.clone();
        thread::scope(|scope| {
            scope.spawn(move || {
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                cancel.cancel();
                release.send(()).unwrap();
            });
            owner
                .action(Action::NativeCopy { cut: false, access })
                .unwrap();
        });
        assert_unchanged(&mut owner, target);
        assert_eq!(
            owner.tab.screen.message,
            "Desktop copied | ede copy cancelled"
        );
    }

    #[derive(Debug)]
    struct DeletionGate {
        active: AtomicBool,
        fail: bool,
        gate: Gate,
    }

    impl document::FaultInjector for DeletionGate {
        fn check(&self, point: document::IoPoint) -> std::io::Result<()> {
            if point == document::IoPoint::Write && self.active.swap(false, Ordering::AcqRel) {
                self.gate.hold();
                if self.fail {
                    return Err(std::io::Error::other("injected deletion failure"));
                }
            }
            Ok(())
        }
    }

    /// A successful handoff remains visible when storage refuses or loses authorization to delete.
    #[test]
    fn native_cut_failure_or_revocation_at_storage_commit() {
        for fail in [false, true] {
            let (gate, entered, release) = Gate::pair();
            let faults = Arc::new(DeletionGate {
                active: AtomicBool::new(false),
                fail,
                gate,
            });
            let options = OpenOptions {
                faults: Some(faults.clone()),
                ..OpenOptions::default()
            };
            let mut owner = seeded(options);
            let target = owner.clipboard_target().unwrap();
            let desktop = Arc::new(Desktop::default());
            let access = access(&desktop);
            let cancel = access.cancel.clone();
            faults.active.store(true, Ordering::Release);
            thread::scope(|scope| {
                scope.spawn(move || {
                    entered.recv_timeout(Duration::from_secs(5)).unwrap();
                    if !fail {
                        cancel.cancel();
                    }
                    release.send(()).unwrap();
                });
                owner
                    .action(Action::NativeCopy { cut: true, access })
                    .unwrap();
            });
            assert_unchanged(&mut owner, target);
            assert_eq!(owner.tab.screen.message, "Desktop copied | selection kept");
            assert_eq!(
                *desktop.bytes.lock().unwrap(),
                "selected 界\r\nsecond".as_bytes()
            );
            owner.action(Action::Command(Command::Undo)).unwrap();
            assert_eq!(contents(&mut owner), "retained");
        }
    }

    /// A failed write blocks a Paste waiting behind it. A later explicit Paste reads once.
    #[test]
    fn native_clipboard_orders_copy_before_paste_and_rejects_bad_text() {
        let mut owner = seeded(OpenOptions::default());
        let desktop = Arc::new(Desktop::default());
        desktop.failed.store(true, Ordering::Relaxed);
        owner.active_sequence = 1;
        owner
            .action(Action::NativeCopy {
                cut: false,
                access: access(&desktop),
            })
            .unwrap();
        let target = owner.clipboard_target().unwrap();
        assert_eq!(
            owner.tab.screen.message,
            "Copied internally | desktop clipboard write failed"
        );
        owner.active_sequence = 2;
        owner
            .action(Action::NativePaste {
                access: access(&desktop),
                observed: 0,
            })
            .unwrap();
        assert_eq!(desktop.reads.load(Ordering::Relaxed), 0);
        assert_eq!(owner.clipboard_target(), Some(target));

        for bytes in [&[0xff][..], b"with\0nul", b""] {
            *desktop.bytes.lock().unwrap() = bytes.to_vec();
            owner
                .action(Action::NativePaste {
                    access: access(&desktop),
                    observed: 1,
                })
                .unwrap();
            assert_eq!(owner.clipboard_target(), Some(target));
            assert_eq!(contents(&mut owner), "selected 界\r\nsecond");
            assert!(owner.tab.screen.message.starts_with("Desktop "));
        }

        *desktop.bytes.lock().unwrap() = "external 界\r\nmarker\n".as_bytes().to_vec();
        owner
            .action(Action::NativePaste {
                access: access(&desktop),
                observed: 1,
            })
            .unwrap();
        assert_eq!(contents(&mut owner), "external 界\r\nmarker\n");
        assert_eq!(desktop.reads.load(Ordering::Relaxed), 4);
        owner.action(Action::Command(Command::Undo)).unwrap();
        assert_eq!(owner.tab.screen.position, target.position);
        assert_eq!(contents(&mut owner), "selected 界\r\nsecond");
    }

    /// A read that was pending while focus or selection changed must never replace text.
    #[test]
    fn native_paste_revoked_while_reading() {
        let (gate, entered, release) = Gate::pair();
        let desktop = Arc::new(Desktop {
            bytes: Mutex::new(b"unwanted".to_vec()),
            read_gate: Some(gate),
            ..Desktop::default()
        });
        let mut owner = seeded(OpenOptions::default());
        let target = owner.clipboard_target().unwrap();
        let access = access(&desktop);
        let cancel = access.cancel.clone();
        thread::scope(|scope| {
            scope.spawn(move || {
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                cancel.cancel();
                release.send(()).unwrap();
            });
            owner
                .action(Action::NativePaste {
                    access,
                    observed: 0,
                })
                .unwrap();
        });
        assert_unchanged(&mut owner, target);
        assert_eq!(owner.tab.screen.message, "Desktop paste cancelled");
        assert_eq!(desktop.writes.load(Ordering::Relaxed), 0);
    }
}
