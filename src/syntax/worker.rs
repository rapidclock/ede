//! Subprocess supervision, cancellable bounded pipe I/O and immutable snapshot transfer.

use std::{
    io::{self, Read, Write},
    os::fd::{AsFd, AsRawFd},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::document::{Cancellation, Permit, Snapshot};

use super::{
    Config, Fallback, Language, ViewKey,
    config::{self, Rules},
    limits,
    projection::{self, GlyphRun, Projection},
    wire,
};

const STARTUP: Duration = Duration::from_secs(2);
const TRANSFER: Duration = Duration::from_secs(5);
/// Worst-case descriptors and reply plus bounded transport scratch.
pub(crate) const WORK_CHARGE: usize = 396 * 1024;

/// Coordinates the single worker process and the terminal's job-control path.
#[derive(Default)]
pub(crate) struct ProcessControl {
    program: Option<PathBuf>,
    state: Mutex<ProcessState>,
    // Low bit is suspension; the remaining bits identify pre-suspend work.
    phase: AtomicU64,
}

#[derive(Default)]
struct ProcessState {
    serial: u64,
    child: Option<(u64, Child)>,
}

impl ProcessControl {
    /// Only the CLI names an executable that implements the private protocol.
    pub(crate) fn new(program: Option<PathBuf>) -> Self {
        Self {
            program,
            ..Self::default()
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.program.is_some()
    }

    /// Terminate and reap the child before stopping the interactive process.
    pub(crate) fn pause(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let phase = self.phase.load(Ordering::Relaxed).wrapping_add(2) | 1;
        self.phase.store(phase, Ordering::Release);
        if let Some((_, mut child)) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        phase >> 1
    }

    /// Admit fresh work after terminal modes have been restored.
    pub(crate) fn resume(&self) {
        let _state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.phase.fetch_and(!1, Ordering::Release);
    }

    /// Paused sessions are intentionally idle.
    pub(crate) fn paused(&self) -> bool {
        self.phase.load(Ordering::Acquire) & 1 != 0
    }

    /// Distinguish pre-suspend replies even if the terminal resumed between polls.
    pub(crate) fn epoch(&self) -> u64 {
        self.phase.load(Ordering::Acquire) >> 1
    }

    /// Spawn within the lifecycle lock so a simultaneous suspend cannot miss the child.
    fn spawn(self: &Arc<Self>, cancel: &Cancellation) -> Result<Session, Fallback> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.paused() || cancel.is_cancelled() {
            return Err(Fallback::Cancelled);
        }
        if state.child.is_some() {
            return Err(Fallback::Busy);
        }
        let program = self.program.as_ref().ok_or(Fallback::Worker)?;
        let mut command = Command::new(program);
        command.arg("--ede-syntax-worker");
        Self::attach(self, &mut state, command)
    }

    fn attach(
        self: &Arc<Self>,
        state: &mut ProcessState,
        mut command: Command,
    ) -> Result<Session, Fallback> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Fallback::Worker)?;
        let Some((input, output)) = child.stdin.take().zip(child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Fallback::Worker);
        };
        if nonblocking(&input)
            .and_then(|()| nonblocking(&output))
            .is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Fallback::Worker);
        }
        state.serial = state.serial.wrapping_add(1);
        let serial = state.serial;
        state.child = Some((serial, child));
        Ok(Session {
            control: self.clone(),
            serial,
            epoch: self.epoch(),
            projection: 0,
            input,
            output,
        })
    }

    /// Read the worker's status while keeping registration synchronized with suspension.
    fn status(&self, serial: u64) -> Option<std::process::ExitStatus> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .as_mut()
            .filter(|(id, _)| *id == serial)
            .and_then(|(_, child)| child.try_wait().ok().flatten())
    }

    /// Kill and reap exactly this registered child, tolerating earlier suspension.
    fn finish(&self, serial: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.child.as_ref().is_some_and(|(id, _)| *id == serial)
            && let Some((_, mut child)) = state.child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One parked worker retaining the active revision's trees. Never retain a View or Snapshot here.
pub(crate) struct Session {
    control: Arc<ProcessControl>,
    serial: u64,
    epoch: u64,
    projection: u32,
    input: ChildStdin,
    output: ChildStdout,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.control.finish(self.serial);
    }
}

impl Session {
    /// Create a worker and wait for its private startup marker on a background thread.
    fn ready(control: &Arc<ProcessControl>, cancel: &Cancellation) -> Result<Self, Fallback> {
        let mut process = control.spawn(cancel)?;
        let budget = Budget::new(cancel, STARTUP, Fallback::Startup);
        let response = wire::byte(&mut budget.io(&mut process.output));
        if !matches!(response, Ok(wire::READY)) {
            return Err(process.failure(&budget, response.err()));
        }
        Ok(process)
    }

    /// Categorize I/O errors without exposing internal transport diagnostics in the terminal.
    fn failure(&self, budget: &Budget<'_>, error: Option<io::Error>) -> Fallback {
        if budget.cancel.is_cancelled() || self.control.epoch() != self.epoch {
            Fallback::Cancelled
        } else if error
            .as_ref()
            .is_some_and(|error| error.kind() == io::ErrorKind::InvalidData)
        {
            Fallback::Output
        } else if Instant::now() >= budget.until {
            budget.timeout
        } else if self
            .control
            .status(self.serial)
            .is_some_and(|status| status.code() == Some(73))
        {
            Fallback::Memory
        } else {
            Fallback::Worker
        }
    }

    /// Send the entire logical revision while still in the preparation phase.
    fn open(
        &mut self,
        snapshot: Snapshot,
        language: &Language,
        rules: &Rules,
        cancel: &Cancellation,
    ) -> Result<(), Fallback> {
        let budget = Budget::new(cancel, TRANSFER, Fallback::Transfer);
        let mut output = budget.io(&mut self.input);
        let header = (|| {
            output.write_all(&[wire::OPEN, wire::language_code(Some(language))])?;
            wire::send_rules(&mut output, rules)?;
            wire::send_number(&mut output, snapshot.len() as u32)
        })();
        if let Err(error) = header {
            return Err(self.failure(&budget, Some(error)));
        }
        let transfer = limits::transfer(&snapshot, cancel, &mut output);
        drop(snapshot);
        match transfer {
            Err(Fallback::Worker) => return Err(self.failure(&budget, None)),
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let response = wire::byte(&mut budget.io(&mut self.output));
        if matches!(response, Ok(wire::READY)) {
            Ok(())
        } else {
            Err(self.failure(&budget, response.err()))
        }
    }

    /// Project a viewport in one independently supervised analysis deadline.
    fn project(
        &mut self,
        runs: &[GlyphRun],
        cancel: &Cancellation,
    ) -> Result<Vec<projection::Styled>, Fallback> {
        self.projection = self.projection.checked_add(1).ok_or(Fallback::Output)?;
        let budget = Budget::new(cancel, wire::ANALYSIS, Fallback::Timeout);
        let mut output = budget.io(&mut self.input);
        let dispatch = output
            .write_all(&[wire::PROJECT])
            .and_then(|()| wire::send_number(&mut output, self.projection))
            .and_then(|()| wire::send_runs(&mut output, runs));
        if let Err(error) = dispatch {
            return Err(self.failure(&budget, Some(error)));
        }
        let mut input = budget.io(&mut self.output);
        match wire::byte(&mut input) {
            Ok(wire::DONE) => {
                let result = wire::read_projection(&mut input, self.projection)
                    .map_err(|error| self.failure(&budget, Some(error)))?;
                if Instant::now() >= budget.until {
                    return Err(Fallback::Timeout);
                }
                let (spans, _peak, _native_calls) = result;
                projection::valid(runs, &spans)
                    .then_some(spans)
                    .ok_or(Fallback::Output)
            }
            Ok(wire::TIMEOUT) => Err(Fallback::Timeout),
            Ok(wire::BUDGET) => Err(Fallback::Output),
            Ok(wire::INVALID) => Err(Fallback::Worker),
            Ok(_) => Err(Fallback::Output),
            Err(error) => Err(self.failure(&budget, Some(error))),
        }
    }
}

/// The worker owns these values during transfer and projection, with one bounded release point.
pub(crate) struct Work {
    pub(crate) key: ViewKey,
    pub(crate) language: &'static Language,
    pub(crate) config: Arc<Config>,
    pub(crate) cancel: Cancellation,
    pub(crate) control: Arc<ProcessControl>,
    pub(crate) snapshot: Option<Snapshot>,
    pub(crate) session: Option<Session>,
    pub(crate) permit: Permit,
}

/// A matching projection can be published while the returned session serves later geometry.
pub(crate) type WorkerResult = Result<(Session, Arc<Projection>), Fallback>;

impl Work {
    /// Build bounded projections from compact byte descriptors prepared before dispatch.
    pub(crate) fn descriptors(view: &crate::text::View) -> Option<Vec<GlyphRun>> {
        GlyphRun::collect(view)
    }

    /// Run on the supervisor thread, never on the coordinator or terminal.
    pub(crate) fn run(mut self, runs: Vec<GlyphRun>) -> WorkerResult {
        let mut session = match self.session {
            Some(session) => session,
            None => Session::ready(&self.control, &self.cancel)?,
        };
        if let Some(snapshot) = self.snapshot {
            session.open(snapshot, self.language, &self.config.rules, &self.cancel)?;
        }
        let ranges = session.project(&runs, &self.cancel)?;
        drop(runs);
        self.permit
            .shrink(ranges.capacity() * std::mem::size_of::<projection::Styled>() + 2048);
        Ok((
            session,
            Arc::new(Projection {
                key: self.key,
                ranges,
                _permit: self.permit,
            }),
        ))
    }
}

/// Load candidate source files off both owner loops.
pub(crate) fn read_config() -> Result<Config, String> {
    Config::load(config::directory().as_deref()).map_err(|error| short_notice(&error.to_string()))
}

fn short_notice(message: &str) -> String {
    message.chars().take(320).collect()
}

/// Compile user queries in a worker instance with the same containment as ordinary parsing.
pub(crate) fn validate_config(
    config: Arc<Config>,
    control: Arc<ProcessControl>,
    cancel: Cancellation,
) -> Result<Arc<Config>, String> {
    if config.rules.queries.is_empty() {
        return Ok(config);
    }
    let validate = |process: &mut Session| -> Result<_, Fallback> {
        let transfer = Budget::new(&cancel, TRANSFER, Fallback::Transfer);
        let mut output = transfer.io(&mut process.input);
        let result = output
            .write_all(&[wire::VALIDATE])
            .and_then(|()| wire::send_rules(&mut output, &config.rules));
        result.map_err(|error| process.failure(&transfer, Some(error)))?;
        let response = wire::byte(&mut transfer.io(&mut process.output));
        if !matches!(response, Ok(wire::READY)) {
            return Err(process.failure(&transfer, response.err()));
        }
        let analysis = Budget::new(&cancel, wire::ANALYSIS, Fallback::Timeout);
        wire::send(&mut analysis.io(&mut process.input), wire::VALIDATE)
            .map_err(|error| process.failure(&analysis, Some(error)))?;
        let answer = wire::byte(&mut analysis.io(&mut process.output));
        match answer {
            Ok(wire::DONE) if Instant::now() < analysis.until => Ok(None),
            Ok(wire::DONE) => Err(Fallback::Timeout),
            Ok(wire::INVALID) => wire::read_error(&mut analysis.io(&mut process.output))
                .map(Some)
                .map_err(|error| process.failure(&analysis, Some(error))),
            Ok(_) => Err(Fallback::Output),
            Err(error) => Err(process.failure(&analysis, Some(error))),
        }
    };
    let mut process =
        Session::ready(&control, &cancel).map_err(|reason| reason.message().to_owned())?;
    match validate(&mut process) {
        Ok(None) => Ok(config),
        Ok(Some((id, reason))) => Err(short_notice(&format!(
            "{}: {reason}",
            config::query_path(id).display()
        ))),
        Err(reason) => {
            let files = config
                .rules
                .queries
                .keys()
                .map(|id| config::query_path(id).display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(short_notice(&format!("{} ({files})", reason.message())))
        }
    }
}

fn nonblocking(fd: &impl AsFd) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = fcntl(fd, FcntlArg::F_GETFL)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_retain(flags) | OFlag::O_NONBLOCK),
    )?;
    Ok(())
}

/// A wall-clock deadline enforced even when the native child never checks its callbacks.
struct Budget<'a> {
    cancel: &'a Cancellation,
    until: Instant,
    timeout: Fallback,
}

impl<'a> Budget<'a> {
    fn new(cancel: &'a Cancellation, duration: Duration, timeout: Fallback) -> Self {
        Self {
            cancel,
            until: Instant::now() + duration,
            timeout,
        }
    }

    fn io<'b, T>(&'b self, inner: &'b mut T) -> Timed<'b, 'a, T> {
        Timed {
            budget: self,
            inner,
        }
    }

    fn check(&self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            Err(io::Error::other("syntax operation cancelled"))
        } else if Instant::now() >= self.until {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "syntax operation timed out",
            ))
        } else {
            Ok(())
        }
    }
}

struct Timed<'a, 'b, T> {
    budget: &'a Budget<'b>,
    inner: &'a mut T,
}

impl<T> Timed<'_, '_, T> {
    fn transfer(
        &mut self,
        mut operation: impl FnMut(&mut T) -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            self.budget.check()?;
            match operation(self.inner) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl<T: Read + AsRawFd> Read for Timed<'_, '_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.transfer(|input| input.read(bytes))
    }
}

impl<T: Write + AsRawFd> Write for Timed<'_, '_, T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.transfer(|output| output.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.budget.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real children that emit no reply must be killed and reaped even if they
    /// have no cooperative parser callback or normal exit path.
    #[test]
    fn noncooperating_child_is_reaped_at_deadline_and_suspend() {
        let control = Arc::new(ProcessControl::default());
        let make_child = || {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "printf R; while :; do :; done"]);
            let mut session = control
                .attach(&mut control.state.lock().unwrap(), command)
                .unwrap();
            let cancel = Cancellation::default();
            let ready = Budget::new(&cancel, STARTUP, Fallback::Startup);
            assert_eq!(
                wire::byte(&mut ready.io(&mut session.output)).unwrap(),
                wire::READY
            );
            session
        };
        let mut process = make_child();
        let start = Instant::now();
        let cancel = Cancellation::default();
        assert_eq!(
            process.project(&[], &cancel).unwrap_err(),
            Fallback::Timeout
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let id = process.serial;
        drop(process);
        assert!(control.status(id).is_none());
        assert!(control.state.lock().unwrap().child.is_none());

        let mut process = make_child();
        let epoch = control.pause();
        assert!(control.state.lock().unwrap().child.is_none());
        control.resume();
        assert_eq!(control.epoch(), epoch);
        assert_eq!(
            process.project(&[], &cancel).unwrap_err(),
            Fallback::Cancelled
        );
    }
}
