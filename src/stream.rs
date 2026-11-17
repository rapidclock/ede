//! Independent source owners and coalesced publication of immutable retained text.

mod decoder;
mod follow;

use crate::document::{Cancellation, Capture, CaptureAdoption, CaptureFactory, Error, Snapshot};
use decoder::{BLOCK, Decoder, PREFIX, within};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    poll::{PollFd, PollFlags, poll},
};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsFd,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

/// An inherited descriptor is separate from the only terminal decoder.
pub(crate) enum Source {
    Stdin(File),
    Follow(PathBuf),
}

#[derive(Default)]
pub(crate) struct Startup {
    pub paths: Vec<PathBuf>,
    pub source: Option<Source>,
}

/// An observation that can supersede earlier progress without losing terminal status.
pub(crate) enum Phase {
    Reading(Option<String>),
    Waiting(Option<String>),
    Finishing,
    Finished(CaptureAdoption),
    FinishFailed(String),
    Failed(String),
    OpenFailed(String),
}

#[derive(Default)]
pub(crate) struct Update {
    pub snapshot: Option<Snapshot>,
    pub phase: Option<Phase>,
    pub boundary: Option<(u64, &'static str)>,
    pub protected_path: Option<PathBuf>,
}

pub(crate) struct Reader {
    cancel: Cancellation,
    retry: SyncSender<()>,
    latest: Arc<Mutex<Update>>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Only actual source owners and in-flight cancelled work consume a reader slot.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Reader {
    pub(crate) fn spawn(
        source: Source,
        captures: CaptureFactory,
        slots: &Arc<AtomicUsize>,
    ) -> Result<Self, Error> {
        slots
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < 4).then_some(used + 1)
            })
            .map_err(|_| Error::ResourceLimit("four live input sources"))?;
        let slot = Slot(slots.clone());
        let cancel = Cancellation::default();
        let source_cancel = cancel.clone();
        let latest = Arc::new(Mutex::new(Update::default()));
        let output = latest.clone();
        let (retry, retries) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("ede-capture".into())
            .spawn(move || {
                let result = capture(source, captures, &source_cancel, &output);
                drop(slot);
                match result {
                    Ok(Some(capture)) => finish(capture, &source_cancel, &output, retries),
                    Ok(None) => {}
                    Err(error) => update(&output, |out| {
                        out.phase = Some(Phase::OpenFailed(error.to_string()))
                    }),
                }
            })?;
        Ok(Self {
            cancel,
            retry,
            latest,
        })
    }

    /// During a Find hold, leave the last coalesced extent in the mailbox.
    pub(crate) fn latest(&self) -> Update {
        std::mem::take(&mut *self.latest.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub(crate) fn retry(&self) {
        let _ = self.retry.try_send(());
    }
}

type Block = Box<[u8]>;

struct Pipe {
    buffers: SyncSender<(Block, usize)>,
    blocks: Receiver<io::Result<(Block, usize)>>,
    cancel: Cancellation,
    worker: Option<thread::JoinHandle<()>>,
}

/// Keep readiness races with other FIFO readers cancellable. Restore inherited flags
/// before relinquishing our descriptor because the open description may be shared.
struct Nonblocking {
    file: File,
    restore: bool,
}

impl Nonblocking {
    fn new(file: File) -> io::Result<Self> {
        let flags = OFlag::from_bits_retain(fcntl(&file, FcntlArg::F_GETFL)?);
        let restore = !flags.contains(OFlag::O_NONBLOCK);
        if restore {
            fcntl(&file, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        }
        Ok(Self { file, restore })
    }
}

impl Drop for Nonblocking {
    fn drop(&mut self) {
        if self.restore
            && let Ok(flags) = fcntl(&self.file, FcntlArg::F_GETFL)
        {
            let _ = fcntl(
                &self.file,
                FcntlArg::F_SETFL(OFlag::from_bits_retain(flags) - OFlag::O_NONBLOCK),
            );
        }
    }
}

impl Pipe {
    fn start(stdin: File, cancel: &Cancellation) -> io::Result<Self> {
        let stdin = Nonblocking::new(stdin)?;
        let (buffers, available) = mpsc::sync_channel::<(Block, usize)>(1);
        let (sent, blocks) = mpsc::sync_channel(1);
        let wake = cancel.clone();
        let worker = thread::Builder::new()
            .name("ede-source".into())
            .spawn(move || {
                let mut stdin = Some(stdin);
                while !wake.is_cancelled() {
                    match available.recv_timeout(Duration::from_millis(20)) {
                        Ok((mut block, capacity)) => {
                            let result = ready_read(
                                &mut stdin.as_mut().expect("open source").file,
                                &mut block[PREFIX..PREFIX + capacity],
                                &wake,
                            );
                            let done = !matches!(result, Ok(n) if n > 0);
                            if done {
                                drop(stdin.take());
                            }
                            if sent.send(result.map(|count| (block, count))).is_err() || done {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            buffers,
            blocks,
            cancel: cancel.clone(),
            worker: Some(worker),
        })
    }

    fn read(&self, block: Block, capacity: usize) -> Result<(Block, usize), Error> {
        self.buffers
            .send((block, capacity))
            .map_err(|_| Error::Cancelled)?;
        self.blocks
            .recv()
            .map_err(|_| Error::Cancelled)?
            .map_err(Error::from)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // A source worker may be delayed in storage, but its independent reader
        // observes the same flag while waiting for space, readiness, or a buffer.
        if let Some(worker) = self.worker.take() {
            self.cancel.cancel();
            let _ = worker.join();
        }
    }
}

fn ready_read(file: &mut File, buffer: &mut [u8], cancel: &Cancellation) -> io::Result<usize> {
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "input capture cancelled",
            ));
        }
        let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, 20u16) {
            Ok(0) | Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        match file.read(buffer) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            result => return result,
        }
    }
}

enum Input {
    Pipe(Pipe),
    Follower(Box<follow::Follower>),
}

fn capture(
    source: Source,
    factory: CaptureFactory,
    cancel: &Cancellation,
    latest: &Mutex<Update>,
) -> Result<Option<Capture>, Error> {
    let mut source = match source {
        Source::Stdin(stdin) => Input::Pipe(Pipe::start(stdin, cancel)?),
        Source::Follow(path) => Input::Follower(Box::new(follow::Follower::open(path)?)),
    };
    let mut data = factory.open()?;
    update(latest, |out| {
        out.phase = Some(Phase::Reading(None));
        if let Input::Follower(follower) = &source {
            out.protected_path = Some(follower.resolved().to_owned());
        }
    });
    let initial = data.snapshot()?;
    update(latest, |out| out.snapshot = Some(initial));
    let mut decoder = Decoder::default();
    let mut block = vec![0; PREFIX + BLOCK].into_boxed_slice();
    let mut generations = 1;
    let mut seam = false;
    let mut last = None;
    let mut reading = true;
    let mut warning = None;
    let result = (|| {
        loop {
            cancel.check()?;
            let capacity = if data.at_capacity() {
                decoder.lookahead()
            } else {
                BLOCK
            };
            let (count, boundary) = match &mut source {
                Input::Pipe(pipe) => {
                    let (filled, count) = pipe.read(block, capacity)?;
                    block = filled;
                    (count, None)
                }
                Input::Follower(follower) => {
                    match follower.read(&mut block[PREFIX..PREFIX + capacity])? {
                        follow::Read::Idle(next) => {
                            if reading || warning != next {
                                warning = next.clone();
                                update(latest, |out| out.phase = Some(Phase::Waiting(next)));
                            }
                            reading = false;
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        follow::Read::Seam(reason) => (0, Some(reason)),
                        follow::Read::Bytes(count, next) => {
                            if !reading || warning != next {
                                warning = next.clone();
                                update(latest, |out| out.phase = Some(Phase::Reading(next)));
                            }
                            reading = true;
                            (count, None)
                        }
                    }
                }
            };
            let eof = count == 0;
            let decoded = decoder.decode(&mut block, count, eof);
            let mut span = decoded.bytes;
            let bom = generations == 1 && decoder.bom && data.retained() == 0;
            let mut separator = 0;
            if bom {
                span.start -= 3;
            }
            if seam && !span.is_empty() {
                separator = match last {
                    None | Some(b'\n') => 0,
                    Some(b'\r') => 2,
                    Some(_) => 1,
                };
                span.start -= separator;
                if separator > 0 {
                    let end = span.start + separator;
                    block[span.start..end].copy_from_slice(if separator == 2 {
                        b"\r\n"
                    } else {
                        b"\n"
                    });
                }
            }
            let bytes = &block[span];
            if !bytes.is_empty() {
                let prefix = usize::from(bom) * 3 + separator;
                let retained = data.append(
                    bytes,
                    bom,
                    |available| {
                        if available < prefix {
                            return 0;
                        }
                        let text = within(&bytes[prefix..], available - prefix);
                        if separator > 0 && text == 0 {
                            0
                        } else {
                            prefix + text
                        }
                    },
                    cancel,
                )?;
                if retained > 0 {
                    if retained > usize::from(bom) * 3 {
                        last = Some(bytes[retained - 1]);
                    }
                    let snapshot = data.snapshot()?;
                    update(latest, |out| out.snapshot = Some(snapshot));
                }
                if retained < bytes.len() {
                    return Err(Error::ResourceLimit("stream retention quota"));
                }
                seam = false;
            }
            if let Some(reason) = decoded.error {
                return Err(io::Error::new(io::ErrorKind::InvalidData, reason).into());
            }
            if decoder.has_partial_text() && data.at_capacity() {
                return Err(Error::ResourceLimit("stream retention quota"));
            }
            if let Some(reason) = boundary {
                generations += 1;
                // A succession of empty generations must not consume a separator.
                seam = true;
                decoder = Decoder::default();
                update(latest, |out| {
                    out.boundary = Some((generations, reason));
                    if let Input::Follower(follower) = &source {
                        out.protected_path = Some(follower.resolved().to_owned());
                    }
                });
            } else if eof {
                return Ok(());
            }
        }
    })();
    match result {
        Ok(()) => {
            update(latest, |out| out.phase = Some(Phase::Finishing));
            // The pipe owner drops its descriptor before sending the EOF block.
            if let Input::Pipe(mut pipe) = source {
                if let Some(worker) = pipe.worker.take() {
                    let _ = worker.join();
                }
                Ok(Some(data))
            } else {
                Ok(None)
            }
        }
        Err(error) => {
            update(latest, |out| {
                out.phase = Some(Phase::Failed(error.to_string()))
            });
            Ok(None)
        }
    }
}

fn finish(
    mut capture: Capture,
    cancel: &Cancellation,
    latest: &Mutex<Update>,
    retries: Receiver<()>,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match capture.finish(cancel) {
            Ok(document) => {
                update(latest, |out| {
                    out.phase = Some(Phase::Finished(CaptureAdoption::new(document)))
                });
                break;
            }
            Err((stored, error)) => {
                capture = *stored;
                update(latest, |out| {
                    out.phase = Some(Phase::FinishFailed(error.to_string()))
                });
            }
        }
        while !cancel.is_cancelled() {
            match retries.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
        }
    }
}

fn update(latest: &Mutex<Update>, change: impl FnOnce(&mut Update)) {
    change(&mut latest.lock().unwrap_or_else(|e| e.into_inner()));
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{OpenOptions, StorageService};
    use std::io::Write;
    use std::time::Instant;

    #[test]
    fn invalid_stream_exports_only_the_committed_serialized_prefix() {
        use crate::document::SaveOutcome;
        let service = StorageService::new(OpenOptions::default()).unwrap();
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut writer = File::from(write);
        let reader = Reader::spawn(
            Source::Stdin(File::from(read)),
            service.captures(),
            &Arc::new(0.into()),
        )
        .unwrap();
        writer.write_all(b"\xef\xbb\xbflead\r\n\0\xff").unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let mut retained = None;
        loop {
            let update = reader.latest();
            retained = update.snapshot.or(retained);
            if let Some(Phase::Failed(reason)) = update.phase {
                assert!(reason.contains("NUL at byte 9"), "{reason}");
                break;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
        let snapshot = retained.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("retained.txt");
        let cancel = Cancellation::default();
        let report = snapshot.export_retained(destination.clone(), [None, None], &cancel);
        assert_eq!(report.outcome, SaveOutcome::Saved);
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"\xef\xbb\xbflead\r\n"
        );
        let report = snapshot.export_retained(destination.clone(), [None, None], &cancel);
        assert_eq!(report.outcome, SaveOutcome::NotCommitted);
        assert_eq!(std::fs::read(destination).unwrap(), b"\xef\xbb\xbflead\r\n");
    }

    #[test]
    fn exact_cap_can_finish_on_eof_and_retry_failed_adoption() {
        use crate::document::{FaultInjector, IoPoint};
        use std::sync::atomic::AtomicBool;

        #[derive(Debug, Default)]
        struct FailWrite(AtomicBool);
        impl FaultInjector for FailWrite {
            fn check(&self, point: IoPoint) -> io::Result<()> {
                if point == IoPoint::Write && self.0.swap(false, Ordering::AcqRel) {
                    return Err(io::Error::other("injected finish failure"));
                }
                Ok(())
            }
        }

        let fault = Arc::new(FailWrite::default());
        let service = StorageService::new(OpenOptions {
            faults: Some(fault.clone()),
            ..OpenOptions::default()
        })
        .unwrap();
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut writer = File::from(write);
        let reader = Reader::spawn(
            Source::Stdin(File::from(read)),
            service.captures().with_limits(4, 4),
            &Arc::new(0.into()),
        )
        .unwrap();
        writer.write_all(b"abcd").unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            if let Some(snapshot) = reader
                .latest()
                .snapshot
                .filter(|snapshot| snapshot.len() == 4)
            {
                break snapshot;
            }
            assert!(
                Instant::now() < end,
                "capture never reached the exact quota"
            );
            thread::sleep(Duration::from_millis(2));
        };
        fault.0.store(true, Ordering::Release);
        drop(writer);
        while !matches!(reader.latest().phase, Some(Phase::FinishFailed(_))) {
            assert!(Instant::now() < end, "clean EOF did not attempt adoption");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            snapshot
                .read_window(crate::document::ByteOffset(0), 4)
                .unwrap()
                .text(),
            "abcd"
        );
        reader.retry();
        while !matches!(reader.latest().phase, Some(Phase::Finished(_))) {
            assert!(
                Instant::now() < end,
                "retry never prepared the same capture"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn exact_cap_detects_extra_cr_without_waiting_for_eof() {
        let (r, w) = nix::unistd::pipe().unwrap();
        let mut writer = File::from(w);
        let service = StorageService::new(OpenOptions::default()).unwrap();
        let factory = service.captures().with_limits(4, 16);
        let reader =
            Reader::spawn(Source::Stdin(File::from(r)), factory, &Arc::new(0.into())).unwrap();
        writer.write_all(b"abcd").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if reader.latest().snapshot.is_some_and(|s| s.len() == 4) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "initial capture did not reach exact cap"
            );
            thread::sleep(Duration::from_millis(2));
        }
        writer.write_all(b"\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if matches!(reader.latest().phase, Some(Phase::Failed(_))) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "proved overflow by one CR, but still waiting for the idle producer"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::document::{FaultInjector, IoPoint, OpenOptions, StorageService};
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    #[derive(Debug)]
    struct WriteGate {
        arm: AtomicBool,
        hit: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl FaultInjector for WriteGate {
        fn check(&self, point: IoPoint) -> io::Result<()> {
            if point == IoPoint::Write && self.arm.swap(false, Ordering::AcqRel) {
                self.hit.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn close_releases_the_pipe_during_a_blocked_spool_write() {
        let (read_end, write_end) = nix::unistd::pipe().unwrap();
        let (hit, blocked) = mpsc::sync_channel(1);
        let (release, proceed) = mpsc::sync_channel(1);
        let gate = Arc::new(WriteGate {
            arm: AtomicBool::new(false),
            hit,
            release: Mutex::new(proceed),
        });
        let service = StorageService::new(OpenOptions {
            faults: Some(gate.clone()),
            ..OpenOptions::default()
        })
        .unwrap();
        let slots = Arc::new(0.into());
        let reader = Reader::spawn(
            Source::Stdin(File::from(read_end)),
            service.captures(),
            &slots,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while reader.latest().snapshot.is_none() {
            assert!(Instant::now() < deadline, "capture never opened");
            thread::sleep(Duration::from_millis(2));
        }
        gate.arm.store(true, Ordering::Release);
        nix::unistd::write(&write_end, b"gate me").unwrap();
        blocked.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(reader);

        let deadline = Instant::now() + Duration::from_millis(600);
        while nix::unistd::write(&write_end, b"x") != Err(Errno::EPIPE) {
            assert!(
                Instant::now() < deadline,
                "pipe reader remained open while the spool was blocked"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            slots.load(Ordering::Acquire),
            1,
            "cancelled work still occupies its slot"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while slots.load(Ordering::Acquire) != 0 {
            assert!(
                Instant::now() < deadline,
                "cancelled capture did not release its slot"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}
